# DeBank RPC — TODO

Open items, listed by priority/phase. Subagents append here during execution; user reviews between tasks.

## In-progress

(empty — populated by Task 1+ as work starts)

## Plan execution (see `docs/debank-rpc-plan.md`)

- [x] Task 1 — Create `feature/debank_rpc` branch *(done out-of-band before subagent execution)*
- [x] Task 2 — Create crate skeleton (`Cargo.toml` + Phase 1 `lib.rs`)
- [x] Task 3 — Port `types.rs`
- [x] Task 4 — Port `erc20_handle.rs`
- [x] Task 5 — Port `multi_call.rs` with revm 34 adaptation
- [x] Task 6 — Port `pre.rs` with revm 34 adaptation
- [x] Task 7 — Run Phase 1 unit tests
- [x] Task 8 — Register `PreApi` + `DebankEthExt` in `ArcAddOns::launch_add_ons`
- [x] Task 9 — `cargo build --workspace` Phase 1 sanity
- [x] Task 10 — Commit Phase 1
- [x] Task 11 — Port `state_diff_db.rs`
- [x] Task 12 — Port `debank_trace.rs` (schema-preserving Tempo strip)
- [x] Task 13 — Port `trace_block.rs` (remove `is_aa`/revert-fee-log paths)
- [x] Task 14 — Register `trace_debankBlock` in `ArcAddOns::launch_add_ons`
- [x] Task 15 — `cargo build --workspace` Phase 2 sanity + unit tests
- [x] Task 16 — Commit Phase 2
- [x] Task 17 — Push branch + open PR to `debank` (PR #1, https://github.com/Chaintable/arc-node/pull/1)

## Bug fixes (post-deployment)

- [x] D19 → D20: restored Tempo's receipt-log fallback in trace_block.rs (NCA precompile log capture)

## Follow-ups (out of plan scope)

- [ ] On-node verification against `chaindev-misc-g1` — deploy PR #1 image, execute `docs/debank-rpc-test-plan.md` (141 tests across 15 sections + 200-block batch regression)
- [ ] `background-tracer` sidecar integration for Arc: configure Kafka topics + S3 bucket layout (per `~/code/task_tempo/docs/generic-node.md` deployment section)
- [ ] `leafage-evm` pipeline ingestion config for Arc
- [ ] Performance benchmarks (target Tempo's 12ms / call baseline)
- [x] Set `exclude_precompile_calls=false` to match the canonical pipeline producer and Arc `debug_*` callTracer (see notes D6)
- [ ] Verify genesis handling on Arc — does Arc inject synthetic genesis txs? (see notes D8)

## Phase 1 polish (do before Task 10 commit)

- [x] Add `repository.workspace = true`, `readme.workspace = true`, `exclude.workspace = true` to `crates/debank-rpc/Cargo.toml [package]` for consistency with sibling crates (Task 2 code-quality Minor 2)
- [~] (Skipped, deferred) Move `md-5 = "0.10"` and `sha1 = "0.10"` to root workspace deps — only debank-rpc uses them, no benefit yet
- [x] D12: Removed `block_timestamp` from `TransactionInfo` literal in pre.rs (alloy-consensus 1.7.3 delta)
- [x] D13: Added explanatory comment + crate-level `#![allow(clippy::too_many_arguments)]` in lib.rs
- [x] D15: Convert debank-rpc dep to workspace-style (workspace.dependencies + `workspace = true`)
- [ ] Watch: if Arc bumps to alloy-consensus 2.x, revert D12 (add `block_timestamp` back to `TransactionInfo` literal in pre.rs)

## Process / housekeeping

- [x] D10: Apply crate-level `#![cfg_attr(test, allow(clippy::unwrap_used))]` to debank-rpc/src/lib.rs (done in Task 3 fix-up)
- [x] D11: Replace `unwrap()` in multi_call.rs:72 with `is_some_and` (Task 5 fix-up)
- [ ] During Tasks 6/12/13 ports: check Tempo source for production-code `unwrap()` patterns and rewrite each
- [ ] Append to `docs/debank-rpc-notes.md` whenever a non-trivial decision is made during execution. Mark with date + status.
