//! `trace_debankBlock` RPC implementation.
//!
//! Replays all transactions in a block, collecting DeBank-format traces, events,
//! and state diffs for consumption by background-tracer → S3/Kafka → leafage-evm.

use alloy_consensus::{transaction::TxHashRef, BlockHeader, Transaction};
use alloy_eips::BlockId;
use alloy_evm::block::{OnStateHook, StateChangeSource};
use alloy_primitives::{Address, Bytes, Log, B256};
use alloy_rpc_types_eth::Header;
use jsonrpsee::core::RpcResult;
use reth_chainspec::{EthChainSpec, EthereumHardforks};
use reth_evm::{execute::BlockExecutor, ConfigureEvm, Evm};
use reth_primitives_traits::BlockBody;
use reth_provider::ChainSpecProvider;
use reth_revm::{database::StateProviderDatabase, State};
use reth_rpc_eth_api::{
    helpers::{
        EthBlocks, EthTransactions, LoadBlock, LoadReceipt, LoadState, SpawnBlocking, TraceExt,
    },
    EthApiTypes,
};
use reth_rpc_eth_types::{cache::db::StateProviderTraitObjWrapper, EthApiError};
use revm::{bytecode::opcode::OpCode, database::InMemoryDB, DatabaseCommit};
use revm_inspectors::tracing::{OpcodeFilter, TracingInspector, TracingInspectorConfig};
use std::sync::{Arc, Mutex, PoisonError};

use crate::debank_trace::*;
use crate::event_inspector::ArcEventInspector;

#[derive(Clone, Debug, PartialEq, Eq)]
struct TxExecutionOutcome {
    success: bool,
    gas_used: u64,
    output: Option<Bytes>,
    created_address: Option<Address>,
    logs: Vec<Log>,
}

#[derive(Clone, Default)]
struct StateDiffAccumulator(Arc<Mutex<InMemoryDB>>);

impl StateDiffAccumulator {
    fn cache(&self) -> reth_revm::db::Cache {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .cache
            .clone()
    }
}

impl OnStateHook for StateDiffAccumulator {
    fn on_state(&mut self, _source: StateChangeSource, state: &revm::state::EvmState) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .commit(state.clone());
    }
}

impl TxExecutionOutcome {
    fn from_result<H>(result: &revm::context::result::ExecutionResult<H>) -> Self {
        Self {
            success: result.is_success(),
            gas_used: result.gas_used(),
            output: result.output().cloned(),
            created_address: result.created_address(),
            logs: result.logs().to_vec(),
        }
    }
}

fn new_debank_inspector() -> (TracingInspector, ArcEventInspector) {
    let mut config = TracingInspectorConfig::default_parity()
        .set_steps(true)
        .set_record_logs(true)
        .set_exclude_precompile_calls(true);
    config.record_opcodes_filter = Some(OpcodeFilter::new().enabled(OpCode::SSTORE));
    (TracingInspector::new(config), ArcEventInspector::default())
}

fn receipt_logs<T: serde::Serialize>(receipt: &T) -> Result<Vec<Log>, EthApiError> {
    let value = serde_json::to_value(receipt)
        .map_err(|err| EthApiError::EvmCustom(format!("failed to serialize receipt: {err}")))?;
    let logs = value
        .get("logs")
        .cloned()
        .ok_or_else(|| EthApiError::EvmCustom("receipt is missing logs".to_string()))?;
    let logs: Vec<alloy_rpc_types_eth::Log> = serde_json::from_value(logs)
        .map_err(|err| EthApiError::EvmCustom(format!("failed to decode receipt logs: {err}")))?;
    Ok(logs.into_iter().map(|log| log.inner).collect())
}

/// `trace` namespace API implementation for `debankBlock`.
#[derive(Clone)]
pub struct DebankTraceBlock<Eth> {
    eth_api: Eth,
}

impl<Eth> DebankTraceBlock<Eth> {
    pub fn new(eth_api: Eth) -> Self {
        Self { eth_api }
    }
}

