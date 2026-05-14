# DeBank Custom RPC — Implementation Notes

> Decision log + open questions encountered while porting DeBank custom RPCs (`pre_traceMany`, `eth_multiCall`, `trace_debankBlock`) from Tempo to Arc.
>
> Append entries in chronological order. Mark each entry with date and status (`[decided]`, `[open]`, `[resolved]`, `[deferred]`).

---

## Reference repositories

| Repo | Path | Purpose |
|---|---|---|
| Tempo `debank-rpc` | `~/code/task_tempo/crates/debank-rpc/` on `debank` | Primary source. Ports done against reth `8328732` (≈v1.11.3) + revm 36. |
| `debankdefi/reth` | `~/ghorg/debankdefi/reth` on `debank` | Original implementation against reth v1.6.0 + revm 27. RPC trait files: `crates/rpc/rpc-api/src/pre.rs`, `crates/rpc/rpc-eth-api/src/core.rs`. |
| `chaintable/mega-reth` | `~/ghorg/chaintable/mega-reth/crates/megaeth/node/src/add_ons.rs` on `debank` | mega-reth style `launch_add_ons_with` registration pattern. Tempo design doc credits this as the "RPC registration pattern" source. |
| Tempo design doc | `~/code/task_tempo/docs/debank-rpc-design.md` | revm 36 adaptation notes (§8 has 9 decision points). |
| Tempo generic-node doc | `~/code/task_tempo/docs/generic-node.md` | `trace_debankBlock` flow + Tempo-specific behaviors to strip. |
| Tempo test plan | `~/code/task_tempo/docs/test-plan-generic-node.md` | 136-item test plan covering output schema. |
| Arc analysis | `~/code/task_arc/ARC_ANALYSIS.md` | Arc's chain architecture (Reth + Malachite, USDC-as-native). |

---

## 2026-05-14 — initial decisions

### D1. Crate placement [decided]

Create `crates/debank-rpc/` as a standalone workspace member, registered via `members = ["crates/*", ...]` (already wildcards-included). No modifications to reth-core. Mirrors Tempo's mega-reth-style isolation.

**Why:** Minimizes upstream-rebase conflicts. Same approach Tempo took.

### D2. Native token sentinel (`0xeeee...`) behavior on Arc [decided]

Keep Tempo's defaults verbatim:
- `balanceOf(addr)` → `state.basic_ref(addr).balance` (equivalent to `eth_getBalance`)
- `name()` / `symbol()` → ABI-encoded `"ETH"`
- `decimals()` → 18
- `totalSupply()` → 1

**Why:** User direction — "balanceOf = getValue 行为". On Arc, native balance happens to be USDC (18-dec), but `0xeeee` is a generic sentinel; callers needing real USDC metadata should query the real USDC contract at `0x3600000000000000000000000000000000000000`.

### D3. `DebankTransaction` schema preservation [decided]

Retain Tempo-only fields (`calls: Option<Vec<TempoCall>>`, `fee_token: Option<Address>`, `fee_payer_signature: Option<serde_json::Value>`) in the struct definition. On Arc, these always serialize as `null`. Do not rename `TempoCall` — only referenced by an always-`None` field on Arc.

**Why:** Same `background-tracer` Go consumer ingests Tempo and Arc output. Schema parity avoids forking the consumer code. Per `task_tempo/docs/generic-node.md` "pipeline Go 版的字段兼容性" — Go side already tolerates null.

### D4. revm 36 → revm 34 adaptation [decided]

Port from Tempo's revm 36 form back to Arc's revm 34:
- `gas: ResultGas` ⇒ `gas_used: u64` (direct field, no method call)
- `Revert { gas, output, logs }` ⇒ `Revert { gas_used, output }` (drop `logs`)
- `Halt { reason, gas, logs }` ⇒ `Halt { reason, gas_used }` (drop `logs`)
- `Output::into_data() -> Bytes` ⇒ unchanged (same in revm 34 and 36)

**Source:** revm-context-interface 14.0.0 `result.rs` confirmed via crates.io download.

### D5. Tempo specifics to strip [decided]

| Tempo path | Removal action | Reason |
|---|---|---|
| AA tx (type=0x76) `to_addr`/`input` unpacking | Remove `is_aa` branch; use standard `tx.to()` / `tx.input()` | Arc has no AA tx |
| Revert-tx fee log reconstruction via receipt serde | Remove the receipt-replay branch | Arc has no handler-emitted fee logs |
| AA root-trace classification fix (CallTraceArena `success` unreliable) | Remove merge-back logic | Arc has no AA wrapper traces |
| TempoEvm handler / TIP-20 fee Transfer comments | Remove | Arc uses stock EIP-1559 path |

### D6. `exclude_precompile_calls = true` [decided, deferred verification]

