#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
writer_producer="23d38e7d0cbf54e184faf3751c619f2169b3ed79"
format_reference_release="v0.1.43"
format_reference_commit="7c4e096bfbc132dcb79312e2371c80919b966a52"

panic_log_detected() {
  local log_file="$1"
  LC_ALL=C grep -Eq "thread .* panicked at|panicked at .*deferred_trie\.rs(:[0-9]+)?" "$log_file"
}

validate_capture_result() {
  local cargo_status="$1"
  local tee_status="$2"
  local log_file="$3"

  if (( cargo_status != 0 )); then
    echo "Arc fixture exporter cargo test failed with status $cargo_status" >&2
    return "$cargo_status"
  fi
  if (( tee_status != 0 )); then
    echo "failed to capture Arc fixture exporter log with status $tee_status" >&2
    return "$tee_status"
  fi
  if panic_log_detected "$log_file"; then
    echo "Arc fixture exporter emitted a Rust panic despite cargo status 0" >&2
    return 1
  fi
}

panic_detection_self_test() (
  local test_dir safe_log panic_log
  test_dir="$(mktemp -d "${TMPDIR:-/tmp}/arc-a1b-panic-test.XXXXXX")"
  safe_log="$test_dir/safe.log"
  panic_log="$test_dir/exit-zero-with-panic.log"
  trap 'rm -rf -- "$test_dir"' EXIT

  printf '%s\n' 'test result: ok. 1 passed; 0 failed' >"$safe_log"
  printf '%s\n' \
    'test result: ok. 1 passed; 0 failed' \
    "thread 'proof-acct-00' panicked at /checkout/reth/crates/chain-state/src/deferred_trie.rs:316:9:" \
    'wait_cloned must not be called from a rayon worker thread' >"$panic_log"

  if ! validate_capture_result 0 0 "$safe_log"; then
    echo "capture validator rejected a clean cargo log" >&2
    return 1
  fi
  if validate_capture_result 0 0 "$panic_log" >/dev/null 2>&1; then
    echo "capture validator accepted an exit-zero deferred_trie panic log" >&2
    return 1
  fi
  echo "capture validator self-test passed"
)

if [[ "${1:-}" == "--self-test-panic-detection" ]]; then
  panic_detection_self_test
  exit 0
fi

if [[ "${1:-}" == "--self-test-provenance" ]]; then
  python3 "$repo_root/scripts/fixtures/build_leafage_a1b_fixtures.py" \
    --self-test-provenance
  exit 0
fi

if [[ "${1:-}" == "--self-test-capture-set" ]]; then
  python3 "$repo_root/scripts/fixtures/build_leafage_a1b_fixtures.py" \
    --self-test-capture-set
  exit 0
fi

if [[ $# -ne 0 ]]; then
  echo "usage: $0 [--self-test-panic-detection|--self-test-provenance|--self-test-capture-set]" >&2
  exit 2
fi

panic_detection_self_test
python3 "$repo_root/scripts/fixtures/build_leafage_a1b_fixtures.py" \
  --self-test-provenance
python3 "$repo_root/scripts/fixtures/build_leafage_a1b_fixtures.py" \
  --self-test-capture-set

format_reference_worktree="${BACKGROUND_TRACER_WORKTREE:?set BACKGROUND_TRACER_WORKTREE}"
output_dir="${LEAFAGE_A1B_FIXTURE_OUT:?set LEAFAGE_A1B_FIXTURE_OUT to a new output directory}"
python3 "$repo_root/scripts/fixtures/build_leafage_a1b_fixtures.py" \
  --preflight-output \
  --output-dir "$output_dir" \
  --exporter-worktree "$repo_root"

assert_exporter_source_unchanged() {
  local current_head
  current_head="$(git -C "$repo_root" rev-parse HEAD)"
  if [[ "$current_head" != "$exporter_commit" ]]; then
    echo "Arc exporter HEAD changed: expected $exporter_commit, found $current_head" >&2
    return 1
  fi
  if [[ -n "$(git -C "$repo_root" status --porcelain --untracked-files=all)" ]]; then
    echo "Arc exporter worktree must remain clean" >&2
    return 1
  fi
}

exporter_commit="$(git -C "$repo_root" rev-parse HEAD)"
if ! git -C "$repo_root" merge-base --is-ancestor "$writer_producer" "$exporter_commit"; then
  echo "Arc exporter must descend from writer producer $writer_producer" >&2
  exit 1
fi
assert_exporter_source_unchanged
format_reference_head="$(git -C "$format_reference_worktree" rev-parse HEAD)"
format_reference_tag_commit="$(git -C "$format_reference_worktree" rev-parse "${format_reference_release}^{}")"
if [[ "$format_reference_head" != "$format_reference_commit" ]] || \
  [[ "$format_reference_tag_commit" != "$format_reference_commit" ]]; then
  echo "background-tracer format reference must be $format_reference_release@$format_reference_commit" >&2
  exit 1
fi
if [[ -n "$(git -C "$format_reference_worktree" status --porcelain --untracked-files=all)" ]]; then
  echo "background-tracer format-reference worktree must be clean" >&2
  exit 1
fi
capture_dir="$(mktemp -d "${TMPDIR:-/tmp}/arc-a1b-capture.XXXXXX")"
verification_capture_dir="$(mktemp -d "${TMPDIR:-/tmp}/arc-a1b-verify.XXXXXX")"
capture_log_dir="$(mktemp -d "${TMPDIR:-/tmp}/arc-a1b-logs.XXXXXX")"
capture_log="$capture_log_dir/capture.log"
verification_capture_log="$capture_log_dir/verification.log"
cleanup() {
  rm -rf -- "$capture_dir" "$verification_capture_dir" "$capture_log_dir"
}
trap cleanup EXIT

run_capture() {
  local target_dir="$1"
  local log_file="$2"
  local -a pipeline_status

  assert_exporter_source_unchanged
  set +e
  ARC_A1B_CAPTURE_DIR="$target_dir" \
    rustup run 1.91.1 cargo test \
      --manifest-path "$repo_root/Cargo.toml" \
      --locked \
      -p arc-execution-e2e \
      --test export_leafage_a1b \
      export_arc_a1b_fixtures \
      -- --ignored --exact --nocapture 2>&1 | tee "$log_file"
  pipeline_status=("${PIPESTATUS[@]}")
  set -e

  assert_exporter_source_unchanged
  validate_capture_result "${pipeline_status[0]}" "${pipeline_status[1]}" "$log_file"
}

run_capture "$capture_dir" "$capture_log"
run_capture "$verification_capture_dir" "$verification_capture_log"

rm -- "$capture_log" "$verification_capture_log"

assert_exporter_source_unchanged
python3 "$repo_root/scripts/fixtures/build_leafage_a1b_fixtures.py" \
  --capture-dir "$capture_dir" \
  --verification-capture-dir "$verification_capture_dir" \
  --output-dir "$output_dir" \
  --exporter-worktree "$repo_root" \
  --expected-exporter-commit "$exporter_commit" \
  --format-reference-worktree "$format_reference_worktree"

echo "Arc A1b fixtures written to $output_dir"
