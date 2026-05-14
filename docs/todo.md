# DeBank RPC — TODO

Open items, listed by priority/phase. Subagents append here during execution; user reviews between tasks.

## In-progress

(empty — populated by Task 1+ as work starts)

## Plan execution (see `docs/debank-rpc-plan.md`)

- [ ] Task 1 — Create `feature/debank_rpc` branch *(done out-of-band before subagent execution)*
- [ ] Task 2 — Create crate skeleton (`Cargo.toml` + Phase 1 `lib.rs`)
- [ ] Task 3 — Port `types.rs`
- [ ] Task 4 — Port `erc20_handle.rs`
- [ ] Task 5 — Port `multi_call.rs` with revm 34 adaptation
- [ ] Task 6 — Port `pre.rs` with revm 34 adaptation
- [ ] Task 7 — Run Phase 1 unit tests
- [ ] Task 8 — Register `PreApi` + `DebankEthExt` in `ArcAddOns::launch_add_ons`
- [ ] Task 9 — `cargo build --workspace` Phase 1 sanity
- [ ] Task 10 — Commit Phase 1
- [ ] Task 11 — Port `state_diff_db.rs`
- [ ] Task 12 — Port `debank_trace.rs` (schema-preserving Tempo strip)
- [ ] Task 13 — Port `trace_block.rs` (remove `is_aa`/revert-fee-log paths)
- [ ] Task 14 — Register `trace_debankBlock` in `ArcAddOns::launch_add_ons`
- [ ] Task 15 — `cargo build --workspace` Phase 2 sanity + unit tests
- [ ] Task 16 — Commit Phase 2
- [ ] Task 17 — Push branch + open PR to `debank`

## Follow-ups (out of plan scope)

- [ ] On-node verification against `chaindev-misc-g1` — deploy new image, run RPC test suite from `task_tempo/docs/test-plan-generic-node.md` adapted to Arc (chain id `5042002`)
- [ ] `background-tracer` sidecar integration for Arc: configure Kafka topics + S3 bucket layout (per `~/code/task_tempo/docs/generic-node.md` deployment section)
- [ ] `leafage-evm` pipeline ingestion config for Arc
- [ ] Performance benchmarks (target Tempo's 12ms / call baseline)
- [ ] Verify `exclude_precompile_calls=true` doesn't hide Arc's `0x1800...` custom precompile traces (see notes D6)
- [ ] Verify genesis handling on Arc — does Arc inject synthetic genesis txs? (see notes D8)

## Process / housekeeping

- [ ] Append to `docs/debank-rpc-notes.md` whenever a non-trivial decision is made during execution. Mark with date + status.
