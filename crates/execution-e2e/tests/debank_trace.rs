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

use alloy_consensus::constants::KECCAK_EMPTY;
use alloy_eips::{eip2935::HISTORY_STORAGE_ADDRESS, BlockId};
use alloy_primitives::{keccak256, Address, U256};
use arc_execution_e2e::{
    actions::{ProduceBlocks, SendTransaction},
    Action, ArcEnvironment, ArcSetup, ArcTestBuilder,
};
use arc_precompiles::system_accounting::SYSTEM_ACCOUNTING_ADDRESS;
use debank_rpc::debank_trace::{BlockStorageDiff, DebankOutPut};
use eyre::{ensure, Result};
use futures_util::future::BoxFuture;
use jsonrpsee::{core::client::ClientT, rpc_params};

#[derive(Debug)]
struct AssertDebankBlock {
    changed_address: Option<Address>,
}

impl AssertDebankBlock {
    const fn empty() -> Self {
        Self {
            changed_address: None,
        }
    }

    const fn with_changed_address(changed_address: Address) -> Self {
        Self {
            changed_address: Some(changed_address),
        }
    }
}

impl Action for AssertDebankBlock {
    fn execute<'a>(&'a mut self, env: &'a mut ArcEnvironment) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let block_number = env.block_number();
            let client = env
                .node()
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

            if let Some(changed_address) = self.changed_address {
                ensure!(
                    output.block_file.transactions.len() == 1,
                    "transaction block did not contain exactly one transaction"
                );
                let changed_account = state_diff
                    .new_accounts
                    .iter()
                    .find(|account| account.address == keccak256(changed_address.as_slice()))
                    .ok_or_else(|| {
                        eyre::eyre!("transaction recipient is missing from StateDiff")
                    })?;
                ensure!(changed_account.balance == U256::from(1));
                ensure!(changed_account.nonce == 0);
                ensure!(changed_account.code_hash == KECCAK_EMPTY);
            } else {
                ensure!(output.block_file.transactions.is_empty());
                ensure!(output.block_file.traces.is_empty());
                ensure!(output.block_file.error_traces.is_empty());
                ensure!(output.block_file.events.is_empty());
                ensure!(output.block_file.error_events.is_empty());
            }

            Ok(())
        })
    }
}

#[tokio::test]
async fn debank_state_pass_captures_pre_and_post_changes_in_empty_block() -> Result<()> {
    reth_tracing::init_test_tracing();

    ArcTestBuilder::new()
        .with_setup(ArcSetup::new())
        .with_action(ProduceBlocks::new(1))
        .with_action(AssertDebankBlock::empty())
        .run()
        .await
}

#[tokio::test]
async fn debank_state_pass_captures_transaction_changes() -> Result<()> {
    reth_tracing::init_test_tracing();

    let recipient = Address::repeat_byte(0xa5);
    ArcTestBuilder::new()
        .with_setup(ArcSetup::new())
        .with_action(SendTransaction::new("transfer").with_to(recipient))
        .with_action(ProduceBlocks::new(1))
        .with_action(AssertDebankBlock::with_changed_address(recipient))
        .run()
        .await
}
