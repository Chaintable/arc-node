#!/usr/bin/env python3
"""Convert Arc writer captures into pipeline-compatible semantic fixtures."""

from __future__ import annotations

import argparse
import copy
import gzip
import hashlib
import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import Any, Callable


STATE_DIFF_BITMAP_BYTES = 125
STATE_DIFF_OFFSET_COUNT = 1_001
STATE_DIFF_INDEX_BYTES = STATE_DIFF_BITMAP_BYTES + STATE_DIFF_OFFSET_COUNT * 8
WRITER_PRODUCER_COMMIT = "1ff564adb44a54d5ddd7a31e60a4f64498650801"
FORMAT_REFERENCE_RELEASE = "v0.1.43"
FORMAT_REFERENCE_COMMIT = "7c4e096bfbc132dcb79312e2371c80919b966a52"
FORMAT_REFERENCE_SOURCES = {
    "bin/background-tracer/src/utils/codec.rs": "0fb07bbacddc59cbec352634472895a622fbec65",
    "bin/background-tracer/src/upload/s3.rs": "597fa9ee6de41183f46c1e8b88bb50407094f0a7",
    "types/src/debank.rs": "2926b245378d4b3f54b5584ec36ac7204bca112f",
}
EXPECTED_LABELS = {
    0: "genesis",
    1: "empty-hooks",
    2: "native-transfer",
    3: "create2",
    4: "failed-create",
}
CAPTURE_CONTEXT = "capture-context.json"
EXPECTED_CAPTURE_ENTRIES = frozenset(
    {CAPTURE_CONTEXT}
    | {f"{number}-{label}.json" for number, label in EXPECTED_LABELS.items()}
)
EXPORTER_ENTRYPOINT = Path("scripts/fixtures/build_leafage_a1b_fixtures.py")
ALLOWED_EXPORTER_CHANGES = frozenset(
    {
        "crates/execution-e2e/src/setup.rs",
        "crates/execution-e2e/tests/export_leafage_a1b.rs",
        EXPORTER_ENTRYPOINT.as_posix(),
        "scripts/generate-leafage-a1b-fixtures.sh",
    }
)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--capture-dir", type=Path, required=True)
    parser.add_argument("--verification-capture-dir", type=Path, required=True)
    parser.add_argument("--output-dir", type=Path, required=True)
    parser.add_argument("--exporter-worktree", type=Path, required=True)
    parser.add_argument("--expected-exporter-commit", required=True)
    parser.add_argument("--format-reference-worktree", type=Path, required=True)
    return parser.parse_args()


def output_preflight_cli(arguments: list[str]) -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--output-dir", type=Path, required=True)
    parser.add_argument("--exporter-worktree", type=Path, required=True)
    args = parser.parse_args(arguments)
    resolve_final_output_path(
        args.output_dir, args.exporter_worktree.resolve(strict=True)
    )


def git(worktree: Path, *args: str) -> str:
    return subprocess.check_output(
        ["git", "-C", str(worktree), *args], text=True
    ).strip()


def write_new(path: Path, data: bytes) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("xb") as output:
        output.write(data)


def json_bytes(
    value: Any, *, pretty: bool = False, trailing_newline: bool = True
) -> bytes:
    if pretty:
        encoded = json.dumps(value, indent=2, sort_keys=True, ensure_ascii=False)
    else:
        encoded = json.dumps(value, separators=(",", ":"), ensure_ascii=False)
    if trailing_newline:
        encoded += "\n"
    return encoded.encode()


def gzip_json(value: Any) -> bytes:
    # background-tracer compresses serde_json::to_vec(value), which has no
    # trailing newline. mtime=0 makes the fixture container reproducible; its
    # decoded JSON semantics, not a particular gzip header, are the contract.
    return gzip.compress(
        json_bytes(value, trailing_newline=False), compresslevel=6, mtime=0
    )


def decode_quantity(value: str | int) -> int:
    return value if isinstance(value, int) else int(value, 0)


def relative(path: Path, root: Path) -> str:
    return path.relative_to(root).as_posix()


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def normalized_capture(output: dict[str, Any]) -> bytes:
    normalized = copy.deepcopy(output)
    normalized["block_file"]["block"]["process_start_timestamp"] = 0
    return (
        json.dumps(
            normalized, separators=(",", ":"), sort_keys=True, ensure_ascii=False
        )
        + "\n"
    ).encode()