impl<Eth> DebankTraceBlock<Eth>
where
    Eth: EthApiTypes
        + EthBlocks
        + LoadBlock
        + LoadReceipt
        + LoadState
        + SpawnBlocking
        + TraceExt
        + 'static,
    Eth::Provider: ChainSpecProvider<ChainSpec: EthChainSpec + EthereumHardforks>,
{
    /// Build `DebankOutPut` for the given block.
    async fn trace_debank_block(&self, block_id: BlockId) -> Result<DebankOutPut, Eth::Error> {
        let block = self.eth_api.recovered_block(block_id).await?;
        let Some(block) = block else {
            return Err(EthApiError::HeaderNotFound(block_id).into());
        };

        let debank_block = DebankBlock {
            id: block.hash(),
            height: block.number(),
            parent_id: block.parent_hash(),
            base_fee_per_gas: block.base_fee_per_gas(),
            miner: block.beneficiary(),
            gas_limit: block.gas_limit(),
            gas_used: block.gas_used(),
            timestamp: block.timestamp(),
            process_start_timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0),
        };

        let debank_header = Header {
            inner: alloy_consensus::Header {
                parent_hash: block.parent_hash(),
                ommers_hash: block.ommers_hash(),
                beneficiary: block.beneficiary(),
                state_root: block.state_root(),
                transactions_root: block.transactions_root(),
                receipts_root: block.receipts_root(),
                logs_bloom: block.logs_bloom(),
                difficulty: block.difficulty(),
                number: block.number(),
                gas_limit: block.gas_limit(),
                gas_used: block.gas_used(),
                timestamp: block.timestamp(),
                extra_data: block.extra_data().clone(),
                mix_hash: block.mix_hash().unwrap_or_default(),
                nonce: block.nonce().unwrap_or_default(),
                base_fee_per_gas: block.base_fee_per_gas(),
                withdrawals_root: block.withdrawals_root(),
                blob_gas_used: block.blob_gas_used(),
                excess_blob_gas: block.excess_blob_gas(),
                parent_beacon_block_root: block.parent_beacon_block_root(),
                requests_hash: block.requests_hash(),
            },
            hash: block.hash(),
            total_difficulty: None,
            size: None,
        };

        // Genesis block: synthetic txs from chain spec
        if block.number() == 0 {
            let chain_spec = self.eth_api.provider().chain_spec();
            let genesis = chain_spec.genesis();
            let mut state_diff: BlockStorageDiff = genesis.into();
            state_diff.hash = block.state_root();
            let (transactions, traces) = build_genesis_txs_and_traces(genesis);
            let block_file = BlockFile {
                block: debank_block,
                transactions,
                traces,
                storage_contracts: get_storage_contracts_from_genesis(genesis),
                ..Default::default()
            };
            let validation_hash = block_file.validation().validation_hash;
            return Ok(DebankOutPut {
                block_file,
                header: debank_header,
                state_diff: alloy_rlp::encode(state_diff).into(),
                validation_hash,
            });
        }

        // Build DebankTransactions from receipts
        use alloy_network::ReceiptResponse;

        let receipts = self.eth_api.block_receipts(block_id).await?;
        let Some(receipts) = receipts else {
            return Err(EthApiError::HeaderNotFound(block_id).into());
        };

        let block_txs = block.body().transactions();
        if receipts.len() != block_txs.len() {
            return Err(EthApiError::EvmCustom(format!(
                "block {} has {} transactions but {} receipts",
                block.number(),
                block_txs.len(),
                receipts.len(),
            ))
            .into());
        }
        let mut debank_txs: Vec<DebankTransaction> = Vec::with_capacity(block_txs.len());

        for index in 0..block_txs.len() {
            let tx = &block_txs[index];
            let receipt = &receipts[index];

            // Arc has only standard tx types (no AA tx); use trait accessors
            // directly. Tempo-only fields (`calls`, `fee_token`, `signature`, ...)
            // stay at their `None`/default values per D3 (schema-preserving) — D17
            // covers the related Header field-set divergence.
            let dtx = DebankTransaction {
                id: receipt.transaction_hash().to_string(),
                from: receipt.from(),
                to: debank_transaction_target(receipt.to(), receipt.contract_address()),
                gas_limit: tx.gas_limit(),
                gas_price: receipt.effective_gas_price(),
                gas_used: receipt.gas_used(),
                status: receipt.status(),
                gas_fee_cap: tx.max_fee_per_gas(),
                gas_tip_cap: tx.max_priority_fee_per_gas().unwrap_or_default(),
                input: tx.input().clone(),
                nonce: tx.nonce(),
                transaction_index: receipt.transaction_index().unwrap_or(0),
                value: tx.value(),
                ..Default::default()
            };

            debank_txs.push(dtx);
        }

        // Per-tx receipt status drives the failed-tx classification below.
        let tx_statuses: Vec<bool> = receipts.iter().map(|r| r.status()).collect();

        // `ReceiptResponse` does not expose logs directly. Decode its stable RPC
        // representation once and retain the complete consensus logs for exact
        // comparison with both execution passes and the event inspector.
        let receipt_logs_per_tx: Vec<Vec<Log>> = receipts
            .iter()
            .map(receipt_logs)
            .collect::<Result<_, _>>()?;

        let parent_hash = block.parent_hash();
        let parent_block = self.eth_api.recovered_block(parent_hash.into()).await?;
        let Some(parent_block) = parent_block else {
            return Err(EthApiError::HeaderNotFound(block_id).into());
        };

        let mut block_file = BlockFile {
            block: debank_block,
            transactions: debank_txs,
            ..Default::default()
        };

        // Prepare block replay.
        // The genesis path was handled above; all other blocks (including
        // empty ones) take the replay path unconditionally so trace output
        // stays consistent with trace_transaction.
        let block_state_root = block.state_root();
        let parent_state_root = parent_block.state_root();

        // Collect tx hashes before move
        let tx_hashes: Vec<B256> = block_txs.iter().map(|tx| *tx.tx_hash()).collect();

        let (evm_env, _) = self.eth_api.evm_env_at(block_id).await?;

        let parent_block_id = BlockId::hash(parent_hash);
        let tx_statuses_clone = tx_statuses.clone();

        let (traces_result, state_diff, change_addresses) = self
            .eth_api
            .spawn_blocking_io_fut(move |eth_api| async move {
                // Three independent views of the same parent state: one for
                // comparing the final diff, one for canonical block execution,
                // and one for trace replay.
                let pre_state = eth_api.state_at_block_id(parent_block_id).await?;
                let state_pass_provider = eth_api.state_at_block_id(parent_block_id).await?;
                let trace_pass_provider = eth_api.state_at_block_id(parent_block_id).await?;

                let pre_db = State::builder()
                    .with_database(StateProviderDatabase::new(StateProviderTraitObjWrapper(
                        pre_state,
                    )))
                    .build();

                // State pass: execute the complete block through ArcBlockExecutor,
                // including pre- and post-execution hooks. The executor commits
                // into its outer State cache, so capture every state change through
                // its hook instead of wrapping the underlying provider DB.
                let captured_diff = StateDiffAccumulator::default();
                let mut canonical_outcomes = Vec::with_capacity(tx_hashes.len());
                {
                    let mut state_db = State::builder()
                        .with_database(StateProviderDatabase::new(StateProviderTraitObjWrapper(
                            state_pass_provider,
                        )))
                        .build();
                    let mut executor = eth_api
                        .evm_config()
                        .executor_for_block(&mut state_db, block.sealed_block())
                        .map_err(|err| EthApiError::EvmCustom(err.to_string()))?;
                    executor.set_state_hook(Some(Box::new(captured_diff.clone())));

                    executor
                        .apply_pre_execution_changes()
                        .map_err(EthApiError::from)?;
                    for tx in block.transactions_recovered() {
                        executor
                            .execute_transaction_with_result_closure(tx, |result| {
                                canonical_outcomes.push(TxExecutionOutcome::from_result(result));
                            })
                            .map_err(EthApiError::from)?;
                    }
                    let execution_result = executor
                        .apply_post_execution_changes()
                        .map_err(EthApiError::from)?;
                    if execution_result.gas_used != block.gas_used()
                        || execution_result.receipts.len() != receipts.len()
                    {
                        return Err(EthApiError::EvmCustom(format!(
                            "canonical replay mismatch for block {}: gas {}/{}, receipts {}/{}",
                            block.number(),
                            execution_result.gas_used,
                            block.gas_used(),
                            execution_result.receipts.len(),
                            receipts.len(),
                        ))
                        .into());
                    }
                    for (idx, outcome) in canonical_outcomes.iter().enumerate() {
                        let receipt = receipts.get(idx).ok_or_else(|| {
                            EthApiError::EvmCustom(format!(
                                "missing receipt {idx} for block {}",
                                block.number()
                            ))
                        })?;
                        if outcome.success != tx_statuses_clone.get(idx).copied().unwrap_or(false)
                            || outcome.gas_used != receipt.gas_used()
                            || outcome.logs
                                != receipt_logs_per_tx.get(idx).cloned().unwrap_or_default()
                        {
                            return Err(EthApiError::EvmCustom(format!(
                                "canonical replay differs from receipt {idx} for block {}",
                                block.number()
                            ))
                            .into());
                        }
                    }
                }

                let captured_cache = captured_diff.cache();
                let change_addresses = get_storage_contracts_from_cache(&captured_cache);
                let state_diff = get_storage_diffs_from_cache(captured_cache, pre_db);

                let mut trace_db = State::builder()
                    .with_database(StateProviderDatabase::new(StateProviderTraitObjWrapper(
                        trace_pass_provider,
                    )))
                    .build();
                let evm_config = eth_api.evm_config();
                let trace_context = evm_config
                    .context_for_block(block.sealed_block())
                    .map_err(|err| EthApiError::EvmCustom(err.to_string()))?;
                let trace_evm = evm_config.evm_with_env_and_inspector(
                    &mut trace_db,
                    evm_env,
                    new_debank_inspector(),
                );
                let mut trace_executor = evm_config.create_executor(trace_evm, trace_context);
                trace_executor
                    .apply_pre_execution_changes()
                    .map_err(EthApiError::from)?;
                // Pre-execution system calls are block-level state changes, not
                // transaction traces. Start the first transaction with a fresh arena.
                *trace_executor.evm_mut().inspector_mut() = new_debank_inspector();

                let log_index = std::cell::RefCell::new(0usize);
                // (traces, error_traces, events, error_events)
                let mut all_results: Vec<DebankTraceResult> = Vec::new();

                for (idx, tx) in block.transactions_recovered().enumerate() {
                    let tx_hash = tx_hashes[idx];
                    let mut trace_outcome = None;
                    trace_executor
                        .execute_transaction_with_result_closure(tx, |result| {
                            trace_outcome = Some(TxExecutionOutcome::from_result(result));
                        })
                        .map_err(EthApiError::from)?;
                    let trace_outcome = trace_outcome.ok_or_else(|| {
                        EthApiError::EvmCustom(format!(
                            "missing trace result for transaction {tx_hash}"
                        ))
                    })?;
                    if canonical_outcomes.get(idx) != Some(&trace_outcome) {
                        return Err(EthApiError::EvmCustom(format!(
                            "state and trace replay differ for transaction {tx_hash}"
                        ))
                        .into());
                    }

                    let exec_logs = trace_outcome.logs;
                    let (tracing_inspector, event_inspector) = std::mem::replace(
                        trace_executor.evm_mut().inspector_mut(),
                        new_debank_inspector(),
                    );
                    let arena = tracing_inspector.into_traces();
                    let captured_events = event_inspector.into_captured();
                    if captured_events.successful_logs() != exec_logs {
                        return Err(EthApiError::EvmCustom(format!(
                            "event inspector logs differ from execution for transaction {tx_hash}"
                        ))
                        .into());
                    }
                    let (traces, error_traces, events, error_events) =
                        build_debank_traces(tx_hash, arena, captured_events, &log_index)
                            .map_err(EthApiError::EvmCustom)?;

                    all_results.push((traces, error_traces, events, error_events));
                }

                Ok((all_results, state_diff, change_addresses))
            })
            .await?;

        // Assemble block file.
        // D18: Classification uses per-node success from build_debank_traces, with
        // receipt status as override. The `root_misclassified` AA merge branch is
        // kept structurally identical to Tempo so the two ports stay diff-friendly,
        // even though Arc has no AA wrapper traces and the branch never fires.
        //
        // 1. Successful tx, root trace correctly classified (in traces):
        //    Keep per-node classification. Internal revert sub-calls
        //    (try/catch) stay in error lists. Matches reth-x behavior.
        //
        // 2. Successful tx, root trace misclassified (in error_traces):
        //    AA-only path (dead on Arc) — `CallTraceArena` marks the handler
        //    wrapper and its children as success=false even though the tx
        //    succeeds. The arena's success flags are unreliable for the
        //    entire tree, so merge all error_traces/events into success lists.
        //
        // 3. Failed tx: all traces/events go to error lists.
        for (idx, (mut trace, mut error_trace, mut event, mut error_event)) in
            traces_result.into_iter().enumerate()
        {
            let tx_success = tx_statuses.get(idx).copied().unwrap_or(true);
            if tx_success {
                let root_misclassified = error_trace.iter().any(|t| t.trace_address.is_empty());
                if root_misclassified {
                    // AA tx: arena success flags unreliable, merge all
                    trace.extend(error_trace);
                    event.extend(error_event);
                } else {
                    // Normal tx: keep per-node classification (try/catch)
                    block_file.error_traces.extend(error_trace);
                    block_file.error_events.extend(error_event);
                }
                block_file.traces.extend(trace);
                block_file.events.extend(event);
            } else {
                // Tx failed: all traces/events go to error lists
                error_trace.extend(trace);
                error_event.extend(event);
                for event in &mut error_event {
                    event.idx = 0;
                }
                block_file.error_traces.extend(error_trace);
                block_file.error_events.extend(error_event);
            }
        }

        let mut state_diff = state_diff;
        state_diff.hash = block_state_root;
        state_diff.parent_hash = parent_state_root;
        block_file.storage_contracts = change_addresses;

        let validation_hash = block_file.validation().validation_hash;
        Ok(DebankOutPut {
            block_file,
            header: debank_header,
            state_diff: alloy_rlp::encode(state_diff).into(),
            validation_hash,
        })
    }
}

