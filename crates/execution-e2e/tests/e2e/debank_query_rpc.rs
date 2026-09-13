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

//! Launched-node coverage for DeBank query RPC behavior.

use alloy_eips::BlockId;
use alloy_primitives::{address, bytes, Address, Bytes, B256, U256};
use alloy_rpc_types_eth::{
    state::{StateOverride, StateOverridesBuilder},
    BlockNumberOrTag, BlockOverrides, TransactionInput, TransactionRequest,
};
use arc_execution_e2e::{ArcSetup, ArcTestNode, TxKind};
use debank_rpc::{
    erc20_handle::NATIVE_TOKEN_ADDRESS,
    types::{MultiCallErrorCode, MultiCallResp, PreErrorCode, PreResult, SingleCallResult},
};
use eyre::{ensure, Result};
use jsonrpsee::{core::client::ClientT, rpc_params};

use super::helpers::debank::trace_debank_block;

const COUNTER_ADDRESS: Address = address!("000000000000000000000000000000000000c001");
const NUMBER_ADDRESS: Address = address!("000000000000000000000000000000000000c002");
const REVERTER_ADDRESS: Address = address!("000000000000000000000000000000000000c003");
const OVERRIDDEN_BLOCK_NUMBER: u64 = 0x1234;

// Return slot 0, then increment it. The previous value lets the tests distinguish sequential
// commits (40, 41) from isolated calls (40, 40).
const COUNTER_RUNTIME: Bytes = bytes!("5f54805f526001015f5560205ff3");
const NUMBER_RUNTIME: Bytes = bytes!("435f5260205ff3");
// Revert with the raw UTF-8 reason "probe".
const REVERTER_RUNTIME: Bytes = bytes!("6470726f62655f526005601bfd");
const DECIMALS_SELECTOR: Bytes = bytes!("313ce567");

fn call(from: Address, to: Address, input: Bytes) -> TransactionRequest {
    TransactionRequest {
        from: Some(from),
        to: Some(TxKind::Call(to)),
        gas: Some(200_000),
        input: TransactionInput::new(input),
        ..Default::default()
    }
}

fn query_state_overrides() -> StateOverride {
    StateOverridesBuilder::default()
        .with_code(COUNTER_ADDRESS, COUNTER_RUNTIME.clone())
        .with_state_diff(COUNTER_ADDRESS, [(B256::ZERO, storage_word(40))])
        .with_code(NUMBER_ADDRESS, NUMBER_RUNTIME.clone())
        .with_code(REVERTER_ADDRESS, REVERTER_RUNTIME.clone())
        .build()
}

fn query_block_overrides() -> BlockOverrides {
    BlockOverrides::default().with_number(U256::from(OVERRIDDEN_BLOCK_NUMBER))
}

fn storage_word(value: u64) -> B256 {
    B256::from(U256::from(value).to_be_bytes::<32>())
}

fn pre_success_word(result: &PreResult) -> Result<U256> {
    ensure!(result.error.is_none(), "pre_traceMany call failed");
    ensure!(result.gas_used > 0, "successful call used no gas");
    ensure!(result.logs.is_empty(), "probe unexpectedly emitted logs");
    ensure!(
        result.trace.len() == 1,
        "probe did not produce one root trace"
    );
    let trace = &result.trace[0].trace;
    ensure!(
        trace.trace_address.is_empty(),
        "probe trace is not the root"
    );
    let output = trace
        .result
        .as_ref()
        .ok_or_else(|| eyre::eyre!("successful root trace has no result"))?
        .output();
    Ok(U256::from_be_slice(output))
}

fn assert_multi_success_word(
    result: &SingleCallResult,
    expected: U256,
    expect_gas: bool,
) -> Result<()> {
    ensure!(result.code == MultiCallErrorCode::Success as i32);
    ensure!(result.err.is_empty());
    ensure!(!result.from_cache);
    if expect_gas {
        ensure!(result.gas_used > 0, "EVM call used no gas");
    } else {
        ensure!(result.gas_used == 0, "native sentinel consumed EVM gas");
    }
    ensure!(U256::from_be_slice(&result.result) == expected);
    Ok(())
}

#[tokio::test]
async fn pre_trace_many_commits_state_and_applies_overrides() -> Result<()> {
    reth_tracing::init_test_tracing();

    let mut node = ArcTestNode::start(ArcSetup::new()).await?;
    node.produce_block().await?;
    let from = node.wallet_signer(0)?.address();
    let client = node
        .node
        .rpc_client()
        .ok_or_else(|| eyre::eyre!("RPC client not available"))?;

    let requests = vec![
        call(from, COUNTER_ADDRESS, Bytes::new()),
        call(from, REVERTER_ADDRESS, Bytes::new()),
        call(from, COUNTER_ADDRESS, Bytes::new()),
        call(from, NUMBER_ADDRESS, Bytes::new()),
    ];
    let results: Vec<PreResult> = client
        .request(
            "pre_traceMany",
            rpc_params![
                requests,
                BlockId::latest(),
                Some(query_state_overrides()),
                Some(Box::new(query_block_overrides()))
            ],
        )
        .await?;

    ensure!(results.len() == 4);
    ensure!(pre_success_word(&results[0])? == U256::from(40));
    ensure!(pre_success_word(&results[2])? == U256::from(41));
    ensure!(pre_success_word(&results[3])? == U256::from(OVERRIDDEN_BLOCK_NUMBER));

    let reverted = &results[1];
    let error = reverted
        .error
        .as_ref()
        .ok_or_else(|| eyre::eyre!("reverting call has no PreError"))?;
    ensure!(error.code == PreErrorCode::Reverted as i64);
    ensure!(error.msg == "revert");
    ensure!(reverted.trace.is_empty());
    ensure!(reverted.logs.is_empty());
    ensure!(reverted.gas_used == 0);

    ensure!(
        node.storage_at(COUNTER_ADDRESS, U256::ZERO, None).await? == B256::ZERO,
        "pre_traceMany modified canonical state"
    );
    Ok(())
}

