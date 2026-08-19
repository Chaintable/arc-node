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

//! The ignored test launches the Arc v0.7.3 writer and captures its real
//! `trace_debankBlock` payloads. It deliberately writes only producer output;
//! the companion Python script applies the background-tracer object encoding.

mod helpers;

use alloy_consensus::constants::{EMPTY_ROOT_HASH, KECCAK_EMPTY};
use alloy_eips::{eip2935::HISTORY_STORAGE_ADDRESS, eip7685::RequestsOrHash, BlockId};
use alloy_primitives::{address, keccak256, Address, B256, U256};
use alloy_rpc_types_engine::{ForkchoiceState, PayloadAttributes};
use arc_execution_e2e::{
    actions::{assert_valid_or_syncing, SendTransaction},
    Action, ArcEnvironment, ArcSetup, ArcTestBuilder, BlockInfo,
};
use arc_precompiles::system_accounting::SYSTEM_ACCOUNTING_ADDRESS;
use debank_rpc::debank_trace::{BlockStorageDiff, DebankOutPut};
use eyre::{ensure, Context, Result};
use futures_util::future::BoxFuture;
use helpers::{
    constants::{SYSTEM_ADDRESS, WALLET_FIRST_ADDRESS},
    contracts::{
        create2_balance_probe_calldata, create2_with_balance_probe, reverting_constructor_code,
    },
    debank::{assert_eip7708_transfer, current_mainnet_test_chain_spec, transaction_view},
};
use jsonrpsee::{core::client::ClientT, rpc_params};
use reth_chainspec::EthChainSpec;
use reth_ethereum::node::EthEngineTypes;
use reth_rpc_api::clients::EngineApiClient;
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

const OUTPUT_ENV: &str = "ARC_A1B_CAPTURE_DIR";
const FEE_RECIPIENT: Address = address!("0x65E0a200006D4FF91bD59F9694220dafc49dbBC1");

fn assert_storage_contracts(
    output: &DebankOutPut,
    state_diff: &BlockStorageDiff,
    expected_contracts: &[Address],
) -> Result<()> {
    ensure!(
        output.block_file.storage_contracts.as_slice() == expected_contracts,
        "block {}: unexpected storage contracts",
        output.header.number
    );

    let mut expected_hashes: Vec<_> = expected_contracts
        .iter()
        .map(|address| keccak256(address.as_slice()))
        .collect();
    expected_hashes.sort_unstable();
    let storage_hashes: Vec<_> = state_diff
        .storage_diffs
        .iter()
        .map(|diff| diff.address)
        .collect();
    ensure!(
        storage_hashes == expected_hashes,
        "block {}: StateDiff storage account set mismatch",
        output.header.number
    );
    Ok(())
}

fn assert_system_hook_state(output: &DebankOutPut, state_diff: &BlockStorageDiff) -> Result<()> {
    assert_storage_contracts(
        output,
        state_diff,
        &[HISTORY_STORAGE_ADDRESS, SYSTEM_ACCOUNTING_ADDRESS],
    )?;
    ensure!(
        state_diff
            .storage_diffs
            .iter()
            .all(|diff| diff.diffs.len() == 1),
        "block {}: each Arc hook must write exactly one storage slot",
        output.header.number
    );
    Ok(())
}

fn assert_genesis(output: &DebankOutPut, state_diff: &BlockStorageDiff) -> Result<()> {
    ensure!(
        !output.block_file.transactions.is_empty()
            && output.block_file.transactions.len() == output.block_file.traces.len(),
        "genesis alloc was not represented as one root trace per synthetic transaction"
    );
    ensure!(
        output.block_file.error_traces.is_empty()
            && output.block_file.events.is_empty()
            && output.block_file.error_events.is_empty(),
        "genesis alloc unexpectedly contains events or failed traces"
    );
    ensure!(
        !state_diff.new_accounts.is_empty()
            && !state_diff.new_codes.is_empty()
            && !state_diff.storage_diffs.is_empty(),
        "genesis StateDiff is not a full alloc capture"
    );
    Ok(())
}