def load_captures(capture_dir: Path) -> list[tuple[str, dict[str, Any]]]:
    captures = []
    for number, label in EXPECTED_LABELS.items():
        path = capture_dir / f"{number}-{label}.json"
        captures.append((label, json.loads(path.read_text())))
    return captures


def validate_capture_directory(capture_dir: Path) -> None:
    if capture_dir.is_symlink() or not capture_dir.is_dir():
        raise ValueError(f"capture path is not a directory: {capture_dir}")

    actual_entries = {entry.name for entry in capture_dir.iterdir()}
    missing = sorted(EXPECTED_CAPTURE_ENTRIES - actual_entries)
    unexpected = sorted(actual_entries - EXPECTED_CAPTURE_ENTRIES)
    if missing or unexpected:
        raise ValueError(
            f"capture directory has missing entries {missing} "
            f"and unexpected entries {unexpected}: {capture_dir}"
        )

    for name in sorted(EXPECTED_CAPTURE_ENTRIES):
        entry = capture_dir / name
        if entry.is_symlink() or not entry.is_file():
            raise ValueError(f"capture entry must be a regular file: {entry}")


def ensure_entrypoint_binding(script_path: Path, exporter_root: Path) -> None:
    expected_path = (exporter_root / EXPORTER_ENTRYPOINT).resolve()
    if script_path.resolve() != expected_path:
        raise ValueError(
            "executed converter does not belong to --exporter-worktree: "
            f"{script_path.resolve()} != {expected_path}"
        )


def ensure_only_test_harness_changes(changed_paths: set[str]) -> None:
    unexpected = sorted(changed_paths - ALLOWED_EXPORTER_CHANGES)
    if unexpected:
        raise ValueError(
            "exporter commit changes paths outside the test-only allowlist: "
            + ", ".join(unexpected)
        )


def ensure_expected_exporter_commit(actual: str, expected: str) -> None:
    if actual != expected:
        raise ValueError(
            f"exporter HEAD changed: expected {expected}, found {actual}"
        )


def ensure_exporter_worktree_state(
    exporter_root: Path, expected_exporter_commit: str
) -> None:
    ensure_expected_exporter_commit(
        git(exporter_root, "rev-parse", "HEAD"), expected_exporter_commit
    )
    if git(
        exporter_root,
        "status",
        "--porcelain",
        "--untracked-files=all",
    ):
        raise ValueError("exporter worktree must be clean")


def resolve_final_output_path(output_dir: Path, exporter_root: Path) -> Path:
    if output_dir.exists() or output_dir.is_symlink():
        raise ValueError(f"output directory already exists: {output_dir}")

    resolved_output = output_dir.resolve()
    try:
        resolved_output.relative_to(exporter_root)
    except ValueError:
        return resolved_output
    raise ValueError(
        f"output directory must be outside the exporter worktree: {output_dir}"
    )


def create_staging_output(final_output: Path) -> tempfile.TemporaryDirectory:
    # TemporaryDirectory forces 0700; match Path.mkdir's mode after the current umask.
    current_umask = os.umask(0)
    os.umask(current_umask)
    staging = tempfile.TemporaryDirectory(
        prefix=f".{final_output.name}.staging.", dir=final_output.parent
    )
    Path(staging.name).chmod(0o777 & ~current_umask)
    return staging


def finalize_staged_output(
    staging: tempfile.TemporaryDirectory,
    final_output: Path,
    exporter_root: Path,
    expected_exporter_commit: str,
) -> None:
    try:
        ensure_exporter_worktree_state(exporter_root, expected_exporter_commit)
        if final_output.exists() or final_output.is_symlink():
            raise ValueError(f"output directory already exists: {final_output}")
        Path(staging.name).rename(final_output)
    except BaseException:
        staging.cleanup()
        raise
    staging.cleanup()


def expect_value_error(action: Callable[[], Any], message: str) -> None:
    try:
        action()
    except ValueError:
        return
    raise AssertionError(message)