#[tokio::test]
async fn eth_multi_call_isolates_calls_and_fast_fails() -> Result<()> {
    reth_tracing::init_test_tracing();

    let mut node = ArcTestNode::start(ArcSetup::new()).await?;
    node.produce_block().await?;
    let anchor = node.get_block(BlockNumberOrTag::Latest).await?;
    let from = node.wallet_signer(0)?.address();
    let client = node
        .node
        .rpc_client()
        .ok_or_else(|| eyre::eyre!("RPC client not available"))?;

    let response: MultiCallResp = client
        .request(
            "eth_multiCall",
            rpc_params![
                vec![
                    call(from, NATIVE_TOKEN_ADDRESS, DECIMALS_SELECTOR.clone()),
                    call(from, COUNTER_ADDRESS, Bytes::new()),
                    call(from, COUNTER_ADDRESS, Bytes::new()),
                    call(from, NUMBER_ADDRESS, Bytes::new()),
                    call(from, REVERTER_ADDRESS, Bytes::new()),
                ],
                BlockId::latest(),
                false,
                false,
                true,
                Some(query_state_overrides()),
                Some(Box::new(query_block_overrides()))
            ],
        )
        .await?;

    ensure!(response.results.len() == 5);
    assert_multi_success_word(&response.results[0], U256::from(18), false)?;
    assert_multi_success_word(&response.results[1], U256::from(40), true)?;
    assert_multi_success_word(&response.results[2], U256::from(40), true)?;
    assert_multi_success_word(
        &response.results[3],
        U256::from(OVERRIDDEN_BLOCK_NUMBER),
        true,
    )?;
    let reverted = &response.results[4];
    ensure!(reverted.code == MultiCallErrorCode::EVMReverted as i32);
    ensure!(reverted.err == "probe");
    ensure!(!reverted.from_cache);
    ensure!(reverted.result.is_empty());
    ensure!(reverted.gas_used > 0);

    ensure!(response.stats.block_num == anchor.header.number);
    ensure!(response.stats.block_hash == anchor.header.hash);
    ensure!(response.stats.block_time == anchor.header.timestamp);
    ensure!(!response.stats.success);
    ensure!(!response.stats.cache_enabled);

    let fast_fail: MultiCallResp = client
        .request(
            "eth_multiCall",
            rpc_params![
                vec![
                    call(from, REVERTER_ADDRESS, Bytes::new()),
                    call(from, NUMBER_ADDRESS, Bytes::new()),
                ],
                BlockId::latest(),
                true,
                false,
                false,
                Some(query_state_overrides()),
                Some(Box::new(query_block_overrides()))
            ],
        )
        .await?;
    ensure!(fast_fail.results.len() == 2);
    ensure!(fast_fail.results[0].code == MultiCallErrorCode::EVMReverted as i32);
    let skipped = &fast_fail.results[1];
    ensure!(skipped.code == MultiCallErrorCode::EVMFastFailed as i32);
    ensure!(skipped.err == "skipped due to fast_fail");
    ensure!(!skipped.from_cache);
    ensure!(skipped.result.is_empty());
    ensure!(skipped.gas_used == 0);
    ensure!(skipped.time_cost == 0.0);
    ensure!(!fast_fail.stats.success);
    ensure!(fast_fail.stats.cache_enabled);

    ensure!(
        node.storage_at(COUNTER_ADDRESS, U256::ZERO, None).await? == B256::ZERO,
        "eth_multiCall modified canonical state"
    );
    Ok(())
}

#[tokio::test]
async fn trace_debank_block_serves_genesis_via_rpc() -> Result<()> {
    reth_tracing::init_test_tracing();

    let node = ArcTestNode::start(ArcSetup::new()).await?;
    let (output, state_diff) = trace_debank_block(&node, 0).await?;

    ensure!(output.block_file.block.height == 0);
    ensure!(!output.block_file.transactions.is_empty());
    ensure!(output.block_file.transactions.len() == output.block_file.traces.len());
    ensure!(output.block_file.error_traces.is_empty());
    ensure!(output.block_file.events.is_empty());
    ensure!(output.block_file.error_events.is_empty());
    ensure!(!state_diff.new_accounts.is_empty());
    Ok(())
}