fn assert_empty_hooks(output: &DebankOutPut, state_diff: &BlockStorageDiff) -> Result<()> {
    ensure!(
        output.block_file.transactions.is_empty()
            && output.block_file.traces.is_empty()
            && output.block_file.error_traces.is_empty()
            && output.block_file.events.is_empty()
            && output.block_file.error_events.is_empty(),
        "empty-hooks scenario contains transaction data"
    );
    ensure!(
        state_diff.new_codes.is_empty(),
        "empty-hooks scenario contains code"
    );
    let mut expected_accounts = vec![
        keccak256(HISTORY_STORAGE_ADDRESS.as_slice()),
        keccak256(SYSTEM_ACCOUNTING_ADDRESS.as_slice()),
    ];
    expected_accounts.sort_unstable();
    let actual_accounts: Vec<_> = state_diff
        .new_accounts
        .iter()
        .map(|account| account.address)
        .collect();
    ensure!(
        actual_accounts == expected_accounts,
        "empty-hooks scenario contains non-hook account changes"
    );
    assert_system_hook_state(output, state_diff)
}

fn assert_native_transfer(output: &DebankOutPut, state_diff: &BlockStorageDiff) -> Result<()> {
    let recipient = Address::repeat_byte(0xa5);
    ensure!(
        output.block_file.transactions.len() == 1,
        "native-transfer scenario must contain one transaction"
    );
    let transaction = &output.block_file.transactions[0];
    ensure!(
        transaction.status
            && transaction.nonce == 0
            && transaction.from == WALLET_FIRST_ADDRESS
            && transaction.to == recipient
            && transaction.value == U256::from(1),
        "native-transfer transaction does not match the intended scenario"
    );
    let view = transaction_view(output, transaction);
    ensure!(
        view.traces.len() == 1
            && view.error_traces.is_empty()
            && view.events.len() == 1
            && view.error_events.is_empty(),
        "native-transfer scenario must contain one successful root trace and event"
    );
    let root = view.traces[0];
    ensure!(
        root.parent_trace_id.is_empty()
            && root.trace_address.is_empty()
            && root.call_create_type == "call"
            && root.call_type == "call"
            && root.from_addr == WALLET_FIRST_ADDRESS
            && root.to_addr == recipient
            && root.value == U256::from(1)
            && root.subtraces == 0
            && root.error.is_empty(),
        "native-transfer root CALL trace mismatch"
    );
    assert_eip7708_transfer(
        view.events[0],
        WALLET_FIRST_ADDRESS,
        recipient,
        U256::from(1),
        &root.id,
        0,
        0,
    )?;
    ensure!(
        view.events[0].contract_id == SYSTEM_ADDRESS,
        "native-transfer event is not emitted by SYSTEM_ADDRESS"
    );
    let recipient_account = state_diff
        .new_accounts
        .iter()
        .find(|account| account.address == keccak256(recipient.as_slice()))
        .ok_or_else(|| eyre::eyre!("native-transfer recipient is missing from StateDiff"))?;
    ensure!(
        recipient_account.balance == U256::from(1)
            && recipient_account.nonce == 0
            && recipient_account.code_hash == KECCAK_EMPTY,
        "native-transfer recipient state mismatch"
    );
    assert_system_hook_state(output, state_diff)
}