fn debank_transaction_target(
    recipient: Option<alloy_primitives::Address>,
    contract_address: Option<alloy_primitives::Address>,
) -> alloy_primitives::Address {
    recipient.or(contract_address).unwrap_or_default()
}

// ---------------------------------------------------------------------------
// jsonrpsee server trait implementation
// ---------------------------------------------------------------------------

#[async_trait::async_trait]
impl<Eth> crate::DebankTraceApiServer for DebankTraceBlock<Eth>
where
    Eth: EthApiTypes
        + EthBlocks
        + EthTransactions
        + LoadBlock
        + LoadReceipt
        + LoadState
        + SpawnBlocking
        + TraceExt
        + 'static,
    Eth::Provider: ChainSpecProvider<ChainSpec: EthChainSpec + EthereumHardforks>,
{
    async fn trace_debank_block(&self, block_id: BlockId) -> RpcResult<DebankOutPut> {
        Self::trace_debank_block(self, block_id)
            .await
            .map_err(Into::into)
    }
}

impl<Eth> std::fmt::Debug for DebankTraceBlock<Eth> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DebankTraceBlock").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_evm::block::{StateChangePostBlockSource, StateChangePreBlockSource};
    use alloy_primitives::{Address, U256};
    use revm::state::{Account, AccountInfo, EvmState, EvmStorageSlot};

    fn state_change(address: Address, balance: u64, slots: &[(u64, u64, u64)]) -> EvmState {
        let mut account = Account::from(AccountInfo {
            balance: U256::from(balance),
            ..Default::default()
        });
        account.mark_touch();
        for (key, old, new) in slots {
            account.storage.insert(
                U256::from(*key),
                EvmStorageSlot::new_changed(U256::from(*old), U256::from(*new), 0),
            );
        }
        let mut state = EvmState::default();
        state.insert(address, account);
        state
    }

    #[test]
    fn transaction_target_uses_created_address_for_create() {
        let created = Address::repeat_byte(0x11);
        assert_eq!(debank_transaction_target(None, Some(created)), created);
    }

    #[test]
    fn transaction_target_prefers_recipient_for_call() {
        let recipient = Address::repeat_byte(0x22);
        let unexpected_contract = Address::repeat_byte(0x33);
        assert_eq!(
            debank_transaction_target(Some(recipient), Some(unexpected_contract)),
            recipient
        );
    }

    #[test]
    fn state_diff_accumulator_merges_pre_transaction_and_post_changes() {
        let address = Address::repeat_byte(0x11);
        let mut accumulator = StateDiffAccumulator::default();
        accumulator.on_state(
            StateChangeSource::PreBlock(StateChangePreBlockSource::BlockHashesContract),
            &state_change(address, 1, &[(0, 0, 1)]),
        );
        accumulator.on_state(
            StateChangeSource::Transaction(0),
            &state_change(address, 2, &[(1, 0, 2)]),
        );
        accumulator.on_state(
            StateChangeSource::PostBlock(StateChangePostBlockSource::BalanceIncrements),
            &state_change(address, 3, &[(0, 1, 3)]),
        );

        let cache = accumulator.cache();
        let account = cache.accounts.get(&address).unwrap();
        assert_eq!(account.info.balance, U256::from(3));
        assert_eq!(account.storage.get(&U256::from(0)), Some(&U256::from(3)));
        assert_eq!(account.storage.get(&U256::from(1)), Some(&U256::from(2)));
    }
}
