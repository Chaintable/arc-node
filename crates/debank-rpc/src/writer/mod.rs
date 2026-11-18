//! Execution-time Arc Writer capture.

mod config;
mod evm;
mod executor;

use alloy_genesis::Genesis;
use alloy_primitives::B256;
use alloy_rlp::Decodable;
use pipeline_rs_arc::{BlockRef, CapturedArcBlock, SpoolError, Validation};
use reth_ethereum_primitives::EthPrimitives;
use reth_primitives_traits::{BlockTy, SealedBlock};
use revm::database::BundleState;

use crate::{
    debank_trace::{
        captured_storage_diff, get_storage_contracts_from_bundle, BlockFile, BlockStorageDiff,
        DebankOutPut,
    },
    trace_block::{block_metadata, genesis_output},
};

pub use config::{LiveCapture, WriterEvmConfig};

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

pub fn capture_genesis(
    genesis: &Genesis,
    block: &SealedBlock<BlockTy<EthPrimitives>>,
) -> Result<CapturedArcBlock, SpoolError> {
    let (block, header) = block_metadata(block);
    let output = genesis_output(genesis, block, header);
    let diff = BlockStorageDiff::decode(&mut output.state_diff.as_ref())?;
    capture_output(output, diff.parent_hash)
}

fn capture_block(
    block: &SealedBlock<BlockTy<EthPrimitives>>,
    parent_state_root: B256,
    bundle: BundleState,
    mut block_file: BlockFile,
) -> Result<CapturedArcBlock, SpoolError> {
    let (metadata, header) = block_metadata(block);
    block_file.block = metadata;
    block_file.storage_contracts = get_storage_contracts_from_bundle(&bundle);
    let mut state_diff = captured_storage_diff(bundle);
    state_diff.hash = header.state_root;
    state_diff.parent_hash = parent_state_root;
    let validation_hash = block_file.validation().validation_hash;
    capture_output(
        DebankOutPut {
            header,
            block_file,
            state_diff: alloy_rlp::encode(state_diff).into(),
            validation_hash,
        },
        parent_state_root,
    )
}