fn assert_create2(output: &DebankOutPut, state_diff: &BlockStorageDiff) -> Result<()> {
    let probe = WALLET_FIRST_ADDRESS.create(1);
    let child = probe.create2([0_u8; 32], keccak256([0_u8]));
    ensure!(
        output.block_file.transactions.len() == 2,
        "create2 scenario must contain deploy and run transactions"
    );
    let deploy = output
        .block_file
        .transactions
        .iter()
        .find(|transaction| transaction.nonce == 1)
        .ok_or_else(|| eyre::eyre!("create2 deploy transaction is missing"))?;
    let run = output
        .block_file
        .transactions
        .iter()
        .find(|transaction| transaction.nonce == 2)
        .ok_or_else(|| eyre::eyre!("create2 run transaction is missing"))?;
    ensure!(
        deploy.status
            && deploy.from == WALLET_FIRST_ADDRESS
            && deploy.to == probe
            && deploy.value == U256::from(1),
        "create2 deploy transaction mismatch"
    );
    let deploy_view = transaction_view(output, deploy);
    ensure!(
        deploy_view.traces.len() == 1
            && deploy_view.error_traces.is_empty()
            && deploy_view.events.len() == 1
            && deploy_view.error_events.is_empty(),
        "create2 deploy must contain one successful CREATE root and event"
    );
    let deploy_root = deploy_view.traces[0];
    ensure!(
        deploy_root.parent_trace_id.is_empty()
            && deploy_root.trace_address.is_empty()
            && deploy_root.call_create_type == "create"
            && deploy_root.call_type.is_empty()
            && deploy_root.from_addr == WALLET_FIRST_ADDRESS
            && deploy_root.to_addr == probe
            && deploy_root.value == U256::from(1)
            && deploy_root.error.is_empty(),
        "create2 deploy root CREATE trace mismatch"
    );
    assert_eip7708_transfer(
        deploy_view.events[0],
        WALLET_FIRST_ADDRESS,
        probe,
        U256::from(1),
        &deploy_root.id,
        0,
        0,
    )?;

    ensure!(
        run.status
            && run.from == WALLET_FIRST_ADDRESS
            && run.to == probe
            && run.value == U256::ZERO,
        "create2 run transaction mismatch"
    );
    let run_view = transaction_view(output, run);
    ensure!(
        run_view.traces.len() == 2
            && run_view.error_traces.is_empty()
            && run_view.events.len() == 1
            && run_view.error_events.is_empty(),
        "create2 run must contain successful CALL/CREATE2 traces and one event"
    );
    let call_root = run_view
        .traces
        .iter()
        .copied()
        .find(|trace| trace.parent_trace_id.is_empty())
        .ok_or_else(|| eyre::eyre!("create2 root CALL trace is missing"))?;
    let create_child = run_view
        .traces
        .iter()
        .copied()
        .find(|trace| trace.call_create_type == "create")
        .ok_or_else(|| eyre::eyre!("internal CREATE2 trace is missing"))?;
    ensure!(
        call_root.call_create_type == "call"
            && call_root.call_type == "call"
            && call_root.from_addr == WALLET_FIRST_ADDRESS
            && call_root.to_addr == probe
            && call_root.subtraces == 1
            && call_root.self_storage_change
            && call_root.storage_change
            && call_root.error.is_empty(),
        "create2 run root CALL trace mismatch"
    );
    ensure!(
        create_child.parent_trace_id == call_root.id
            && create_child.trace_address == [0]
            && create_child.pos_in_parent_trace == 0
            && create_child.call_type.is_empty()
            && create_child.from_addr == probe
            && create_child.to_addr == child
            && create_child.value == U256::from(1)
            && create_child.error.is_empty(),
        "internal CREATE2 child trace mismatch"
    );
    assert_eip7708_transfer(
        run_view.events[0],
        probe,
        child,
        U256::from(1),
        &create_child.id,
        0,
        1,
    )?;
    assert_storage_contracts(
        output,
        state_diff,
        &[HISTORY_STORAGE_ADDRESS, SYSTEM_ACCOUNTING_ADDRESS, probe],
    )?;

    let probe_account = state_diff
        .new_accounts
        .iter()
        .find(|account| account.address == keccak256(probe.as_slice()))
        .ok_or_else(|| eyre::eyre!("create2 probe account is missing from StateDiff"))?;
    ensure!(
        probe_account.code_hash != KECCAK_EMPTY
            && state_diff
                .new_codes
                .iter()
                .any(|code| { code.code_hash == probe_account.code_hash && !code.code.is_empty() }),
        "create2 probe runtime code is missing from StateDiff"
    );
    let child_account = state_diff
        .new_accounts
        .iter()
        .find(|account| account.address == keccak256(child.as_slice()))
        .ok_or_else(|| eyre::eyre!("CREATE2 child account is missing from StateDiff"))?;
    ensure!(
        child_account.balance == U256::from(1)
            && child_account.nonce == 1
            && child_account.code_hash == KECCAK_EMPTY,
        "CREATE2 child state mismatch"
    );
    let probe_storage = state_diff
        .storage_diffs
        .iter()
        .find(|account| account.address == keccak256(probe.as_slice()))
        .ok_or_else(|| eyre::eyre!("create2 probe storage is missing from StateDiff"))?;
    let slot_zero = keccak256([0_u8; 32]);
    ensure!(
        probe_storage.diffs.iter().any(|diff| {
            diff.index == slot_zero && diff.value == U256::from_be_slice(child.as_slice())
        }),
        "create2 probe did not persist the child address"
    );
    Ok(())
}

