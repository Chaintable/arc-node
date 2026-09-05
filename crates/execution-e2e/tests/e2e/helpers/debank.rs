// Copyright 2026 Circle Internet Group, Inc. All rights reserved.
//
// SPDX-License-Identifier: Apache-2.0
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//      http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Reusable `trace_debankBlock` assertions for launched-node e2e tests.

use std::collections::HashSet;

use alloy_consensus::constants::EMPTY_ROOT_HASH;
use alloy_eips::BlockId;
use alloy_primitives::{Address, B256, U256};
use arc_execution_e2e::ArcTestNode;
use debank_rpc::debank_trace::{
    BlockStorageDiff, DebankEvent, DebankID, DebankOutPut, DebankTrace, DebankTransaction,
};
use eyre::{ensure, Result};
use jsonrpsee::{core::client::ClientT, rpc_params};

use super::eip7708::{SYSTEM_ADDRESS, TRANSFER_EVENT_SIGNATURE};

/// Fetches a block through `trace_debankBlock` and checks invariants shared by every scenario.
pub async fn trace_debank_block(
    node: &ArcTestNode,
    block_number: u64,
) -> Result<(DebankOutPut, BlockStorageDiff)> {
    let client = node
        .node
        .rpc_client()
        .ok_or_else(|| eyre::eyre!("RPC client not available"))?;
    let output: DebankOutPut = client
        .request(
            "trace_debankBlock",
            rpc_params![BlockId::number(block_number)],
        )
        .await?;
    let state_diff = alloy_rlp::decode_exact::<BlockStorageDiff>(&output.state_diff)?;

    ensure!(
        output.block_file.block.height == block_number,
        "trace_debankBlock returned height {} for requested block {block_number}",
        output.block_file.block.height
    );
    ensure!(
        state_diff.hash == output.header.state_root,
        "StateDiff root does not match the block header"
    );
    if block_number == 0 {
        ensure!(
            state_diff.parent_hash == EMPTY_ROOT_HASH,
            "genesis StateDiff parent root is not the empty root"
        );
    } else {
        let parent: Option<alloy_rpc_types_eth::Block> = client
            .request(
                "eth_getBlockByHash",
                rpc_params![output.block_file.block.parent_id, false],
            )
            .await?;
        let parent = parent.ok_or_else(|| eyre::eyre!("parent block is unavailable"))?;
        ensure!(
            state_diff.parent_hash == parent.header.state_root,
            "StateDiff parent root does not match the parent block header"
        );
    }
    ensure!(
        output.validation_hash == output.block_file.validation().validation_hash,
        "BlockFile validation hash mismatch"
    );

    let trace_ids: HashSet<_> = output
        .block_file
        .traces
        .iter()
        .chain(&output.block_file.error_traces)
        .map(|trace| trace.id.as_str())
        .collect();
    let trace_count = output
        .block_file
        .traces
        .iter()
        .chain(&output.block_file.error_traces)
        .count();
    ensure!(
        trace_ids.len() == trace_count,
        "BlockFile contains duplicate trace ids"
    );
    let transaction_ids: HashSet<_> = output
        .block_file
        .transactions
        .iter()
        .map(|transaction| transaction.id.as_str())
        .collect();
    ensure!(
        transaction_ids.len() == output.block_file.transactions.len(),
        "BlockFile contains duplicate transaction ids"
    );
    for trace in output
        .block_file
        .traces
        .iter()
        .chain(&output.block_file.error_traces)
    {
        ensure!(
            transaction_ids.contains(trace.tx_id.as_str()),
            "trace references a missing transaction"
        );
        ensure!(trace.id == trace.debank_id(), "trace id is not canonical");
        ensure!(
            trace.parent_trace_id.is_empty() || trace_ids.contains(trace.parent_trace_id.as_str()),
            "trace references a missing parent"
        );
    }
    let event_ids: HashSet<_> = output
        .block_file
        .events
        .iter()
        .chain(&output.block_file.error_events)
        .map(|event| event.id.as_str())
        .collect();
    let event_count = output
        .block_file
        .events
        .iter()
        .chain(&output.block_file.error_events)
        .count();
    ensure!(
        event_ids.len() == event_count,
        "BlockFile contains duplicate event ids"
    );
    for event in output
        .block_file
        .events
        .iter()
        .chain(&output.block_file.error_events)
    {
        ensure!(event.id == event.debank_id(), "event id is not canonical");
        ensure!(
            trace_ids.contains(event.parent_trace_id.as_str()),
            "event references a missing parent trace"
        );
    }

    Ok((output, state_diff))
}