def validate_exporter_provenance(
    exporter_worktree: Path,
    expected_exporter_commit: str,
) -> tuple[str, str, str, list[str]]:
    exporter_root = exporter_worktree.resolve(strict=True)
    git_root = Path(git(exporter_root, "rev-parse", "--show-toplevel")).resolve(
        strict=True
    )
    if exporter_root != git_root:
        raise ValueError("--exporter-worktree must be the git worktree root")

    script_path = Path(__file__).resolve(strict=True)
    ensure_entrypoint_binding(script_path, exporter_root)
    resolved_expected_commit = git(
        exporter_root, "rev-parse", f"{expected_exporter_commit}^{{commit}}"
    )
    ensure_expected_exporter_commit(
        resolved_expected_commit, expected_exporter_commit
    )
    ensure_exporter_worktree_state(exporter_root, expected_exporter_commit)

    release_commit = git(exporter_root, "rev-parse", "v0.7.3^{}")
    subprocess.run(
        [
            "git",
            "-C",
            str(exporter_root),
            "merge-base",
            "--is-ancestor",
            release_commit,
            WRITER_PRODUCER_COMMIT,
        ],
        check=True,
    )
    subprocess.run(
        [
            "git",
            "-C",
            str(exporter_root),
            "merge-base",
            "--is-ancestor",
            WRITER_PRODUCER_COMMIT,
            expected_exporter_commit,
        ],
        check=True,
    )
    changed_paths = sorted(
        filter(
            None,
            git(
                exporter_root,
                "diff",
                "--name-only",
                "--no-renames",
                f"{WRITER_PRODUCER_COMMIT}..{expected_exporter_commit}",
            ).splitlines(),
        )
    )
    ensure_only_test_harness_changes(set(changed_paths))

    entrypoint_blob = git(
        exporter_root,
        "rev-parse",
        f"{expected_exporter_commit}:{EXPORTER_ENTRYPOINT.as_posix()}",
    )
    working_entrypoint_blob = git(exporter_root, "hash-object", str(script_path))
    if working_entrypoint_blob != entrypoint_blob:
        raise ValueError(
            "executed converter blob does not match the recorded exporter commit"
        )
    return expected_exporter_commit, release_commit, entrypoint_blob, changed_paths


def provenance_self_test() -> None:
    exporter_root = Path("/arc-a1b-exporter-self-test")
    ensure_entrypoint_binding(exporter_root / EXPORTER_ENTRYPOINT, exporter_root)
    ensure_only_test_harness_changes(set(ALLOWED_EXPORTER_CHANGES))
    ensure_expected_exporter_commit("a" * 40, "a" * 40)

    expect_value_error(
        lambda: ensure_entrypoint_binding(
            exporter_root / "copied-transformer.py", exporter_root
        ),
        "provenance self-test accepted a copied entrypoint",
    )
    expect_value_error(
        lambda: ensure_only_test_harness_changes(
            set(ALLOWED_EXPORTER_CHANGES) | {"crates/node/src/lib.rs"}
        ),
        "provenance self-test accepted a source path outside the allowlist",
    )
    expect_value_error(
        lambda: ensure_expected_exporter_commit("a" * 40, "b" * 40),
        "provenance self-test accepted a mismatched exporter commit",
    )
    output_staging_self_test()
    print("provenance self-test passed")


def output_staging_self_test() -> None:
    with tempfile.TemporaryDirectory(prefix="arc-a1b-output-test.") as temp:
        test_root = Path(temp)
        exporter_root = test_root / "exporter"
        exporter_root.mkdir()
        exporter_root = exporter_root.resolve(strict=True)
        inside_output = exporter_root / "fixtures"
        expect_value_error(
            lambda: resolve_final_output_path(inside_output, exporter_root),
            "output self-test accepted a path inside the exporter",
        )
        if inside_output.exists():
            raise AssertionError("output preflight created a rejected final path")

        exporter_alias = test_root / "exporter-alias"
        exporter_alias.symlink_to(exporter_root)
        expect_value_error(
            lambda: resolve_final_output_path(
                exporter_alias / "fixtures", exporter_root
            ),
            "output self-test accepted a symlink into the exporter",
        )

        subprocess.run(
            ["git", "-C", str(exporter_root), "init", "--quiet"], check=True
        )
        subprocess.run(
            [
                "git",
                "-C",
                str(exporter_root),
                "-c",
                "user.name=Arc fixture self-test",
                "-c",
                "user.email=arc-fixture-self-test@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "core.hooksPath=/dev/null",
                "commit",
                "--quiet",
                "--no-verify",
                "--allow-empty",
                "-m",
                "self-test",
            ],
            check=True,
        )
        expected_commit = git(exporter_root, "rev-parse", "HEAD")
        output_parent = test_root / "outputs"
        output_parent.mkdir()
        final_output = output_parent / "fixtures"
        staging = create_staging_output(final_output)
        staging_path = Path(staging.name)
        (staging_path / "manifest.json").write_text("{}")
        (exporter_root / "dirty").write_text("dirty")
        expect_value_error(
            lambda: finalize_staged_output(
                staging, final_output, exporter_root, expected_commit
            ),
            "output self-test accepted a dirty exporter",
        )
        if final_output.exists() or staging_path.exists():
            raise AssertionError("failed output publication left files behind")

        (exporter_root / "dirty").unlink()
        staging = create_staging_output(final_output)
        staging_path = Path(staging.name)
        (staging_path / "manifest.json").write_text("{}")
        control_output = test_root / "control-output"
        control_output.mkdir()
        finalize_staged_output(staging, final_output, exporter_root, expected_commit)
        if not (final_output / "manifest.json").is_file() or staging_path.exists():
            raise AssertionError("output self-test failed to publish staged output")
        if (final_output.stat().st_mode & 0o777) != (
            control_output.stat().st_mode & 0o777
        ):
            raise AssertionError("published output root does not honor the current umask")