fn assert_failed_create(output: &DebankOutPut, state_diff: &BlockStorageDiff) -> Result<()> {
    let failed_create = WALLET_FIRST_ADDRESS.create(3);
    ensure!(
        output.block_file.transactions.len() == 1,
        "failed-create scenario must contain one transaction"
    );
    let transaction = &output.block_file.transactions[0];
    ensure!(
        !transaction.status
            && transaction.nonce == 3
            && transaction.from == WALLET_FIRST_ADDRESS
            && transaction.to == failed_create
            && transaction.value == U256::from(1_000_000),
        "failed CREATE transaction or receipt-derived status mismatch"
    );
    let view = transaction_view(output, transaction);
    ensure!(
        view.traces.is_empty()
            && view.events.is_empty()
            && view.error_traces.len() == 1
            && view.error_events.len() == 1,
        "failed CREATE must appear only in the error trace/event buckets"
    );
    let root = view.error_traces[0];
    ensure!(
        root.parent_trace_id.is_empty()
            && root.trace_address.is_empty()
            && root.call_create_type == "create"
            && root.call_type.is_empty()
            && root.from_addr == WALLET_FIRST_ADDRESS
            && root.to_addr == failed_create
            && root.value == U256::from(1_000_000)
            && root.subtraces == 0
            && root.error == "Reverted",
        "failed CREATE root trace mismatch"
    );
    assert_eip7708_transfer(
        view.error_events[0],
        WALLET_FIRST_ADDRESS,
        failed_create,
        U256::from(1_000_000),
        &root.id,
        0,
        0,
    )?;
    let failed_hash = keccak256(failed_create.as_slice());
    ensure!(
        state_diff
            .new_accounts
            .iter()
            .all(|account| account.address != failed_hash)
            && state_diff
                .storage_diffs
                .iter()
                .all(|account| account.address != failed_hash)
            && state_diff.new_codes.is_empty(),
        "failed CREATE leaked account, code, or storage into StateDiff"
    );
    assert_system_hook_state(output, state_diff)
}

fn assert_fixture_scenario(
    label: &str,
    output: &DebankOutPut,
    state_diff: &BlockStorageDiff,
) -> Result<()> {
    ensure!(
        state_diff.deleted_accounts.is_empty(),
        "{label} scenario unexpectedly deletes accounts"
    );
    match label {
        "genesis" => assert_genesis(output, state_diff),
        "empty-hooks" => assert_empty_hooks(output, state_diff),
        "native-transfer" => assert_native_transfer(output, state_diff),
        "create2" => assert_create2(output, state_diff),
        "failed-create" => assert_failed_create(output, state_diff),
        _ => Err(eyre::eyre!("unknown fixture scenario {label}")),
    }
}

fn output_dir() -> Result<PathBuf> {
    std::env::var_os(OUTPUT_ENV)
        .map(PathBuf::from)
        .ok_or_else(|| eyre::eyre!("{OUTPUT_ENV} is required"))
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .wrap_err_with(|| format!("create {}", path.display()))?;
    file.write_all(bytes)
        .wrap_err_with(|| format!("write {}", path.display()))
}

#[derive(Debug)]
struct CaptureDebankBlock {
    label: &'static str,
}

impl CaptureDebankBlock {
    const fn new(label: &'static str) -> Self {
        Self { label }
    }
}

impl Action for CaptureDebankBlock {
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