Keep at `true`. Arc's custom precompiles at `0x1800...` segment are injected via reth's precompile lookup, not via `warm_addresses()` — same pattern Tempo uses for TIP-20.

**Open verification:** confirm during Phase 2 that Arc's precompile registration does not put `0x1800...` addresses into `warm_addresses()`. If it does, this setting would hide their traces.

### D7. `unwrap_used = "deny"` workspace lint [decided]

Replace `unwrap()` in ported test code with `expect("…")` (or `unwrap_or_default()` where appropriate). Tempo's `erc20_handle.rs` tests have 3 such occurrences in `name_returns_eth` / `symbol_returns_eth` decoders.

### D8. Genesis special-case handling [open]

Tempo's `trace_block.rs` says "No empty block shortcut: Tempo has a system tx in every block." Need to verify Arc's genesis + early blocks:
- Does Arc's chainspec inject synthetic genesis txs (Tempo: 15 synthetic txs in block 0)?
- Are there always system txs in non-genesis blocks?

**Action:** during Phase 2 Task 13, test against Arc's genesis. If divergent, may need genesis special-case branch like Tempo's.

---

## Open questions / decisions deferred to execution

(Subagents fill these in as they encounter ambiguity. Each entry: short question, current understanding, what would resolve it.)

### Q1. Final list of `DebankTraceBlock` trait bounds against `ArcEthApi` [resolved for Phase 1; Phase 2 still open]

Tempo's `DebankTraceBlock<Eth>` requires `Eth: EthApiTypes + LoadReceipt + LoadBlock + LoadState + TraceExt + EthTransactions + EthBlocks + SpawnBlocking`. Need to confirm Arc's `ArcEthApiBuilder::EthApi` satisfies all of these on reth v1.11.3.

**Phase 1 resolution (Task 8):** Arc's `ArcEthApiBuilder::EthApi = EthApiFor<N, NetworkT>` — the
standard reth `EthApiFor` type alias (default `NetworkT = Ethereum`). It satisfies:
- `PreApi<Eth>` bounds: `EthApiTypes + EthTransactions + TraceExt + 'static` — OK
- `DebankEthExt<Eth>` bounds: `EthApiTypes + EthCall + 'static`, plus
  `RpcTxReq<Eth::NetworkTypes>: AsRef<TransactionRequest>` — OK because for the standard
  `Ethereum` network type `RpcTxReq` resolves to `alloy_rpc_types_eth::TransactionRequest`
  which has the blanket `AsRef<Self>` impl.

`cargo check --workspace` after Task 8 wiring passed cleanly with no trait-bound diagnostics, confirming this.

**Phase 2:** the wider `DebankTraceBlock` bound list (`LoadReceipt + LoadBlock + LoadState + EthBlocks + SpawnBlocking`) is still to be confirmed during Task 14; expected to pass since `EthApiFor` is the same reth-standard type used by Tempo (where the bounds are known to hold).

### Q2. AccessList field on `DebankTransaction` for Arc [open]

Tempo `DebankTransaction` has `pub access_list: ...` shared between AA tx and EIP-2930. Need to verify how Arc populates this for standard EIP-2930/1559 txs — same as Tempo's non-AA path, but worth confirming the alloy `Transaction` trait method name on the Arc-pinned alloy version.

**Resolution:** Task 12.

---

## 2026-05-14 — Task 2 (crate scaffold)

### D9. `reth-rpc-convert` added to workspace.dependencies [decided]

`reth-rpc-convert` was referenced in `debank-rpc/Cargo.toml` as `workspace = true` but was absent from the root `[workspace.dependencies]`. It was already present in `Cargo.lock` at v1.11.3 (pulled transitively), so no new resolution was needed — only the explicit declaration. Added with the same `git = "https://github.com/paradigmxyz/reth", tag = "v1.11.3"` pattern as all other reth-prefixed workspace deps. No `default-features = false` because none of the other single-purpose reth RPC crates (`reth-rpc`, `reth-rpc-api`, etc.) set it.

---

## 2026-05-14 — Task 3 (types.rs port)

### D10. Crate-level `#![cfg_attr(test, allow(clippy::unwrap_used))]` [decided]

Arc workspace declares `unwrap_used = "deny"` (clippy lint) and CI runs
`cargo clippy --all-targets -- -D warnings`. Tempo's `types.rs` test code uses
9 `unwrap()` calls; `erc20_handle.rs` and `pre.rs` will have more.

Instead of replacing each `unwrap()` with `expect("…")` per-file (the
original plan), apply a single crate-root attribute:

```rust
#![cfg_attr(test, allow(clippy::unwrap_used))]
```

