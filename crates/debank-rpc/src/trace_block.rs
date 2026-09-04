//! `trace_debankBlock` RPC implementation.
//!
//! Replays all transactions in a block, collecting DeBank-format traces, events,
//! and state diffs for consumption by background-tracer → S3/Kafka → leafage-evm.

use alloy_consensus::{BlockHeader, Transaction};
use alloy_eips::BlockId;
use alloy_primitives::{Address, Bytes, Log, B256};
use alloy_rpc_types_eth::Header;
use arc_evm::ArcEvmConfig;
use jsonrpsee::core::RpcResult;
use reth_chainspec::{EthChainSpec, EthereumHardforks};
use reth_ethereum_primitives::EthPrimitives;
use reth_evm::{block::TxResult, execute::BlockExecutor, ConfigureEvm, Evm};
use reth_primitives_traits::{BlockBody, NodePrimitives};
use reth_provider::ChainSpecProvider;
use reth_revm::{database::StateProviderDatabase, State};
use reth_rpc_eth_api::{
    helpers::{
        EthBlocks, EthTransactions, LoadBlock, LoadReceipt, LoadState, SpawnBlocking, TraceExt,
    },
    EthApiTypes, RpcNodeCore,
};
use reth_rpc_eth_types::{cache::db::StateProviderTraitObjWrapper, EthApiError};
use revm::{bytecode::opcode::OpCode, database::states::bundle_state::BundleRetention};
use revm_inspectors::tracing::{OpcodeFilter, TracingInspector, TracingInspectorConfig};

use crate::debank_trace::*;
use crate::event_inspector::{record_subcall_trace_completion, ArcEventInspector};

#[derive(Clone, Debug, PartialEq, Eq)]
struct TxExecutionOutcome {
    success: bool,
    gas_used: u64,
    output: Option<Bytes>,
    created_address: Option<Address>,
    logs: Vec<Log>,
}