def capture_set_self_test() -> None:
    with tempfile.TemporaryDirectory(prefix="arc-a1b-capture-set-test.") as temp:
        capture_dir = Path(temp)
        for name in EXPECTED_CAPTURE_ENTRIES:
            (capture_dir / name).write_text("{}")
        validate_capture_directory(capture_dir)

        missing = capture_dir / "4-failed-create.json"
        missing.unlink()
        expect_value_error(
            lambda: validate_capture_directory(capture_dir),
            "capture-set self-test accepted a missing file",
        )
        missing.write_text("{}")

        unexpected = capture_dir / "5-unexpected.json"
        unexpected.write_text("{}")
        expect_value_error(
            lambda: validate_capture_directory(capture_dir),
            "capture-set self-test accepted an extra file",
        )
        unexpected.unlink()

        unexpected_directory = capture_dir / "unexpected-directory"
        unexpected_directory.mkdir()
        expect_value_error(
            lambda: validate_capture_directory(capture_dir),
            "capture-set self-test accepted an extra directory",
        )
        unexpected_directory.rmdir()

        expected = capture_dir / "0-genesis.json"
        expected.unlink()
        expected.symlink_to(capture_dir / "capture-context.json")
        expect_value_error(
            lambda: validate_capture_directory(capture_dir),
            "capture-set self-test accepted a symlink",
        )
        expected.unlink()
        expected.mkdir()
        expect_value_error(
            lambda: validate_capture_directory(capture_dir),
            "capture-set self-test accepted a directory",
        )
    print("capture-set self-test passed")


