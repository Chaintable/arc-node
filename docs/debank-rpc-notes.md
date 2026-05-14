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

### Q1. Final list of `DebankTraceBlock` trait bounds against `ArcEthApi` [open]

Tempo's `DebankTraceBlock<Eth>` requires `Eth: EthApiTypes + LoadReceipt + LoadBlock + LoadState + TraceExt + EthTransactions + EthBlocks + SpawnBlocking`. Need to confirm Arc's `ArcEthApiBuilder::EthApi` satisfies all of these on reth v1.11.3.

**Resolution:** discovered during Task 14 `cargo check`. Document any missing trait + how it was sourced (e.g., from a deeper helpers module).

### Q2. AccessList field on `DebankTransaction` for Arc [open]

Tempo `DebankTransaction` has `pub access_list: ...` shared between AA tx and EIP-2930. Need to verify how Arc populates this for standard EIP-2930/1559 txs — same as Tempo's non-AA path, but worth confirming the alloy `Transaction` trait method name on the Arc-pinned alloy version.

**Resolution:** Task 12.