impl TxExecutionOutcome {
    fn from_result<R: TxResult>(tx_result: &R) -> Self {
        let result = &tx_result.result().result;
        Self {
            success: result.is_success(),
            gas_used: result.tx_gas_used(),
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
        .set_exclude_precompile_calls(false);
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
        + RpcNodeCore<Evm = ArcEvmConfig, Primitives = EthPrimitives>
        + EthBlocks
        + LoadBlock
        + LoadReceipt
        + LoadState
        + SpawnBlocking
        + TraceExt
        + 'static,
    Eth::Primitives: NodePrimitives<BlockHeader = alloy_consensus::Header>,
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
            inner: block.header().clone(),
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

        let block_txs = BlockBody::transactions(block.body());
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
            // stay at their `None`/default values for schema compatibility.
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
        let block_number = block.number();

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
                // including pre- and post-execution changes. Bundle updates preserve
                // the canonical EIP-161 and net-storage-change semantics applied by State.
                let mut canonical_outcomes = Vec::with_capacity(tx_hashes.len());
                let bundle = {
                    let mut state_db = State::builder()
                        .with_database(StateProviderDatabase::new(StateProviderTraitObjWrapper(
                            state_pass_provider,
                        )))
                        .with_bundle_update()
                        .build();
                    let mut executor = eth_api
                        .evm_config()
                        .executor_for_block(&mut state_db, block.sealed_block())
                        .map_err(|err| EthApiError::EvmCustom(err.to_string()))?;

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
                    let (evm, execution_result) = executor.finish().map_err(EthApiError::from)?;
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
                    let (state_db, _) = evm.finish();
                    state_db.merge_transitions(BundleRetention::PlainState);
                    state_db.take_bundle()
                };

                let change_addresses = get_storage_contracts_from_bundle(&bundle);
                let state_diff = get_storage_diffs_from_bundle(bundle, pre_db);

                let mut trace_db = State::builder()
                    .with_database(StateProviderDatabase::new(StateProviderTraitObjWrapper(
                        trace_pass_provider,
                    )))
                    .build();
                let evm_config = eth_api.evm_config();
                let trace_context = evm_config
                    .context_for_block(block.sealed_block())
                    .map_err(|err| EthApiError::EvmCustom(err.to_string()))?;
                let mut trace_evm = evm_config.evm_with_env_and_inspector(
                    &mut trace_db,
                    evm_env,
                    new_debank_inspector(),
                );
                trace_evm.set_subcall_trace_completion_hook(record_subcall_trace_completion);
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

        // Assemble the block file without rewriting inspector classifications.
        // Arc has no Tempo AA wrapper transactions, so a receipt/root mismatch
        // indicates an inspector regression and must fail instead of moving traces
        // between success and error buckets.
        for (idx, (trace, error_trace, event, error_event)) in traces_result.into_iter().enumerate()
        {
            let tx_success = tx_statuses.get(idx).copied().ok_or_else(|| {
                EthApiError::EvmCustom(format!(
                    "missing receipt status for transaction {idx} in block {block_number}"
                ))
            })?;
            validate_transaction_classification(
                tx_success,
                &trace,
                &error_trace,
                &event,
                &error_event,
            )
            .map_err(|error| {
                EthApiError::EvmCustom(format!(
                    "invalid trace classification for transaction {idx} in block {}: {error}",
                    block_number
                ))
            })?;
            block_file.traces.extend(trace);
            block_file.error_traces.extend(error_trace);
            block_file.events.extend(event);
            block_file.error_events.extend(error_event);
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

fn validate_transaction_classification(
    tx_success: bool,
    traces: &[DebankTrace],
    error_traces: &[DebankTrace],
    events: &[DebankEvent],
    error_events: &[DebankEvent],
) -> Result<(), &'static str> {
    if traces.iter().any(|trace| !trace.error.is_empty()) {
        return Err("successful trace has an error");
    }
    if error_traces.iter().any(|trace| trace.error.is_empty()) {
        return Err("error trace has no error");
    }
    if error_events.iter().any(|event| event.idx != 0) {
        return Err("error event has a non-zero index");
    }

    let success_roots = traces
        .iter()
        .filter(|trace| trace.parent_trace_id.is_empty())
        .count();
    let error_roots = error_traces
        .iter()
        .filter(|trace| trace.parent_trace_id.is_empty())
        .count();
    if tx_success {
        if success_roots != 1 || error_roots != 0 {
            return Err("successful receipt does not have exactly one successful root trace");
        }
    } else if success_roots != 0 || error_roots != 1 || !traces.is_empty() || !events.is_empty() {
        return Err("failed receipt has successful traces or events");
    }

    Ok(())
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
        + RpcNodeCore<Evm = ArcEvmConfig, Primitives = EthPrimitives>
        + EthBlocks
        + EthTransactions
        + LoadBlock
        + LoadReceipt
        + LoadState
        + SpawnBlocking
        + TraceExt
        + 'static,
    Eth::Primitives: NodePrimitives<BlockHeader = alloy_consensus::Header>,
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
    use alloy_primitives::{Address, TxKind, U256};
    use revm::{
        context::TxEnv,
        database::{EmptyDB, InMemoryDB},
        inspector::InspectorEvmTr,
        primitives::hardfork::SpecId,
        state::{Account, AccountInfo, Bytecode, EvmState, EvmStorageSlot},
        DatabaseCommit, InspectEvm, MainBuilder, MainContext,
    };

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
    fn trace_classification_rejects_receipt_or_error_mismatches() {
        let success_root = DebankTrace::default();
        let internal_error = DebankTrace {
            parent_trace_id: "root".to_string(),
            error: "Reverted".to_string(),
            ..Default::default()
        };
        assert!(validate_transaction_classification(
            true,
            std::slice::from_ref(&success_root),
            std::slice::from_ref(&internal_error),
            &[],
            &[DebankEvent::default()]
        )
        .is_ok());

        let error_root = DebankTrace {
            error: "Reverted".to_string(),
            ..Default::default()
        };
        let parent_failed_child = DebankTrace {
            parent_trace_id: "root".to_string(),
            error: "parent call failed".to_string(),
            ..Default::default()
        };
        assert!(validate_transaction_classification(
            false,
            &[],
            &[error_root.clone(), parent_failed_child],
            &[],
            &[]
        )
        .is_ok());
        assert!(validate_transaction_classification(
            true,
            &[],
            std::slice::from_ref(&error_root),
            &[],
            &[]
        )
        .is_err());
        assert!(validate_transaction_classification(
            false,
            std::slice::from_ref(&success_root),
            &[],
            &[],
            &[]
        )
        .is_err());
        assert!(validate_transaction_classification(
            false,
            &[],
            &[DebankTrace::default()],
            &[],
            &[]
        )
        .is_err());
        let success_with_error = DebankTrace {
            error: "Reverted".to_string(),
            ..Default::default()
        };
        assert!(
            validate_transaction_classification(true, &[success_with_error], &[], &[], &[])
                .is_err()
        );
        let nonzero_error_event = DebankEvent {
            idx: 1,
            ..Default::default()
        };
        assert!(validate_transaction_classification(
            true,
            &[success_root],
            &[internal_error],
            &[],
            &[nonzero_error_event]
        )
        .is_err());
    }

    #[test]
    fn debank_inspector_attaches_standard_precompile_to_call_tree() {
        let caller = Address::repeat_byte(0xaa);
        let contract = Address::repeat_byte(0xbb);
        let ecrecover = Address::with_last_byte(1);
        let mut db = InMemoryDB::default();
        db.insert_account_info(
            caller,
            AccountInfo {
                balance: U256::from(1_000_000_000u64),
                ..Default::default()
            },
        );
        db.insert_account_info(
            contract,
            AccountInfo {
                // STATICCALL ecrecover with 128 zero bytes of input, then discard the result.
                code: Some(Bytecode::new_legacy(Bytes::from_static(&[
                    0x60, 0x20, 0x60, 0x00, 0x60, 0x80, 0x60, 0x00, 0x60, 0x01, 0x61, 0xff, 0xff,
                    0xfa, 0x50, 0x00,
                ]))),
                ..Default::default()
            },
        );

        let mut evm = revm::Context::mainnet()
            .modify_cfg_chained(|cfg| cfg.spec = SpecId::CANCUN)
            .with_db(db)
            .build_mainnet_with_inspector(new_debank_inspector());
        let result = evm
            .inspect_tx(TxEnv {
                caller,
                kind: TxKind::Call(contract),
                gas_limit: 1_000_000,
                gas_price: 0,
                ..Default::default()
            })
            .expect("precompile fixture should execute");
        assert!(result.result.is_success());

        let (_, inspectors) = evm.ctx_inspector();
        let nodes = inspectors.0.traces().nodes();
        assert!(nodes[0]
            .children
            .iter()
            .any(|&child| nodes[child].trace.address == ecrecover));
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
    fn canonical_bundle_merges_pre_transaction_and_post_changes() {
        let address = Address::repeat_byte(0x11);
        let mut state = State::builder()
            .with_database(EmptyDB::default())
            .with_bundle_update()
            .build();
        state.insert_not_existing(address);
        state.commit(state_change(address, 1, &[(0, 0, 1)]));
        state.commit(state_change(address, 2, &[(1, 0, 2)]));
        state.commit(state_change(address, 3, &[(0, 1, 3)]));
        state.merge_transitions(BundleRetention::PlainState);

        let bundle = state.take_bundle();
        let account = bundle.state.get(&address).unwrap();
        assert_eq!(account.info.as_ref().unwrap().balance, U256::from(3));
        assert_eq!(
            account.storage.get(&U256::from(0)).unwrap().present_value,
            U256::from(3)
        );
        assert_eq!(
            account.storage.get(&U256::from(1)).unwrap().present_value,
            U256::from(2)
        );
    }

    #[test]
    fn canonical_bundle_omits_storage_restored_to_parent_value() {
        let address = Address::repeat_byte(0x11);
        let mut state = State::builder()
            .with_database(EmptyDB::default())
            .with_bundle_update()
            .build();
        state.insert_account(
            address,
            AccountInfo {
                balance: U256::from(1),
                ..Default::default()
            },
        );
        state.commit(state_change(address, 1, &[(0, 0, 7)]));
        state.commit(state_change(address, 1, &[(0, 7, 0)]));
        state.merge_transitions(BundleRetention::PlainState);

        let diff = get_storage_diffs_from_bundle(state.take_bundle(), EmptyDB::default());
        assert!(diff.new_accounts.is_empty());
        assert!(diff.deleted_accounts.is_empty());
        assert!(diff.storage_diffs.is_empty());
        assert!(diff.new_codes.is_empty());
    }
}