/// Checks a traced block and runs an assertion against exactly one transaction hash.
pub async fn assert_debank_block(
    node: &ArcTestNode,
    block_number: u64,
    tx_hash: B256,
    assertion: impl FnOnce(&DebankOutPut, &BlockStorageDiff, &DebankTransaction) -> Result<()>,
) -> Result<()> {
    let (output, state_diff) = trace_debank_block(node, block_number).await?;
    let tx_id = tx_hash.to_string();
    let mut matching_transactions = output
        .block_file
        .transactions
        .iter()
        .filter(|transaction| transaction.id == tx_id);
    let transaction = matching_transactions
        .next()
        .ok_or_else(|| eyre::eyre!("BlockFile does not contain transaction {tx_hash}"))?;
    ensure!(
        matching_transactions.next().is_none(),
        "BlockFile contains transaction {tx_hash} more than once"
    );

    assertion(&output, &state_diff, transaction)
}

/// Transaction-scoped slices of the block-wide trace and event arrays.
pub struct DebankTxView<'a> {
    pub traces: Vec<&'a DebankTrace>,
    pub error_traces: Vec<&'a DebankTrace>,
    pub events: Vec<&'a DebankEvent>,
    pub error_events: Vec<&'a DebankEvent>,
}

pub fn transaction_view<'a>(
    output: &'a DebankOutPut,
    transaction: &DebankTransaction,
) -> DebankTxView<'a> {
    let traces: Vec<_> = output
        .block_file
        .traces
        .iter()
        .filter(|trace| trace.tx_id == transaction.id)
        .collect();
    let error_traces: Vec<_> = output
        .block_file
        .error_traces
        .iter()
        .filter(|trace| trace.tx_id == transaction.id)
        .collect();
    let trace_ids: HashSet<_> = traces
        .iter()
        .chain(&error_traces)
        .map(|trace| trace.id.as_str())
        .collect();
    let events = output
        .block_file
        .events
        .iter()
        .filter(|event| trace_ids.contains(event.parent_trace_id.as_str()))
        .collect();
    let error_events = output
        .block_file
        .error_events
        .iter()
        .filter(|event| trace_ids.contains(event.parent_trace_id.as_str()))
        .collect();
    DebankTxView {
        traces,
        error_traces,
        events,
        error_events,
    }
}

/// Verifies the BlockFile representation of an Arc EIP-7708 native Transfer event.
pub fn assert_eip7708_transfer(
    event: &DebankEvent,
    from: Address,
    to: Address,
    value: U256,
    parent_trace_id: &str,
    pos_in_parent_trace: usize,
    idx: usize,
) -> Result<()> {
    let expected_topics = vec![
        B256::left_padding_from(from.as_slice()).to_string(),
        B256::left_padding_from(to.as_slice()).to_string(),
    ];
    ensure!(
        event.contract_id == SYSTEM_ADDRESS,
        "unexpected event emitter"
    );
    ensure!(
        event.selector == TRANSFER_EVENT_SIGNATURE.to_string(),
        "unexpected event selector"
    );
    ensure!(event.topics == expected_topics, "unexpected event topics");
    ensure!(
        event.data.as_ref() == value.to_be_bytes::<32>().as_slice(),
        "unexpected event data"
    );
    ensure!(
        event.parent_trace_id == parent_trace_id,
        "event is attached to the wrong trace"
    );
    ensure!(
        event.pos_in_parent_trace == pos_in_parent_trace,
        "unexpected event position"
    );
    ensure!(event.idx == idx, "unexpected event log index");
    Ok(())
}