This allows `unwrap()` in test code across the entire `debank-rpc` crate.
Rationale: test code is allowed to panic on unexpected results; `unwrap()`'s
panic location is informative enough for test debugging; per-file `expect()`
rewrites add noise without improving test quality. Matches Rust community
convention.

**Plan amendment:** Task 4's "Step 2: Replace `unwrap()` in test code with
`expect()`" is now obsolete — the crate-level attribute handles it.
Task 6 (pre.rs) similarly no longer needs unwrap replacements.

## 2026-05-14 — Task 5 (multi_call.rs port)

### D11. Production-code `unwrap()` removal [decided]

`multi_call.rs:72` had `result_response.last().unwrap()` in production. Workspace
`clippy::unwrap_used = "deny"` applies to production code (D10's
`cfg_attr(test, allow(...))` only covers tests). The Tempo source has this
pattern because Tempo doesn't deny `unwrap_used` at workspace level.

Replaced with `result_response.last().is_some_and(|r| r.code != Success)` — drops
the now-redundant `!is_empty()` guard.

**Pattern to watch:** subsequent ports (pre.rs, debank_trace.rs, trace_block.rs)
may have similar production-code `unwrap()` calls inherited from Tempo. Each
needs case-by-case rewrite (typically `is_some_and` / `map_or` / `?` with proper
error type).

## 2026-05-14 — Task 6 (pre.rs port)

### D12. `alloy-consensus` workspace caret pin resolves to 1.7.3 [decided, note]

Arc declares `alloy-consensus = "1.6.3"` (with caret semver) in
`[workspace.dependencies]`. `Cargo.lock` resolves to **1.7.3**. Tempo
locks to **2.0.4** (a major version up).

`TransactionInfo` gained a `block_timestamp: Option<u64>` field in
alloy-consensus 2.0.4. In 1.7.3 it has only 5 fields:
`hash, index, block_hash, block_number, base_fee`.

Tempo's `pre.rs` constructs `TransactionInfo { ..., block_timestamp: Some(...) }`.
On Arc, removed the field assignment; the `block_timestamp: u64` parameter
is still computed and passed to `Log { block_timestamp: Some(...), .. }`
(present in alloy-rpc-types-eth 1.6.3+).

**Watch:** if Arc later bumps to alloy-consensus 2.x, this adaptation
needs to be reverted. Recommend documenting in a comment near the
construction site as well.

### D13. Crate-level `#![allow(clippy::too_many_arguments)]` [decided]

The jsonrpsee `#[rpc(server, namespace = "eth")]` macro on
`DebankEthExtApi::multi_call` generates a server function with 8 parameters
(`&self` + 7 RPC params), tripping clippy's `too_many_arguments` lint
(default threshold = 7).

The lint is on generated code, so we can't add a targeted `#[allow(...)]`
on the macro output. Tempo handles this with a prose comment; Arc adds the
explicit crate-level `#![allow(clippy::too_many_arguments)]` in `lib.rs`
with a comment naming the trigger.

The crate-wide scope is acceptable because `debank-rpc` is a single-purpose
crate; no other functions hit the threshold (`trace_many` has 5 args).

## 2026-05-14 — Task 8 (RPC registration)

### D14. Append-only registration in `ArcAddOns::launch_add_ons_with` closure [decided]

Followed Karpathy "Surgical Changes": Arc's existing closure accesses
`container.modules` directly (not destructuring `container`). Instead of refactoring
to mirror Tempo's destructured form (`let RpcModuleContainer { modules, registry, .. } = container;`),
appended `let eth_api = container.registry.eth_api().clone();` after the existing
`arc_rpc.enabled` block and used the longhand `container.modules.*` for the merge calls.
Diff is +9 lines (registration block) + 1 line (import) + 1 line (Cargo.toml dep), no
restructuring of existing code.

**Verification:** `cargo check --workspace` clean. Pre-existing unused-import errors
in `arc-consensus-types` (from initial open-source commit `3e2f9f4`) surface under
`cargo clippy ... -D warnings`, but they are unrelated to this task.

### D15. `debank-rpc` declared as workspace-level dep [decided]

Initially added in Task 8 as a direct path dep in `evm-node/Cargo.toml`:
```toml
debank-rpc = { path = "../debank-rpc" }
```

Code-quality review pointed out Arc's convention: production crate
cross-deps are declared in root `[workspace.dependencies]` and consumers
reference via `workspace = true`. Test/e2e crates may use raw path style,
but production crates follow the workspace pattern.

Refactored:
- Added `debank-rpc = { version = "0.0.1", path = "crates/debank-rpc" }` to
  root `Cargo.toml` workspace deps
- Changed `evm-node/Cargo.toml` line 31 to `debank-rpc.workspace = true`

This matches `arc-evm-node`, `arc-eth-engine`, etc.