            ensure!(
                output.header.number == block_number,
                "producer returned block {} for requested block {block_number}",
                output.header.number
            );
            let state_diff = alloy_rlp::decode_exact::<BlockStorageDiff>(&output.state_diff)?;
            ensure!(
                state_diff.hash == output.header.state_root,
                "block {block_number}: StateDiff root does not match Header"
            );
            ensure!(
                output.validation_hash == output.block_file.validation().validation_hash,
                "block {block_number}: BlockFile validation hash mismatch"
            );

            let canonical: Option<alloy_rpc_types_eth::Block> = client
                .request(
                    "eth_getBlockByNumber",
                    rpc_params![format!("{block_number:#x}"), false],
                )
                .await?;
            let canonical = canonical
                .ok_or_else(|| eyre::eyre!("block {block_number}: canonical block unavailable"))?;
            ensure!(
                canonical.header.hash == output.header.hash
                    && canonical.header.state_root == output.header.state_root
                    && canonical.header.parent_hash == output.header.parent_hash,
                "block {block_number}: trace Header does not match canonical block"
            );
            ensure!(
                output.block_file.block.id == canonical.header.hash
                    && output.block_file.block.parent_id == canonical.header.parent_hash,
                "block {block_number}: BlockFile identity does not match canonical block"
            );

            if block_number == 0 {
                ensure!(
                    state_diff.parent_hash == EMPTY_ROOT_HASH,
                    "genesis StateDiff parent root is not the empty trie root"
                );
            } else {
                let parent: Option<alloy_rpc_types_eth::Block> = client
                    .request(
                        "eth_getBlockByHash",
                        rpc_params![canonical.header.parent_hash, false],
                    )
                    .await?;
                let parent = parent.ok_or_else(|| {
                    eyre::eyre!("block {block_number}: canonical parent unavailable")
                })?;
                ensure!(
                    state_diff.parent_hash == parent.header.state_root,
                    "block {block_number}: StateDiff parent root mismatch"
                );
            }

            assert_fixture_scenario(self.label, &output, &state_diff)?;

            let path = output_dir()?.join(format!("{block_number}-{}.json", self.label));
            write_new(&path, &serde_json::to_vec_pretty(&output)?)?;
            Ok(())
        })
    }
}

/// The standard e2e helper randomizes `prev_randao`; fixed fixtures need the
/// same real payload-builder path with deterministic payload attributes.
#[derive(Debug)]
struct ProduceDeterministicBlocks(u64);

impl ProduceDeterministicBlocks {
    const fn new(count: u64) -> Self {
        Self(count)
    }
}

impl Action for ProduceDeterministicBlocks {
    fn execute<'a>(&'a mut self, env: &'a mut ArcEnvironment) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            for _ in 0..self.0 {
                let current = env.current_block().clone();
                let next_number = current.number + 1;
                let engine_client = env.node().inner.auth_server_handle().http_client();
                let forkchoice = ForkchoiceState {
                    head_block_hash: current.hash,
                    safe_block_hash: current.hash,
                    finalized_block_hash: current.hash,
                };
                let attributes = PayloadAttributes {
                    timestamp: current.timestamp + 1,
                    prev_randao: B256::repeat_byte(u8::try_from(next_number)?),
                    suggested_fee_recipient: FEE_RECIPIENT,
                    withdrawals: Some(Vec::new()),
                    parent_beacon_block_root: Some(B256::ZERO),
                };
                let fcu = EngineApiClient::<EthEngineTypes>::fork_choice_updated_v3(
                    &engine_client,
                    forkchoice,
                    Some(attributes),
                )
                .await?;
                assert_valid_or_syncing(&fcu.payload_status.status, "fixture build FCU")?;
                let payload_id = fcu
                    .payload_id
                    .ok_or_else(|| eyre::eyre!("fixture build FCU returned no payload ID"))?;

                // The fixture chain activates Osaka at genesis, so V5 is the
                // required and deterministic getPayload version.
                let envelope =
                    EngineApiClient::<EthEngineTypes>::get_payload_v5(&engine_client, payload_id)
                        .await?;
                let payload = envelope.execution_payload.clone();
                let requests = envelope.execution_requests.clone();
                let block_hash = payload.payload_inner.payload_inner.block_hash;
                let block_number = payload.payload_inner.payload_inner.block_number;
                let timestamp = payload.payload_inner.payload_inner.timestamp;
                ensure!(block_number == next_number, "unexpected payload height");

