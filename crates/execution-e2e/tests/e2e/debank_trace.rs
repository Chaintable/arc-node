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

//! End-to-end coverage for the canonical `trace_debankBlock` state pass.

use super::helpers::debank::{assert_debank_block, trace_debank_block};

use alloy_consensus::constants::KECCAK_EMPTY;
use alloy_eips::eip2935::HISTORY_STORAGE_ADDRESS;
use alloy_primitives::{keccak256, Address, U256};
use alloy_rpc_types_eth::{BlockNumberOrTag, TransactionRequest};
use arc_execution_e2e::{ArcSetup, ArcTestNode, TxKind};
use arc_precompiles::system_accounting::SYSTEM_ACCOUNTING_ADDRESS;
use eyre::{ensure, Result};

#[tokio::test]
async fn debank_state_pass_captures_pre_and_post_changes_in_empty_block() -> Result<()> {
    reth_tracing::init_test_tracing();

    let mut node = ArcTestNode::start(ArcSetup::new()).await?;
    node.produce_block().await?;
    let block_number = node
        .get_block(BlockNumberOrTag::Latest)
        .await?
        .header
        .number;
    let (output, state_diff) = trace_debank_block(&node, block_number).await?;

    let system_contracts = vec![HISTORY_STORAGE_ADDRESS, SYSTEM_ACCOUNTING_ADDRESS];
    ensure!(
        output.block_file.storage_contracts == system_contracts,
        "block hooks produced unexpected storage contracts: {:?}",
        output.block_file.storage_contracts
    );
    let storage_diff_addresses: Vec<_> = state_diff
        .storage_diffs
        .iter()
        .map(|diff| diff.address)
        .collect();
    let mut expected_storage_diff_addresses: Vec<_> = system_contracts
        .iter()
        .map(|address| keccak256(address.as_slice()))
        .collect();
    expected_storage_diff_addresses.sort_unstable();
    ensure!(
        storage_diff_addresses == expected_storage_diff_addresses,
        "StateDiff does not contain both Arc block hooks"
    );
    ensure!(
        state_diff
            .storage_diffs
            .iter()
            .all(|account| account.diffs.len() == 1),
        "each Arc block hook must write exactly one storage slot"
    );
    ensure!(output.block_file.transactions.is_empty());
    ensure!(output.block_file.traces.is_empty());
    ensure!(output.block_file.error_traces.is_empty());
    ensure!(output.block_file.events.is_empty());
    ensure!(output.block_file.error_events.is_empty());

    Ok(())
}

#[tokio::test]
async fn debank_state_pass_captures_transaction_changes() -> Result<()> {
    reth_tracing::init_test_tracing();

    let recipient = Address::repeat_byte(0xa5);
    let mut node = ArcTestNode::start(ArcSetup::new()).await?;
    let signer = node.wallet_signer(0)?;
    let tx_hash = node
        .send_tx(
            signer.clone(),
            TransactionRequest {
                from: Some(signer.address()),
                to: Some(TxKind::Call(recipient)),
                value: Some(U256::from(1)),
                ..Default::default()
            },
        )
        .await?;
    node.produce_block().await?;
    let receipt = node.get_receipt(tx_hash).await?;
    ensure!(receipt.status(), "native transfer failed");
    let block_number = receipt
        .block_number
        .ok_or_else(|| eyre::eyre!("mined transaction receipt has no block number"))?;

    assert_debank_block(
        &node,
        block_number,
        tx_hash,
        move |output, state_diff, _transaction| {
            let system_contracts = vec![HISTORY_STORAGE_ADDRESS, SYSTEM_ACCOUNTING_ADDRESS];
            ensure!(
                output.block_file.storage_contracts == system_contracts,
                "block hooks produced unexpected storage contracts: {:?}",
                output.block_file.storage_contracts
            );
            let storage_diff_addresses: Vec<_> = state_diff
                .storage_diffs
                .iter()
                .map(|diff| diff.address)
                .collect();
            let mut expected_storage_diff_addresses: Vec<_> = system_contracts
                .iter()
                .map(|address| keccak256(address.as_slice()))
                .collect();
            expected_storage_diff_addresses.sort_unstable();
            ensure!(
                storage_diff_addresses == expected_storage_diff_addresses,
                "StateDiff does not contain both Arc block hooks"
            );
            ensure!(
                state_diff
                    .storage_diffs
                    .iter()
                    .all(|account| account.diffs.len() == 1),
                "each Arc block hook must write exactly one storage slot"
            );
            ensure!(
                output.block_file.transactions.len() == 1,
                "transaction block did not contain exactly one transaction"
            );
            let changed_account = state_diff
                .new_accounts
                .iter()
                .find(|account| account.address == keccak256(recipient.as_slice()))
                .ok_or_else(|| eyre::eyre!("transaction recipient is missing from StateDiff"))?;
            ensure!(changed_account.balance == U256::from(1));
            ensure!(changed_account.nonce == 0);
            ensure!(changed_account.code_hash == KECCAK_EMPTY);
            Ok(())
        },
    )
    .await
}
