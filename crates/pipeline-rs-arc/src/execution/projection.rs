use alloy_consensus::{BlockHeader, Transaction};
use alloy_genesis::Genesis;
use alloy_primitives::{Address, B256};
use alloy_rlp::Decodable;
use alloy_rpc_types_eth::Header;
use revm::database::BundleState;

use super::debank::*;
use crate::{BlockRef, CapturedArcBlock, SpoolError, Validation};

pub fn block_metadata(header: &alloy_consensus::Header, hash: B256) -> (DebankBlock, Header) {
    (
        DebankBlock {
            id: hash,
            height: header.number(),
            parent_id: header.parent_hash(),
            base_fee_per_gas: header.base_fee_per_gas(),
            miner: header.beneficiary(),
            gas_limit: header.gas_limit(),
            gas_used: header.gas_used(),
            timestamp: header.timestamp(),
            process_start_timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0),
        },
        Header {
            inner: header.clone(),
            hash,
            total_difficulty: None,
            size: None,
        },
    )
}

pub fn genesis_output(
    genesis: &alloy_genesis::Genesis,
    block: DebankBlock,
    header: Header,
) -> DebankOutPut {
    let (transactions, traces) = build_genesis_txs_and_traces(genesis);
    let block_file = BlockFile {
        block,
        transactions,
        traces,
        storage_contracts: get_storage_contracts_from_genesis(genesis),
        ..Default::default()
    };
    project_output(block_file, header, genesis.into())
}

pub fn project_output(
    block_file: BlockFile,
    header: Header,
    mut state_diff: BlockStorageDiff,
) -> DebankOutPut {
    state_diff.hash = header.state_root;
    let validation_hash = block_file.validation().validation_hash;
    DebankOutPut {
        block_file,
        header,
        state_diff: alloy_rlp::encode(state_diff).into(),
        validation_hash,
    }
}

pub fn transaction_metadata(
    tx: &impl Transaction,
    hash: B256,
    from: Address,
    gas_price: u128,
    transaction_index: u64,
) -> DebankTransaction {
    DebankTransaction {
        id: hash.to_string(),
        from,
        gas_limit: tx.gas_limit(),
        gas_price,
        gas_fee_cap: tx.max_fee_per_gas(),
        gas_tip_cap: tx.max_priority_fee_per_gas().unwrap_or_default(),
        input: tx.input().clone(),
        nonce: tx.nonce(),
        transaction_index,
        value: tx.value(),
        ..Default::default()
    }
}

pub fn append_traces(
    block_file: &mut BlockFile,
    tx_success: bool,
    (traces, error_traces, events, error_events): DebankTraceResult,
) -> Result<(), &'static str> {
    validate_transaction_classification(
        tx_success,
        &traces,
        &error_traces,
        &events,
        &error_events,
    )?;
    block_file.traces.extend(traces);
    block_file.error_traces.extend(error_traces);
    block_file.events.extend(events);
    block_file.error_events.extend(error_events);
    Ok(())
}

pub fn validate_transaction_classification(
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

pub fn debank_transaction_target(
    recipient: Option<alloy_primitives::Address>,
    contract_address: Option<alloy_primitives::Address>,
) -> alloy_primitives::Address {
    recipient.or(contract_address).unwrap_or_default()
}

fn capture_output(
    output: DebankOutPut,
    parent_state_root: B256,
) -> Result<CapturedArcBlock, SpoolError> {
    let block_file = &output.block_file;
    let header = &output.header;
    let capture = CapturedArcBlock {
        block: BlockRef {
            block_number: header.number,
            hash: header.hash,
            parent_hash: header.parent_hash,
            timestamp: header.timestamp,
        },
        state_root: header.state_root,
        parent_state_root,
        block_file: serde_json::value::to_raw_value(block_file)?,
        header: serde_json::value::to_raw_value(header)?,
        state_diff_rlp: output.state_diff,
        validation: Validation {
            validation_hash: output.validation_hash,
            is_fork: false,
            txs_count: block_file.transactions.len(),
            events_count: block_file.events.len(),
            traces_count: block_file.traces.len(),
            error_events_count: block_file.error_events.len(),
            error_traces_count: block_file.error_traces.len(),
            storage_contracts_count: block_file.storage_contracts.len(),
        },
    };
    capture.validate_state_diff()?;
    Ok(capture)
}

/// Convert the existing replay response into a spool capture.
pub fn capture_replayed_block(output: DebankOutPut) -> Result<CapturedArcBlock, SpoolError> {
    let mut bytes = output.state_diff.as_ref();
    let mut list = alloy_rlp::Header::decode_bytes(&mut bytes, true)?;
    let _ = B256::decode(&mut list)?;
    let parent_state_root = B256::decode(&mut list)?;
    capture_output(output, parent_state_root)
}

pub fn capture_genesis(
    genesis: &Genesis,
    header: &alloy_consensus::Header,
    hash: B256,
) -> Result<CapturedArcBlock, SpoolError> {
    let (block, header) = block_metadata(header, hash);
    capture_replayed_block(genesis_output(genesis, block, header))
}

pub fn capture_block(
    header: &alloy_consensus::Header,
    hash: B256,
    parent_state_root: B256,
    bundle: BundleState,
    mut block_file: BlockFile,
) -> Result<CapturedArcBlock, SpoolError> {
    let (metadata, header) = block_metadata(header, hash);
    block_file.block = metadata;
    block_file.storage_contracts = get_storage_contracts_from_bundle(&bundle);
    let mut state_diff = captured_storage_diff(bundle);
    state_diff.parent_hash = parent_state_root;
    capture_output(
        project_output(block_file, header, state_diff),
        parent_state_root,
    )
}