                let submitted = EngineApiClient::<EthEngineTypes>::new_payload_v4(
                    &engine_client,
                    payload,
                    Vec::new(),
                    B256::ZERO,
                    RequestsOrHash::Requests(requests),
                )
                .await?;
                assert_valid_or_syncing(&submitted.status, "fixture newPayload")?;
                let finalized = EngineApiClient::<EthEngineTypes>::fork_choice_updated_v3(
                    &engine_client,
                    ForkchoiceState {
                        head_block_hash: block_hash,
                        safe_block_hash: block_hash,
                        finalized_block_hash: block_hash,
                    },
                    None,
                )
                .await?;
                assert_valid_or_syncing(&finalized.payload_status.status, "fixture finalize FCU")?;
                env.set_current_block(BlockInfo::new(block_hash, block_number, timestamp));
            }
            Ok(())
        })
    }
}

#[tokio::test]
#[ignore = "writes Arc producer fixtures to ARC_A1B_CAPTURE_DIR"]
async fn export_arc_a1b_fixtures() -> Result<()> {
    reth_tracing::init_test_tracing();

    let capture_dir = output_dir()?;
    fs::create_dir_all(&capture_dir)?;
    ensure!(
        fs::read_dir(&capture_dir)?.next().is_none(),
        "capture directory must be empty: {}",
        capture_dir.display()
    );

    let chain_spec = current_mainnet_test_chain_spec();
    let chain_id = chain_spec.chain().id();
    let native_recipient = Address::repeat_byte(0xa5);
    // The native transfer consumes wallet nonce 0 before the deployment.
    let probe = WALLET_FIRST_ADDRESS.create(1);
    let child = probe.create2([0_u8; 32], alloy_primitives::keccak256([0_u8]));
    // deploy_probe and run_probe consume nonces 1 and 2.
    let failed_create = WALLET_FIRST_ADDRESS.create(3);
    let context = serde_json::json!({
        "chain": "arc-localdev-current-mainnet-hardforks",
        "chain_id": chain_id,
        "wallet": WALLET_FIRST_ADDRESS,
        "native_recipient": native_recipient,
        "create2_probe": probe,
        "create2_child": child,
        "failed_create_address": failed_create,
        "comparison_normalization": { "process_start_timestamp": 0 },
    });
    write_new(
        &capture_dir.join("capture-context.json"),
        &serde_json::to_vec_pretty(&context)?,
    )?;

    ArcTestBuilder::new()
        .with_setup(
            ArcSetup::new()
                .with_chain_spec(chain_spec)
                .with_legacy_state_root_task_enabled(),
        )
        .with_action(CaptureDebankBlock::new("genesis"))
        .with_action(ProduceDeterministicBlocks::new(1))
        .with_action(CaptureDebankBlock::new("empty-hooks"))
        .with_action(SendTransaction::new("native_transfer").with_to(native_recipient))
        .with_action(ProduceDeterministicBlocks::new(1))
        .with_action(CaptureDebankBlock::new("native-transfer"))
        .with_action(
            SendTransaction::new("deploy_probe")
                .with_create()
                .with_data(create2_with_balance_probe())
                .with_value(U256::from(1))
                .with_gas_limit(200_000),
        )
        .with_action(
            SendTransaction::new("run_probe")
                .with_to(probe)
                .with_data(create2_balance_probe_calldata(probe))
                .with_value(U256::ZERO)
                .with_gas_limit(200_000),
        )
        .with_action(ProduceDeterministicBlocks::new(1))
        .with_action(CaptureDebankBlock::new("create2"))
        .with_action(
            SendTransaction::new("failed_create")
                .with_create()
                .with_data(reverting_constructor_code())
                .with_value(U256::from(1_000_000))
                .with_gas_limit(100_000),
        )
        .with_action(ProduceDeterministicBlocks::new(1))
        .with_action(CaptureDebankBlock::new("failed-create"))
        .run()
        .await
}