def generate(staging_holder: list[tempfile.TemporaryDirectory]) -> None:
    args = parse_args()
    exporter_root = args.exporter_worktree.resolve(strict=True)
    exporter_commit, release_commit, entrypoint_blob, exporter_changed_paths = (
        validate_exporter_provenance(
            exporter_root, args.expected_exporter_commit
        )
    )
    final_output = resolve_final_output_path(args.output_dir, exporter_root)
    if not args.capture_dir.is_dir():
        raise SystemExit(f"capture directory does not exist: {args.capture_dir}")
    if not args.verification_capture_dir.is_dir():
        raise SystemExit(
            "verification capture directory does not exist: "
            f"{args.verification_capture_dir}"
        )
    validate_capture_directory(args.capture_dir)
    validate_capture_directory(args.verification_capture_dir)

    format_reference_head = git(args.format_reference_worktree, "rev-parse", "HEAD")
    format_reference_tag = git(
        args.format_reference_worktree,
        "rev-parse",
        f"{FORMAT_REFERENCE_RELEASE}^{{}}",
    )
    if git(args.format_reference_worktree, "status", "--porcelain"):
        raise ValueError("format-reference worktree must be clean")
    if (
        format_reference_head != FORMAT_REFERENCE_COMMIT
        or format_reference_tag != FORMAT_REFERENCE_COMMIT
    ):
        raise ValueError(
            "format-reference worktree must be checked out at "
            f"{FORMAT_REFERENCE_RELEASE}@{FORMAT_REFERENCE_COMMIT}"
        )
    for source, expected_blob in FORMAT_REFERENCE_SOURCES.items():
        actual_blob = git(
            args.format_reference_worktree,
            "rev-parse",
            f"{FORMAT_REFERENCE_COMMIT}:{source}",
        )
        if actual_blob != expected_blob:
            raise ValueError(
                f"format-reference source {source} is {actual_blob}, expected {expected_blob}"
            )

    context = json.loads((args.capture_dir / CAPTURE_CONTEXT).read_text())
    verification_context = json.loads(
        (args.verification_capture_dir / CAPTURE_CONTEXT).read_text()
    )
    if context != verification_context:
        raise ValueError("independent captures used different chain context")
    captures = load_captures(args.capture_dir)
    verification_captures = load_captures(args.verification_capture_dir)
    normalized_capture_sha256 = []
    for (label, output), (verification_label, verification_output) in zip(
        captures, verification_captures, strict=True
    ):
        if label != verification_label:
            raise ValueError("independent capture labels do not match")
        normalized = normalized_capture(output)
        if normalized != normalized_capture(verification_output):
            raise ValueError(
                f"{label}: independent captures differ beyond process_start_timestamp"
            )
        normalized_capture_sha256.append(
            {
                "label": label,
                "sha256": hashlib.sha256(normalized).hexdigest(),
            }
        )

    ensure_exporter_worktree_state(
        exporter_root, args.expected_exporter_commit
    )
    final_output.parent.mkdir(parents=True, exist_ok=True)
    staging = create_staging_output(final_output)
    staging_holder.append(staging)
    args.output_dir = Path(staging.name)
    files: list[dict[str, Any]] = []
    blocks: list[dict[str, Any]] = []
    previous_hash: str | None = None
    genesis_header: dict[str, Any] | None = None
    genesis_state_diff: bytes | None = None

    def record(path: Path, kind: str, number: int | None = None) -> None:
        entry: dict[str, Any] = {
            "path": relative(path, args.output_dir),
            "kind": kind,
            "bytes": path.stat().st_size,
            "sha256": sha256(path),
        }
        if number is not None:
            entry["block_number"] = number
        files.append(entry)

    for expected_number, (label, output) in enumerate(captures):
        block_file = output["block_file"]
        header = output["header"]
        state_diff_hex = output["state_diff"]
        number = decode_quantity(header["number"])
        block_number = decode_quantity(block_file["block"]["height"])
        if number != expected_number or block_number != expected_number:
            raise ValueError(
                f"{label}: expected block {expected_number}, got {number}/{block_number}"
            )
        if block_file["block"]["id"].lower() != header["hash"].lower():
            raise ValueError(f"{label}: BlockFile/Header block hash mismatch")
        if previous_hash is not None and header["parentHash"].lower() != previous_hash:
            raise ValueError(f"{label}: Header parent hash is not linear")
        previous_hash = header["hash"].lower()

        try:
            state_diff = bytes.fromhex(state_diff_hex.removeprefix("0x"))
        except ValueError as error:
            raise ValueError(f"{label}: invalid StateDiff hex") from error
        if not state_diff:
            raise ValueError(f"{label}: producer returned an empty StateDiff")

        rpc_path = args.output_dir / "rpc" / f"{number}-{label}.json"
        header_path = (
            args.output_dir / "per-block" / header["hash"].lower() / "block"
        )
        state_diff_path = (
            args.output_dir
            / "per-block"
            / header["stateRoot"].lower()
            / "stateDiff"
        )
        block_file_path = (
            args.output_dir / "outer" / block_file["block"]["id"].lower()
        )
        write_new(rpc_path, json_bytes(output, pretty=True))
        write_new(header_path, gzip_json(header))
        write_new(state_diff_path, state_diff)
        write_new(block_file_path, gzip_json(block_file))
        record(rpc_path, "rpc_debank_output_json", number)
        record(header_path, "per_block_header_gzip_json", number)
        record(state_diff_path, "per_block_state_diff_raw_rlp", number)
        record(block_file_path, "outer_block_file_gzip_json", number)

        blocks.append(
            {
                "label": label,
                "number": number,
                "hash": header["hash"].lower(),
                "parent_hash": header["parentHash"].lower(),
                "state_root": header["stateRoot"].lower(),
                "extra_data": header["extraData"].lower(),
                "validation_hash": output["validation_hash"],
                "process_start_timestamp": block_file["block"][
                    "process_start_timestamp"
                ],
                "rpc": relative(rpc_path, args.output_dir),
                "header": relative(header_path, args.output_dir),
                "state_diff": relative(state_diff_path, args.output_dir),
                "block_file": relative(block_file_path, args.output_dir),
            }
        )
        if number == 0:
            genesis_header = header
            genesis_state_diff = state_diff

    if genesis_header is None or genesis_state_diff is None:
        raise ValueError("genesis capture is missing")

    bundle_header_path = args.output_dir / "bundle" / "0" / "block"
    bundle_state_diff_path = args.output_dir / "bundle" / "0" / "stateDiff"
    write_new(bundle_header_path, gzip_json([genesis_header]))
    index = bytearray(STATE_DIFF_INDEX_BYTES)
    index[0] = 1  # position zero is a real producer StateDiff
    offset_one = STATE_DIFF_BITMAP_BYTES + 8
    index[offset_one : offset_one + 8] = len(genesis_state_diff).to_bytes(8, "big")
    write_new(bundle_state_diff_path, bytes(index) + genesis_state_diff)
    record(bundle_header_path, "bundle0_header_gzip_json")
    record(bundle_state_diff_path, "bundle0_index_and_state_diff_rlp")

    files.sort(key=lambda item: item["path"])
    ensure_exporter_worktree_state(
        exporter_root, args.expected_exporter_commit
    )
    manifest = {
        "schema_version": 1,
        "producer_baseline": {
            "repository": "Chaintable/arc-node",
            "commit": WRITER_PRODUCER_COMMIT,
            "release": "v0.7.3",
            "release_commit": release_commit,
        },
        "producer": {
            "repository": "Chaintable/arc-node",
            "commit": exporter_commit,
            "baseline_commit": WRITER_PRODUCER_COMMIT,
            "changes_from_baseline": exporter_changed_paths,
            "source_policy": "only test harness and fixture scripts may differ",
        },
        "exporter": {
            "repository": "Chaintable/arc-node",
            "commit": exporter_commit,
        },
        "fixture_transformer": {
            "repository": "Chaintable/arc-node",
            "commit": exporter_commit,
            "entrypoint": "scripts/fixtures/build_leafage_a1b_fixtures.py",
            "entrypoint_blob": entrypoint_blob,
            "encoding": {
                "rpc": "DebankOutPut JSON",
                "per_block_header": "gzip JSON Header",
                "per_block_state_diff": "raw RLP BlockStorageDiff",
                "outer_block_file": "gzip JSON BlockFile",
                "bundle0": "8133-byte index followed by genesis RLP entry",
            },
            "compatibility_contract": {
                "gzip_json_payload": (
                    "compact JSON without a trailing newline, matching "
                    "background-tracer serde_json::to_vec semantics"
                ),
                "gzip_container": (
                    "deterministic fixture encoding; gzip header and compressed bytes "
                    "are not a pipeline contract"
                ),
            },
        },
        "format_reference": {
            "repository": "Chaintable/background-tracer",
            "release": FORMAT_REFERENCE_RELEASE,
            "commit": FORMAT_REFERENCE_COMMIT,
            "sources": [
                {"path": source, "blob": blob}
                for source, blob in FORMAT_REFERENCE_SOURCES.items()
            ],
            "scope": [
                "DebankOutPut JSON input schema",
                "gzip JSON Header and BlockFile objects",
                "raw RLP BlockStorageDiff object",
            ],
            "executed_by_fixture_generation": False,
        },
        "generation": {
            "command": (
                "BACKGROUND_TRACER_WORKTREE=/path/to/background-tracer "
                "LEAFAGE_A1B_FIXTURE_OUT=/tmp/leafage-arc-a1b-fixtures "
                "./scripts/generate-leafage-a1b-fixtures.sh"
            ),
            "comparison_normalization": {"process_start_timestamp": 0},
            "independent_capture_verification": normalized_capture_sha256,
            "deterministic_payload": {
                "timestamp": "parent timestamp + 1",
                "prev_randao": "block number repeated as one byte",
            },
        },
        "chain": context,
        "coverage": [
            "genesis full alloc",
            "empty block EIP-2935 and SystemAccounting hooks",
            "native transfer",
            "successful CREATE root",
            "CALL root with internal CREATE2",
            "failed CREATE root",
            "new account, code, and storage",
        ],
        "excluded": ["StorageCleared"],
        "blocks": blocks,
        "files": files,
    }
    manifest_path = args.output_dir / "manifest.json"
    write_new(manifest_path, json_bytes(manifest, pretty=True))
    finalize_staged_output(
        staging, final_output, exporter_root, args.expected_exporter_commit
    )


def main() -> None:
    staging_holder: list[tempfile.TemporaryDirectory] = []
    try:
        generate(staging_holder)
    finally:
        for staging in staging_holder:
            staging.cleanup()


if __name__ == "__main__":
    if sys.argv[1:] == ["--self-test-provenance"]:
        provenance_self_test()
    elif sys.argv[1:] == ["--self-test-capture-set"]:
        capture_set_self_test()
    elif sys.argv[1:2] == ["--preflight-output"]:
        output_preflight_cli(sys.argv[2:])
    else:
        main()
