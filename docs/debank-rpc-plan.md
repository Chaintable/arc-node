# Arc Node DeBank Custom RPC Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Port DeBank custom RPCs (`pre_traceMany`, `eth_multiCall`, `trace_debankBlock`) from Tempo's `debank-rpc` crate to `arc-node`, enabling the DeBank generic-node data pipeline on Arc.

**Architecture:** Mega-reth pattern — create an isolated `crates/debank-rpc/` crate that depends on reth/revm public APIs only, never modifying reth-core. Register the three RPC handlers via `ArcAddOns::launch_add_ons_with` closure in `crates/evm-node/src/node.rs`. Reuse Tempo's implementation verbatim where possible; strip Tempo-specific code paths (AA tx unpacking, fee_token handler, revert-tx fee log reconstruction) since Arc uses stock Ethereum semantics; adapt revm 36 → revm 34 API regressions where field shapes diverge.

**Tech Stack:** Rust edition 2024, reth `v1.11.3` tag, revm `34.0.0`, `revm-context-interface 14.0.0`, `revm-inspectors 0.34.2`, alloy `1.6.3`, `alloy-evm 0.27.2`, jsonrpsee `0.26.0`.

**Reference repo:** `/Users/lihe/code/task_tempo/crates/debank-rpc/` (Tempo's implementation, branch `debank`). Tempo design doc: `task_tempo/docs/debank-rpc-design.md`. Generic-node doc: `task_tempo/docs/generic-node.md`.

**Target repo:** `/Users/lihe/ghorg/chaintable/arc-node` (Chaintable fork of `circlefin/arc-node`). Base branch: `debank`. Feature branch: `feature/debank_rpc`.

---

## Critical API differences: revm 36 → revm 34

`ExecutionResult` in revm 34 (`revm-context-interface 14.0.0`):

```rust
pub enum ExecutionResult<HaltReasonTy = HaltReason> {
    Success {
        reason: SuccessReason,
        gas_used: u64,
        gas_refunded: u64,
        logs: Vec<Log>,
        output: Output,
    },
    Revert { gas_used: u64, output: Bytes },        // NO logs field
    Halt   { reason: HaltReasonTy, gas_used: u64 }, // NO logs field
}
```

Tempo (revm 36) uses `gas: ResultGas` instead — `gas.tx_gas_used()` returns u64. Port back to direct `gas_used: u64` field access.

Differences to apply during port:

| Tempo (revm 36) | Arc (revm 34) |
|---|---|
| `ExecutionResult::Success { gas, logs, output, .. }` then `gas.tx_gas_used()` | `ExecutionResult::Success { gas_used, logs, output, .. }` direct field |
| `ExecutionResult::Revert { gas, output, .. }` then `gas.tx_gas_used()` | `ExecutionResult::Revert { gas_used, output, .. }` |
| `ExecutionResult::Halt { reason, gas, .. }` then `gas.tx_gas_used()` | `ExecutionResult::Halt { reason, gas_used, .. }` |
| Revert/Halt include `logs` field | Revert/Halt have NO `logs` field — strip any logs extraction |
| `Output::into_data()` returns `Bytes` | Same — no change |

## Workspace clippy guard

`arc-node/Cargo.toml` declares `unwrap_used = "deny"`. Any `unwrap()` in ported code must be replaced with `expect("…")`, `.unwrap_or_default()`, or `?`. Tempo's unit tests use `unwrap()` freely; rewrite them.

## Tempo-specific code to STRIP

Listed per file in the corresponding Phase 2 tasks. Summary:

| Tempo behavior | Reason it doesn't apply to Arc | Action |
|---|---|---|
| `TempoEvm` handler-level fee flow (TIP-20 storage, fee Transfer log) | Arc uses stock EIP-1559 fees in USDC wei; no handler-level fee logic | `eth_api.inspect()` produces complete `ResultAndState` already — no post-processing needed |
| Revert-tx fee log reconstruction via `serde_json::from_value::<Vec<Log>>(receipt.logs)` | Arc has no handler-emitted fee logs; revert tx receipts have only EVM-captured logs | Remove the receipt-replay branch entirely |
| AA tx (type=0x76) `to_addr` / `input` unpacking from receipt | Arc has no AA tx type | Use `tx.to()` / `tx.input()` directly from the standard alloy `Transaction` trait |
| AA root-trace classification fix (CallTraceArena `success` unreliable) | Arc has no AA wrapper traces | Use `CallTraceNode.trace.success` directly; final classification still governed by `receipt.status` |
| `exclude_precompile_calls=true` discussion re: TIP-20 / FeeManager | Arc's precompiles at `0x1800...` segment, but they're injected via reth's `set_precompile_lookup` like Tempo's — same warm_addresses behavior; setting stays `true` | No code change; document in plan only |

## Native token sentinel (`0xeeee…`) behavior

Per user instruction: keep Tempo's behavior verbatim. `balanceOf(addr)` returns `state.basic_ref(addr).balance` (equivalent to `eth_getBalance`). On Arc, this surfaces the user's native USDC balance (18-dec). `name()`/`symbol()` return `"ETH"`, `decimals()` returns `18`, `totalSupply()` returns `1`. Callers needing real USDC metadata should use the real USDC contract `0x3600000000000000000000000000000000000000`.

---

# Phase 1 — `pre_traceMany` + `eth_multiCall`

(mirrors Tempo commit `2b16aeca1`)

### Task 1: Create feature branch

**Files:** none

- [ ] **Step 1: Verify clean state**

```bash
cd /Users/lihe/ghorg/chaintable/arc-node
git status
git branch --show-current
```

Expected: working tree clean, branch `debank`.

- [ ] **Step 2: Create feature branch**

```bash
git checkout -b feature/debank_rpc
```

Expected: switched to new branch `feature/debank_rpc`.

---

### Task 2: Create crate skeleton

**Files:**
- Create: `crates/debank-rpc/Cargo.toml`
- Create: `crates/debank-rpc/src/lib.rs`

- [ ] **Step 1: Create directory**

```bash
cd /Users/lihe/ghorg/chaintable/arc-node
mkdir -p crates/debank-rpc/src
```

- [ ] **Step 2: Write Cargo.toml**

Create `crates/debank-rpc/Cargo.toml` with:

```toml
[package]
name = "debank-rpc"

version.workspace = true
edition.workspace = true
license.workspace = true
rust-version.workspace = true
publish.workspace = true

[lints]
workspace = true

[dependencies]
# RPC
jsonrpsee = { workspace = true, features = ["server", "macros"] }

# Alloy
alloy-consensus = { workspace = true }
alloy-eips = { workspace = true }
alloy-genesis = { workspace = true }
alloy-network = { workspace = true }
alloy-primitives = { workspace = true }
alloy-rlp = { workspace = true }
alloy-rpc-types-eth = { workspace = true }
alloy-rpc-types-trace = { workspace = true }
alloy-sol-types = { workspace = true }

# Reth
reth-chainspec = { workspace = true }
reth-evm = { workspace = true }
reth-primitives-traits = { workspace = true }
reth-provider = { workspace = true }
reth-revm = { workspace = true }
reth-rpc-convert = { workspace = true }
reth-rpc-eth-api = { workspace = true }
reth-rpc-eth-types = { workspace = true }

# Revm
revm = { workspace = true }
revm-inspectors = { workspace = true }

# Serde
serde = { workspace = true }

# Crypto
md-5 = "0.10"
sha1 = "0.10"

# Misc
async-trait = { workspace = true }
futures = { workspace = true }
serde_json = { workspace = true }

[dev-dependencies]
```

- [ ] **Step 3: Verify workspace deps exist in arc-node**

For each `workspace = true` entry above, confirm it's declared in `/Users/lihe/ghorg/chaintable/arc-node/Cargo.toml`:

```bash
cd /Users/lihe/ghorg/chaintable/arc-node
for dep in jsonrpsee alloy-consensus alloy-eips alloy-genesis alloy-network alloy-primitives \
  alloy-rlp alloy-rpc-types-eth alloy-rpc-types-trace alloy-sol-types \
  reth-chainspec reth-evm reth-primitives-traits reth-provider reth-revm \
  reth-rpc-convert reth-rpc-eth-api reth-rpc-eth-types revm revm-inspectors \
  serde async-trait futures serde_json; do
  grep -q "^$dep " Cargo.toml || echo "MISSING: $dep"
done
```

Expected: any deps printed as MISSING must be added to root `Cargo.toml` `[workspace.dependencies]` using the version already pinned for adjacent reth-* / alloy-* crates. Most should exist; `reth-rpc-convert` and `reth-rpc-eth-types` may need adding.

- [ ] **Step 4: Add any missing workspace deps**

For each missing dep, add a line to `/Users/lihe/ghorg/chaintable/arc-node/Cargo.toml` `[workspace.dependencies]` section. Reth-prefixed deps go with existing reth lines using the same `tag = "v1.11.3"` form. Example for `reth-rpc-convert`:

```toml
reth-rpc-convert = { git = "https://github.com/paradigmxyz/reth", tag = "v1.11.3" }
```

For `reth-rpc-eth-types`:

```toml
reth-rpc-eth-types = { git = "https://github.com/paradigmxyz/reth", tag = "v1.11.3" }
```

For other potentially missing crypto deps (`md-5`, `sha1`) — they're direct, not workspace; no change to root.

- [ ] **Step 5: Write Phase 1 lib.rs (Phase 2 modules added in later tasks)**

Create `crates/debank-rpc/src/lib.rs`:

```rust
//! DeBank custom RPC extensions for Arc.
//!
//! Provides `pre_traceMany` and `eth_multiCall` (Phase 1).
//! `trace_debankBlock` is added in Phase 2.

pub mod erc20_handle;
pub mod multi_call;
pub mod pre;
pub mod types;

// Re-export key types for registration
pub use multi_call::DebankEthExt;
pub use pre::PreApi;
pub use types::{MultiCallResp, PreResult};

use alloy_eips::BlockId;
use alloy_rpc_types_eth::{BlockOverrides, state::StateOverride};
use jsonrpsee::core::RpcResult;

/// RPC trait for `pre` namespace.
#[jsonrpsee::proc_macros::rpc(server, namespace = "pre")]
pub trait DebankPreApi<TxReq> {
    /// Execute multiple transactions sequentially, returning execution traces for each.
    #[method(name = "traceMany")]
    async fn trace_many(
        &self,
        transactions: Vec<TxReq>,
        block_id: Option<BlockId>,
        state_overrides: Option<StateOverride>,
        block_overrides: Option<Box<BlockOverrides>>,
    ) -> RpcResult<Vec<PreResult>>;
}

/// RPC trait extending `eth` namespace with `multiCall`.
#[jsonrpsee::proc_macros::rpc(server, namespace = "eth")]
pub trait DebankEthExtApi<TxReq> {
    /// Execute multiple calls in a single request, returning results for each.
    #[method(name = "multiCall")]
    async fn multi_call(
        &self,
        requests: Vec<TxReq>,
        block_number: Option<BlockId>,
        fast_fail: Option<bool>,
        use_parallel: Option<bool>,
        disable_cache: Option<bool>,
        state_overrides: Option<StateOverride>,
        block_overrides: Option<Box<BlockOverrides>>,
    ) -> RpcResult<MultiCallResp>;
}
```

- [ ] **Step 6: Create empty module stubs for the Phase 1 files**

```bash
cd /Users/lihe/ghorg/chaintable/arc-node/crates/debank-rpc/src
touch types.rs erc20_handle.rs multi_call.rs pre.rs
```

Each starts empty; subsequent tasks fill them. Phase 2 files (`state_diff_db.rs`, `debank_trace.rs`, `trace_block.rs`) are created in Phase 2 tasks.

- [ ] **Step 7: Verify workspace picks up the new crate (will fail compile but should resolve)**

```bash
cd /Users/lihe/ghorg/chaintable/arc-node
cargo metadata --format-version=1 --no-deps -q 2>&1 | python3 -c 'import sys, json; data = json.load(sys.stdin); print([p["name"] for p in data["packages"] if p["name"] == "debank-rpc"])'
```

Expected: `['debank-rpc']`. Confirms `members = ["crates/*"]` auto-included it.

---

### Task 3: Port `types.rs`

**Files:**
- Modify: `crates/debank-rpc/src/types.rs`

- [ ] **Step 1: Copy verbatim from Tempo**

```bash
cp /Users/lihe/code/task_tempo/crates/debank-rpc/src/types.rs \
   /Users/lihe/ghorg/chaintable/arc-node/crates/debank-rpc/src/types.rs
```

- [ ] **Step 2: Quick check — any tempo-specific symbols?**

```bash
grep -niE 'tempo|tip[0-9]|fee_token|fee_payer|aa_tx|subblock' \
  /Users/lihe/ghorg/chaintable/arc-node/crates/debank-rpc/src/types.rs
```

Expected: zero matches. `types.rs` is pure DTO definitions (`PreResult`, `MultiCallResp`, `SingleCallResult`, etc.) with no Tempo specifics.

- [ ] **Step 3: cargo check the file alone**

```bash
cd /Users/lihe/ghorg/chaintable/arc-node
cargo check -p debank-rpc 2>&1 | tail -30
```

Expected: compile errors will still appear (other modules empty), but `types.rs` itself should not produce errors. If it does, fix imports.

---

### Task 4: Port `erc20_handle.rs`

**Files:**
- Modify: `crates/debank-rpc/src/erc20_handle.rs`

- [ ] **Step 1: Copy from Tempo**

```bash
cp /Users/lihe/code/task_tempo/crates/debank-rpc/src/erc20_handle.rs \
   /Users/lihe/ghorg/chaintable/arc-node/crates/debank-rpc/src/erc20_handle.rs
```

- [ ] **Step 2: Replace `unwrap()` in test code with `expect()`**

Workspace lint `unwrap_used = "deny"` covers test code unless `#[allow(clippy::unwrap_used)]` is added. Simpler: replace each `.unwrap()` call with `.expect("...")`. Find and fix:

```bash
cd /Users/lihe/ghorg/chaintable/arc-node
grep -n 'unwrap()' crates/debank-rpc/src/erc20_handle.rs
```

Replace each occurrence (3 expected, in `name_returns_eth` / `symbol_returns_eth` decoding) with `expect("decode result")`. Example diff:

```rust
// before
let decoded = sol_data::String::abi_decode(&res.result).unwrap();
// after
let decoded = sol_data::String::abi_decode(&res.result).expect("abi_decode");
```

- [ ] **Step 3: cargo check**

```bash
cd /Users/lihe/ghorg/chaintable/arc-node
cargo check -p debank-rpc 2>&1 | tail -30
```

Expected: `erc20_handle.rs` itself produces no errors. (Other empty modules and `lib.rs` re-exports still cause errors.)

---

### Task 5: Port `multi_call.rs` with revm 34 adaptation

**Files:**
- Modify: `crates/debank-rpc/src/multi_call.rs`

- [ ] **Step 1: Copy from Tempo**

```bash
cp /Users/lihe/code/task_tempo/crates/debank-rpc/src/multi_call.rs \
   /Users/lihe/ghorg/chaintable/arc-node/crates/debank-rpc/src/multi_call.rs
```

- [ ] **Step 2: Adapt `ExecutionResult` destructuring for revm 34**

Find the `match execute_result.result` block and rewrite the three arms. The Tempo version (lines 104–130) uses `gas: ResultGas` then `gas.tx_gas_used()`; rewrite to direct `gas_used: u64`:

```rust
let mut res = match execute_result.result {
    ExecutionResult::Success { output, gas_used, .. } => SingleCallResult {
        code: MultiCallErrorCode::Success as i32,
        err: String::new(),
        from_cache: false,
        result: output.into_data(),
        gas_used: gas_used as i64,
        time_cost: 0.0,
    },
    ExecutionResult::Revert { output, gas_used, .. } => SingleCallResult {
        code: MultiCallErrorCode::EVMReverted as i32,
        err: alloy_sol_types::decode_revert_reason(&output)
            .unwrap_or_else(|| "Reason Unknown".to_string()),
        from_cache: false,
        result: alloy_primitives::Bytes::default(),
        gas_used: gas_used as i64,
        time_cost: 0.0,
    },
    ExecutionResult::Halt { reason, gas_used, .. } => SingleCallResult {
        code: MultiCallErrorCode::EVMCancelled as i32,
        err: format!("Halted: {reason:?}"),
        from_cache: false,
        result: alloy_primitives::Bytes::default(),
        gas_used: gas_used as i64,
        time_cost: 0.0,
    },
};
```

- [ ] **Step 3: cargo check**

```bash
cd /Users/lihe/ghorg/chaintable/arc-node
cargo check -p debank-rpc 2>&1 | tail -40
```

Expected: only `pre.rs` / `debank_trace.rs` / `trace_block.rs` / `state_diff_db.rs` related errors remain (those modules still empty). If `multi_call.rs` errors persist, read them and adjust — most likely additional missing trait imports or generic-bound mismatches.

---

### Task 6: Port `pre.rs` with revm 34 adaptation

**Files:**
- Modify: `crates/debank-rpc/src/pre.rs`

- [ ] **Step 1: Copy from Tempo**

```bash
cp /Users/lihe/code/task_tempo/crates/debank-rpc/src/pre.rs \
   /Users/lihe/ghorg/chaintable/arc-node/crates/debank-rpc/src/pre.rs
```

- [ ] **Step 2: Adapt `ExecutionResult::Success` destructure**

In `trace_transaction`, find:

```rust
revm::context::result::ExecutionResult::Success {
    gas,
    logs: exec_logs,
    ..
} => {
    ...
    gas_used: gas.tx_gas_used(),
}
```

Change to:

```rust
revm::context::result::ExecutionResult::Success {
    gas_used,
    logs: exec_logs,
    ..
} => {
    ...
    gas_used,
}
```

The `Halt` and `Revert` arms in Tempo's `pre.rs` only read `reason` / no fields — those are unchanged.

- [ ] **Step 3: cargo check**

```bash
cd /Users/lihe/ghorg/chaintable/arc-node
cargo check -p debank-rpc 2>&1 | tail -40
```

Expected: `pre.rs` compiles. Remaining errors should only be from empty `debank_trace.rs` / `trace_block.rs` / `state_diff_db.rs` and the `lib.rs` re-exports referencing them.

---

### Task 7: Run Phase 1 unit tests

**Files:** none

- [ ] **Step 1: cargo test on debank-rpc**

```bash
cd /Users/lihe/ghorg/chaintable/arc-node
cargo test -p debank-rpc 2>&1 | tail -30
```

Expected: 12 tests from `erc20_handle.rs` pass (`balance_of_returns_account_balance`, `balance_of_unknown_account_returns_zero`, `balance_of_input_too_short`, `balance_of_extra_trailing_bytes_ok`, `total_supply_returns_one`, `decimals_returns_18`, `name_returns_eth`, `symbol_returns_eth`, `none_input_returns_error`, `short_input_less_than_4_bytes`, `unknown_selector_returns_not_found`, plus any I may have missed — verify count matches Tempo source).

---

### Task 8: Register `PreApi` + `DebankEthExt` in `ArcAddOns::launch_add_ons`

**Files:**
- Modify: `crates/evm-node/src/node.rs:380-418`
- Modify: `crates/evm-node/Cargo.toml`

- [ ] **Step 1: Add dependency on debank-rpc**

In `crates/evm-node/Cargo.toml`, under `[dependencies]`, add:

```toml
debank-rpc = { path = "../debank-rpc" }
```

- [ ] **Step 2: Add imports at top of `crates/evm-node/src/node.rs`**

Locate the existing `use` block (around the top of the file) and add:

```rust
use debank_rpc::{DebankEthExt, DebankEthExtApiServer, DebankPreApiServer, PreApi};
```

- [ ] **Step 3: Modify the `launch_add_ons_with` closure**

In `crates/evm-node/src/node.rs:396-417`, after the existing `if self.arc_rpc.enabled { ... }` block and before `Ok(())`, append:

```rust
                // DeBank custom RPCs
                let eth_api = container.registry.eth_api().clone();
                let pre_api = PreApi::new(eth_api.clone());
                container.modules.merge_configured(pre_api.into_rpc())?;
                let debank_eth_ext = DebankEthExt::new(eth_api);
                container
                    .modules
                    .merge_if_module_configured(RethRpcModule::Eth, debank_eth_ext.into_rpc())?;
```

Note: `RethRpcModule` is already imported at the top of `node.rs` (used by `Flashbots` / `Eth` registrations); reuse the existing import.

- [ ] **Step 4: cargo check the workspace**

```bash
cd /Users/lihe/ghorg/chaintable/arc-node
cargo check --workspace 2>&1 | tail -40
```

Expected: clean compile. If trait-bound errors appear for `PreApi<Eth>: DebankPreApiServer<...>` or `DebankEthExt<Eth>: DebankEthExtApiServer<...>`, read the bounds in `pre.rs` (`Eth: EthApiTypes + EthTransactions + TraceExt + 'static`) and `multi_call.rs` (`Eth: EthApiTypes + EthCall + 'static, RpcTxReq<Eth::NetworkTypes>: AsRef<TransactionRequest>`) and verify `ArcEthApiBuilder::EthApi` satisfies them. If `AsRef<TransactionRequest>` is missing on `RpcTxReq<Ethereum>`, add the impl in `pre.rs`/`multi_call.rs` call site (Arc uses the default `Ethereum` network type which should already have it).

---

### Task 9: cargo build to catch link-time issues

**Files:** none

- [ ] **Step 1: Full debug build**

```bash
cd /Users/lihe/ghorg/chaintable/arc-node
cargo build --workspace 2>&1 | tail -20
```

Expected: success.

---

### Task 10: Commit Phase 1

**Files:** none

- [ ] **Step 1: Stage and commit**

```bash
cd /Users/lihe/ghorg/chaintable/arc-node
git add crates/debank-rpc crates/evm-node/Cargo.toml crates/evm-node/src/node.rs Cargo.toml Cargo.lock
git status
git commit -m "feat: add DeBank custom RPCs (pre_traceMany + eth_multiCall)

Ports debank-rpc crate from Tempo, providing two JSON-RPC endpoints:

- pre_traceMany: sequential tx pre-execution with parity traces + logs
- eth_multiCall: batch eth_call with native-token sentinel (0xeeee...)

Adapted from Tempo's revm 36 implementation to Arc's revm 34 by
inlining gas_used field access (no ResultGas wrapper). Registered via
ArcAddOns::launch_add_ons_with closure; no reth-core modification.
"
```

---

# Phase 2 — `trace_debankBlock`

(mirrors Tempo commit `11f91e72f`, with Tempo-specific paths stripped)

### Task 11: Port `state_diff_db.rs`

**Files:**
- Modify: `crates/debank-rpc/src/state_diff_db.rs`

- [ ] **Step 1: Copy from Tempo**

```bash
cp /Users/lihe/code/task_tempo/crates/debank-rpc/src/state_diff_db.rs \
   /Users/lihe/ghorg/chaintable/arc-node/crates/debank-rpc/src/state_diff_db.rs
```

- [ ] **Step 2: Scan for tempo-specific symbols**

```bash
grep -niE 'tempo|tip[0-9]|fee_token|fee_payer|aa_tx|subblock' \
  /Users/lihe/ghorg/chaintable/arc-node/crates/debank-rpc/src/state_diff_db.rs
```

Expected: zero matches. `state_diff_db.rs` is generic state-trace database wrapper, no Tempo coupling.

- [ ] **Step 3: Add module declaration to lib.rs**

In `crates/debank-rpc/src/lib.rs`, add to the `pub mod` block (keep alphabetical with existing modules):

```rust
pub mod state_diff_db;
```

- [ ] **Step 4: cargo check**

```bash
cd /Users/lihe/ghorg/chaintable/arc-node
cargo check -p debank-rpc 2>&1 | tail -20
```

Expected: clean compile. `state_diff_db.rs` is self-contained and doesn't depend on the unwritten Phase 2 files.

---

### Task 12: Port `debank_trace.rs` with Tempo specifics stripped

**Files:**
- Modify: `crates/debank-rpc/src/debank_trace.rs`

- [ ] **Step 1: Copy from Tempo**

```bash
cp /Users/lihe/code/task_tempo/crates/debank-rpc/src/debank_trace.rs \
   /Users/lihe/ghorg/chaintable/arc-node/crates/debank-rpc/src/debank_trace.rs
```

- [ ] **Step 2: Locate Tempo-specific items in this file**

```bash
grep -niE 'tempo|tip[0-9]|fee_token|fee_payer|aa_tx|0x76|TempoCall|TempoTxEnvelope|fee_payer_signature' \
  /Users/lihe/ghorg/chaintable/arc-node/crates/debank-rpc/src/debank_trace.rs
```

Expected match clusters (Tempo source line refs for orientation):
- `pub struct DebankTransaction { ... }` (~line 83) — definition with Tempo-only fields
- `pub calls: Option<Vec<TempoCall>>` (~line 109)
- `pub fee_token: Option<Address>` (~line 112)
- `pub fee_payer_signature: Option<serde_json::Value>` (~line 130)
- `pub struct TempoCall { ... }` (~line 144)
- Test data builders setting `fee_token: Some(...)`, `fee_payer_signature`, `calls: Some(vec![TempoCall { ... }])` (~lines 843-900)

(Population logic — the `is_aa` branch — lives in `trace_block.rs` and is handled in Task 13.)

- [ ] **Step 3: Apply stripping (schema-preserving strategy)**

Goal: keep JSON output shape identical to Tempo's so the same `background-tracer` and Go pipeline can consume Arc output. Only stop populating Tempo-specific fields. Concretely for `debank_trace.rs`:

A. **Keep `DebankTransaction` schema as-is.** Don't remove `calls` / `fee_token` / `fee_payer_signature` fields. Arc-produced JSON will have them as `null` / empty, which the pipeline consumer already tolerates per `task_tempo/docs/generic-node.md` "pipeline Go 版的字段兼容性" section.

B. **Decide on `TempoCall` naming.** Recommend keeping the name `TempoCall` unchanged — it appears only as `Option<Vec<TempoCall>>` and the field will always be `None` on Arc, so the type is effectively vestigial. Cosmetic rename is not worth the test-update churn. If you prefer to rename, choose `BatchCall` and update all callers.

C. **Update unit tests** (lines ~843-1005). The tests build sample `DebankTransaction` instances. Two safe options:
   - Delete the AA-flavored test cases entirely (they're schema-roundtrip tests, not logic tests)
   - Keep them but populate Tempo-only fields with placeholder values just to exercise serialization — fine because these are pure-data tests, not runtime behavior on Arc

The simpler test cases that assert `assert!(json.get("fee_payer_signature").is_none())` on a standard tx remain valid and useful on Arc — keep those.

- [ ] **Step 4: Add module declaration and re-export to lib.rs**

In `crates/debank-rpc/src/lib.rs`, add to the `pub mod` block:

```rust
pub mod debank_trace;
```

And in the re-exports block:

```rust
pub use debank_trace::DebankOutPut;
```

- [ ] **Step 5: cargo check**

```bash
cd /Users/lihe/ghorg/chaintable/arc-node
cargo check -p debank-rpc 2>&1 | tail -30
```

Fix any compile errors that surface — most likely:
- Missing `Transaction` trait import (use `alloy_consensus::Transaction`)
- Field access on `ReceiptResponse` that doesn't exist on Arc — use `Eth::NetworkTypes` associated types
- `ExecutionResult` destructure for `Halt`/`Revert` accessing `logs` (revm 34 has no `logs` on these arms) — remove those reads

---

### Task 13: Port `trace_block.rs` with Tempo specifics stripped

**Files:**
- Modify: `crates/debank-rpc/src/trace_block.rs`

- [ ] **Step 1: Copy from Tempo**

```bash
cp /Users/lihe/code/task_tempo/crates/debank-rpc/src/trace_block.rs \
   /Users/lihe/ghorg/chaintable/arc-node/crates/debank-rpc/src/trace_block.rs
```

- [ ] **Step 2: Identify and strip Tempo-specific code paths**

```bash
grep -niE 'tempo|tip[0-9]|fee_token|fee_payer|aa_tx|0x76|TempoEvm|is_aa|fee.log|serde_json::from_value' \
  /Users/lihe/ghorg/chaintable/arc-node/crates/debank-rpc/src/trace_block.rs
```

Expected match clusters (Tempo source line refs):

1. **AA tx detection + field population** (~lines 152-220):
   ```rust
   let is_aa = tx_json.get("type").and_then(|t| t.as_str()) == Some("0x76");
   ...
   if is_aa {
       dtx.fee_token = tx_json.get("fee_token").and_then(...);
       dtx.fee_payer_signature = tx_json.get("fee_payer_signature").and_then(...);
       dtx.calls = tx_json.get("calls").and_then(...);
       // re-derive to_addr from receipt.to (since tx.to is None for AA envelopes)
       dtx.to_addr = receipt.to();
       // re-derive input from receipt-side data
   }
   ```
   **Remove**: drop the `let is_aa = ...;` and the `if is_aa { ... }` branch entirely. Standard tx path already sets `to_addr` from `tx.to()` and `input` from `tx.input()`.

2. **Revert-tx fee log reconstruction** (Tempo: `task_tempo/docs/generic-node.md` "Revert tx 的 fee log 获取"). Likely a `serde_json::from_value::<Vec<alloy_rpc_types_eth::Log>>(receipt_json.get("logs").cloned())` call inside the revert-handling branch, intended to re-attach handler-level fee logs that aren't in `ExecutionResult::Revert`. Remove entirely — Arc has no handler-emitted fee logs.

3. **TempoEvm handler / fee path commentary and any code reading TempoReceipt-extension fields**. Remove comments referencing `TempoEvmHandler`, TIP-20 fee Transfer logs, FeeManager addresses, fee log de-/re-attachment. The standard `eth_api.inspect()` produces a complete `ResultAndState`; no post-processing is needed.

4. **Genesis special-case for system tx** (~line 278 says `// No empty block shortcut: Tempo has a system tx in every block`). Verify Arc's behavior on genesis — Arc's chainspec has a system genesis tx pattern too (`task_arc/ARC_PLAYBOOK.md` mentions block 0 returns synthetic data on Tempo). For now, keep the Tempo genesis-handling logic; if Arc behaves differently, this surfaces as a runtime issue during later validation, not a compile issue.

- [ ] **Step 3: Adapt revm 34 ExecutionResult**

Search for any `ExecutionResult::Halt { gas, .. }` or `ExecutionResult::Revert { gas, output, logs }` style destructuring. Convert to revm 34 form: `gas_used: u64` field; no `logs` on Halt/Revert. Drop any code reading logs from Halt/Revert.

- [ ] **Step 4: Verify `exclude_precompile_calls=true` remains appropriate**

The `TracingInspectorConfig::default_parity().set_exclude_precompile_calls(true)` (or equivalent) call should stay `true`. Arc's custom precompiles at `0x1800...` segment are injected via reth's `set_precompile_lookup` like Tempo's TIP-20; they don't appear in `warm_addresses()`. Add a one-line comment if the original Tempo comment is too Tempo-specific.

- [ ] **Step 5: Add module declaration, re-export, and trait to lib.rs**

In `crates/debank-rpc/src/lib.rs`, add the module declaration:

```rust
pub mod trace_block;
```

The re-export:

```rust
pub use trace_block::DebankTraceBlock;
```

And after the `DebankEthExtApi` trait block, append the `DebankTraceApi` trait:

```rust
/// RPC trait for `trace` namespace — `debankBlock`.
#[jsonrpsee::proc_macros::rpc(server, namespace = "trace")]
pub trait DebankTraceApi {
    /// Returns DeBank trace/event/state-diff output for the given block.
    #[method(name = "debankBlock")]
    async fn trace_debank_block(&self, block_id: BlockId) -> RpcResult<DebankOutPut>;
}
```

- [ ] **Step 6: cargo check**

```bash
cd /Users/lihe/ghorg/chaintable/arc-node
cargo check -p debank-rpc 2>&1 | tail -50
```

Iterate on compile errors. Common issues:
- `block.body().transactions()` API differences between Tempo's primitives and Arc's `EthPrimitives`
- `RecoveredBlock<...>` generic associated type mismatch — Arc uses `EthPrimitives`, Tempo uses `TempoPrimitives`
- `Receipt` type — Arc uses standard `alloy_consensus::ReceiptEnvelope`, no Tempo extensions

---

### Task 14: Register `trace_debankBlock` in `ArcAddOns::launch_add_ons`

**Files:**
- Modify: `crates/evm-node/src/node.rs` (the same `launch_add_ons_with` closure modified in Task 8)

- [ ] **Step 1: Extend imports**

Update the existing import line:

```rust
use debank_rpc::{
    DebankEthExt, DebankEthExtApiServer, DebankPreApiServer, DebankTraceApiServer,
    DebankTraceBlock, PreApi,
};
```

- [ ] **Step 2: Append registration call**

In the `launch_add_ons_with` closure, after the `DebankEthExt` registration, add:

```rust
                let debank_trace = DebankTraceBlock::new(container.registry.eth_api().clone());
                container.modules.merge_configured(debank_trace.into_rpc())?;
```

(Use a fresh `clone()` since `eth_api` was already moved into `DebankEthExt::new`.)

- [ ] **Step 3: cargo check workspace**

```bash
cd /Users/lihe/ghorg/chaintable/arc-node
cargo check --workspace 2>&1 | tail -30
```

Expected: clean compile. If `DebankTraceBlock<Eth>: DebankTraceApiServer` trait-bound fails, inspect the bounds on `DebankTraceBlock` in `trace_block.rs` and verify Arc's EthApi satisfies them (commonly `EthApiTypes + LoadReceipt + LoadBlock + LoadState + TraceExt + EthTransactions + EthBlocks + SpawnBlocking`).

---

### Task 15: Build full debug artifact

**Files:** none

- [ ] **Step 1: cargo build workspace**

```bash
cd /Users/lihe/ghorg/chaintable/arc-node
cargo build --workspace 2>&1 | tail -10
```

Expected: success.

- [ ] **Step 2: Run all unit tests**

```bash
cd /Users/lihe/ghorg/chaintable/arc-node
cargo test -p debank-rpc 2>&1 | tail -20
```

Expected: `erc20_handle.rs` tests pass; other modules have no tests (matches Tempo's structure — integration testing was done against running node, not in-process).

---

### Task 16: Commit Phase 2

**Files:** none

- [ ] **Step 1: Stage and commit**

```bash
cd /Users/lihe/ghorg/chaintable/arc-node
git add crates/debank-rpc crates/evm-node/src/node.rs
git status
git commit -m "feat: implement trace_debankBlock RPC for generic node pipeline

Extends debank-rpc crate with trace_debankBlock under the trace namespace,
providing block-level traces + events + state diff in the format consumed
by background-tracer sidecar (S3 + Kafka).

Arc port differences vs Tempo source:
- removed AA tx (0x76) unpacking — Arc has no AA tx type
- removed revert-tx fee log reconstruction — Arc has no handler-level fees
- removed TempoEvm handler path commentary — Arc uses stock Ethereum flow
- adapted ExecutionResult destructure for revm 34 (gas_used: u64 field)
"
```

---

# Phase 3 — push + PR

### Task 17: Push branch and open PR

**Files:** none

- [ ] **Step 1: Push branch**

```bash
cd /Users/lihe/ghorg/chaintable/arc-node
git push -u origin feature/debank_rpc
```

- [ ] **Step 2: Confirm CI workflow `.github/workflows/build.debank.yml` triggers**

```bash
gh run list --branch feature/debank_rpc --limit 5
```

Expected: at least one run started/in progress. CI builds amd64+arm64 docker image and pushes to ECR `blockchain/arc-x`.

- [ ] **Step 3: Open PR to `debank` base branch**

```bash
gh pr create --base debank --head feature/debank_rpc \
  --title "feat: add DeBank custom RPCs (pre_traceMany, eth_multiCall, trace_debankBlock)" \
  --body "$(cat <<'EOF'
## Summary

Adds three JSON-RPC endpoints to arc-node for the DeBank data pipeline:

| Method | Namespace | Purpose |
|---|---|---|
| `pre_traceMany` | `pre` | Sequential tx pre-execution returning parity traces + logs + gas |
| `eth_multiCall` | `eth` (extension) | Batch eth_call with native-token sentinel handling |
| `trace_debankBlock` | `trace` (extension) | Block-level traces + events + state diff for background-tracer |

Implementation ported from Tempo's `debank-rpc` crate (branch: `debank`), with Tempo-specific paths (AA tx, TempoEvm handler fees, revert-tx fee log reconstruction) stripped. New isolated crate `crates/debank-rpc/`; no reth-core modification. Registered via `ArcAddOns::launch_add_ons_with` closure.

### Version adaptation

- Tempo source: revm 36 (`ResultGas` wrapper) → Arc target: revm 34 (`gas_used: u64` field)
- Tempo source: revm 36 Revert/Halt carry `logs` → Arc target: revm 34 Revert/Halt have no `logs`

### Native token sentinel (`0xeeee…`)

`balanceOf(addr)` returns `eth_getBalance(addr)` value; metadata kept as Tempo defaults ("ETH"/18/totalSupply=1). Callers needing real USDC metadata should query the native USDC contract at `0x3600000000000000000000000000000000000000`.

## Test plan

- [x] `cargo check --workspace` clean
- [x] `cargo build --workspace` clean
- [x] `cargo test -p debank-rpc` (12 unit tests pass — erc20_handle sentinel logic)
- [ ] On-node verification against `chaindev-misc-g1` deferred to follow-up; image builds via existing CI workflow.

EOF
)"
```

- [ ] **Step 4: Print PR URL**

```bash
gh pr view --json url -q .url
```

Hand the URL back to the user.

---

## Out of scope (follow-up work)

- On-node validation against `chaindev-misc-g1` (deployment of new image + RPC test suite from `task_tempo/docs/test-plan-generic-node.md`)
- background-tracer sidecar integration / Kafka topics / S3 layout for ARC chain (`chain_id=5042002`)
- leafage-evm pipeline ingest configuration
- Performance benchmarking
