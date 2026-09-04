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

//! Launched-node coverage for rare `trace_debankBlock` trace and rollback shapes.

use alloy_consensus::constants::KECCAK_EMPTY;
use alloy_primitives::{address, keccak256, Bytes, B256, U256};
use alloy_rpc_types_eth::{TransactionInput, TransactionRequest};
use alloy_sol_types::{sol, SolCall, SolError, SolEvent};
use arc_execution_config::call_from::{MEMO_ADDRESS, MULTICALL3_FROM_ADDRESS};
use arc_execution_e2e::{ArcSetup, ArcTestNode, TxKind};
use arc_precompiles::call_from::CALL_FROM_ADDRESS;
use eyre::{ensure, Result};

use super::helpers::{
    contracts::{
        create2_balance_probe_calldata, create2_with_balance_probe,
        log1_then_revert_contract_deploy_code, reverting_constructor_code,
        reverting_contract_deploy_code, right_pad_address, selfdestruct_contract_deploy_code,
        selfdestruct_contract_runtime_code, REVERT_LOG_DATA, REVERT_LOG_TOPIC,
    },
    debank::{assert_debank_block, assert_eip7708_transfer, transaction_view},
    utils::deploy_and_mine,
};

sol! {
    interface IMemo {
        error MemoFailed(bytes returnData);
        function memo(address target, bytes data, bytes32 memoId, bytes memoData) external;
        event BeforeMemo(uint256 indexed memoIndex);
    }

    interface IMulticall3From {
        struct Call3 {
            address target;
            bool allowFailure;
            bytes callData;
        }

        struct Result {
            bool success;
            bytes returnData;
        }

        function aggregate3(Call3[] calls) external returns (Result[] returnData);
    }
}

#[tokio::test]
async fn debank_block_failed_create_keeps_trace_and_event_in_error_arrays() -> Result<()> {
    reth_tracing::init_test_tracing();

    let endowment = U256::from(1_000_000);
    let mut node = ArcTestNode::start(ArcSetup::new()).await?;
    let signer = node.wallet_signer(0)?;
    let tx_hash = node
        .send_tx(
            signer.clone(),
            TransactionRequest {
                from: Some(signer.address()),
                to: Some(TxKind::Create),
                value: Some(endowment),
                gas: Some(100_000),
                input: TransactionInput::new(reverting_constructor_code()),
                ..Default::default()
            },
        )
        .await?;
    node.produce_block().await?;
    let receipt = node.get_receipt(tx_hash).await?;
    ensure!(!receipt.status(), "failed CREATE unexpectedly succeeded");
    ensure!(
        receipt.logs().is_empty(),
        "failed CREATE emitted receipt logs"
    );
    let block_number = receipt
        .block_number
        .ok_or_else(|| eyre::eyre!("mined transaction receipt has no block number"))?;

    assert_debank_block(
        &node,
        block_number,
        tx_hash,
        move |output, state_diff, transaction| {
            ensure!(!transaction.status, "failed CREATE is marked successful");
            let created = transaction.from.create(transaction.nonce);
            ensure!(
                transaction.to == created,
                "failed CREATE transaction target mismatch"
            );

            let view = transaction_view(output, transaction);
            ensure!(
                view.traces.is_empty(),
                "failed CREATE leaked a successful trace"
            );
            ensure!(
                view.events.is_empty(),
                "failed CREATE leaked a successful event"
            );
            ensure!(
                view.error_traces.len() == 1,
                "expected one failed CREATE trace"
            );
            ensure!(
                view.error_events.len() == 1,
                "expected one reverted Transfer event"
            );

            let root = view.error_traces[0];
            ensure!(root.parent_trace_id.is_empty(), "CREATE root has a parent");
            ensure!(
                root.trace_address.is_empty(),
                "CREATE root has a trace address"
            );
            ensure!(
                root.pos_in_parent_trace == 0,
                "unexpected CREATE root position"
            );
            ensure!(
                root.call_create_type == "create",
                "root is not a CREATE trace"
            );
            ensure!(root.call_type.is_empty(), "CREATE trace has a call type");
            ensure!(
                root.from_addr == transaction.from,
                "unexpected CREATE caller"
            );
            ensure!(
                root.to_addr == created,
                "unexpected would-be CREATE address"
            );
            ensure!(root.value == endowment, "unexpected CREATE endowment");
            ensure!(root.subtraces == 0, "failed CREATE has child traces");
            ensure!(root.error == "Reverted", "unexpected CREATE error");
            assert_eip7708_transfer(
                view.error_events[0],
                transaction.from,
                created,
                endowment,
                &root.id,
                0,
                0,
            )?;

            let created_hash = keccak256(created.as_slice());
            ensure!(
                state_diff
                    .new_accounts
                    .iter()
                    .all(|account| account.address != created_hash),
                "failed CREATE leaked its account into StateDiff"
            );
            ensure!(
                state_diff
                    .new_codes
                    .iter()
                    .all(|code| code.code_hash == KECCAK_EMPTY && code.code.is_empty()),
                "failed CREATE leaked non-empty runtime code"
            );
            Ok(())
        },
    )
    .await
}

#[tokio::test]
async fn debank_block_internal_create2_has_child_trace_event_and_state() -> Result<()> {
    reth_tracing::init_test_tracing();

    let mut node = ArcTestNode::start(ArcSetup::new()).await?;
    let signer = node.wallet_signer(0)?;
    let probe = signer.address().create(0);
    let child = probe.create2([0u8; 32], keccak256([0u8]));
    let deploy_hash = node
        .send_tx(
            signer.clone(),
            TransactionRequest {
                from: Some(signer.address()),
                to: Some(TxKind::Create),
                value: Some(U256::from(1)),
                gas: Some(200_000),
                input: TransactionInput::new(create2_with_balance_probe()),
                ..Default::default()
            },
        )
        .await?;
    let run_hash = node
        .send_tx(
            signer.clone(),
            TransactionRequest {
                from: Some(signer.address()),
                to: Some(TxKind::Call(probe)),
                value: Some(U256::ZERO),
                gas: Some(200_000),
                input: TransactionInput::new(create2_balance_probe_calldata(probe)),
                ..Default::default()
            },
        )
        .await?;
    node.produce_block().await?;
    let deploy_receipt = node.get_receipt(deploy_hash).await?;
    let run_receipt = node.get_receipt(run_hash).await?;
    ensure!(deploy_receipt.status(), "probe deployment failed");
    ensure!(run_receipt.status(), "CREATE2 probe transaction failed");
    ensure!(
        deploy_receipt.logs().len() == 1,
        "probe deployment log count mismatch"
    );
    ensure!(
        run_receipt.logs().len() == 1,
        "CREATE2 probe log count mismatch"
    );
    ensure!(
        deploy_receipt.block_number == run_receipt.block_number,
        "probe deployment and execution were not mined in the same block"
    );
    let block_number = run_receipt
        .block_number
        .ok_or_else(|| eyre::eyre!("mined transaction receipt has no block number"))?;

    assert_debank_block(
        &node,
        block_number,
        run_hash,
        move |output, state_diff, transaction| {
            ensure!(transaction.status, "CREATE2 probe transaction failed");
            ensure!(transaction.to == probe, "CREATE2 probe target mismatch");

            let view = transaction_view(output, transaction);
            ensure!(
                view.traces.len() == 2,
                "expected CALL root and CREATE2 child"
            );
            ensure!(
                view.error_traces.is_empty(),
                "CREATE2 produced an error trace"
            );
            ensure!(
                view.events.len() == 1,
                "expected one CREATE2 Transfer event"
            );
            ensure!(
                view.error_events.is_empty(),
                "CREATE2 produced an error event"
            );

            let root = view
                .traces
                .iter()
                .copied()
                .find(|trace| trace.parent_trace_id.is_empty())
                .ok_or_else(|| eyre::eyre!("CREATE2 root trace is missing"))?;
            let create = view
                .traces
                .iter()
                .copied()
                .find(|trace| trace.call_create_type == "create")
                .ok_or_else(|| eyre::eyre!("internal CREATE2 trace is missing"))?;
            ensure!(root.call_create_type == "call", "root is not a CALL trace");
            ensure!(root.call_type == "call", "unexpected root call type");
            ensure!(root.to_addr == probe, "root trace target mismatch");
            ensure!(root.subtraces == 1, "root subtrace count mismatch");
            ensure!(
                root.self_storage_change,
                "probe SSTORE was not attributed to root"
            );
            ensure!(
                root.storage_change,
                "probe storage change did not propagate"
            );
            ensure!(create.parent_trace_id == root.id, "CREATE2 parent mismatch");
            ensure!(
                create.trace_address == [0],
                "CREATE2 trace address mismatch"
            );
            ensure!(create.pos_in_parent_trace == 0, "CREATE2 position mismatch");
            ensure!(create.call_type.is_empty(), "CREATE2 trace has a call type");
            ensure!(create.from_addr == probe, "CREATE2 caller mismatch");
            ensure!(create.to_addr == child, "CREATE2 child address mismatch");
            ensure!(create.value == U256::from(1), "CREATE2 value mismatch");
            ensure!(create.error.is_empty(), "CREATE2 trace is marked failed");
            assert_eip7708_transfer(
                view.events[0],
                probe,
                child,
                U256::from(1),
                &create.id,
                0,
                1,
            )?;

            ensure!(
                output.block_file.storage_contracts.contains(&probe),
                "probe is missing from storage_contracts"
            );
            let child_account = state_diff
                .new_accounts
                .iter()
                .find(|account| account.address == keccak256(child.as_slice()))
                .ok_or_else(|| eyre::eyre!("CREATE2 child is missing from StateDiff"))?;
            ensure!(
                child_account.balance == U256::from(1),
                "CREATE2 child balance mismatch"
            );
            ensure!(child_account.nonce == 1, "CREATE2 child nonce mismatch");
            ensure!(
                child_account.code_hash == KECCAK_EMPTY,
                "CREATE2 child code mismatch"
            );
            let probe_storage = state_diff
                .storage_diffs
                .iter()
                .find(|account| account.address == keccak256(probe.as_slice()))
                .ok_or_else(|| eyre::eyre!("probe storage diff is missing"))?;
            let slot_zero = keccak256([0u8; 32]);
            ensure!(
                probe_storage.diffs.iter().any(|diff| {
                    diff.index == slot_zero && diff.value == U256::from_be_slice(child.as_slice())
                }),
                "probe did not persist the CREATE2 child address"
            );
            Ok(())
        },
    )
    .await
}

#[tokio::test]
async fn debank_block_selfdestruct_has_suicide_trace_without_account_deletion() -> Result<()> {
    reth_tracing::init_test_tracing();

    let mut node = ArcTestNode::start(ArcSetup::new()).await?;
    let signer = node.wallet_signer(0)?;
    let source = signer.address().create(0);
    let beneficiary = address!("0x000000000000000000000000000000000000BEEF");
    let endowment = U256::from(1_000_000);
    let source_runtime = selfdestruct_contract_runtime_code();
    let source_code_hash = keccak256(&source_runtime);
    let deploy_hash = node
        .send_tx(
            signer.clone(),
            TransactionRequest {
                from: Some(signer.address()),
                to: Some(TxKind::Create),
                value: Some(endowment),
                gas: Some(200_000),
                input: TransactionInput::new(selfdestruct_contract_deploy_code()),
                ..Default::default()
            },
        )
        .await?;
    let selfdestruct_hash = node
        .send_tx(
            signer.clone(),
            TransactionRequest {
                from: Some(signer.address()),
                to: Some(TxKind::Call(source)),
                value: Some(U256::ZERO),
                gas: Some(200_000),
                input: TransactionInput::new(right_pad_address(beneficiary)),
                ..Default::default()
            },
        )
        .await?;
    node.produce_block().await?;
    let deploy_receipt = node.get_receipt(deploy_hash).await?;
    let selfdestruct_receipt = node.get_receipt(selfdestruct_hash).await?;
    ensure!(
        deploy_receipt.status(),
        "SELFDESTRUCT contract deployment failed"
    );
    ensure!(
        selfdestruct_receipt.status(),
        "SELFDESTRUCT transaction failed"
    );
    ensure!(
        deploy_receipt.logs().len() == 1,
        "deployment log count mismatch"
    );
    ensure!(
        selfdestruct_receipt.logs().len() == 1,
        "SELFDESTRUCT log count mismatch"
    );
    ensure!(
        deploy_receipt.block_number == selfdestruct_receipt.block_number,
        "deployment and SELFDESTRUCT were not mined in the same block"
    );
    let block_number = selfdestruct_receipt
        .block_number
        .ok_or_else(|| eyre::eyre!("mined transaction receipt has no block number"))?;

    assert_debank_block(
        &node,
        block_number,
        selfdestruct_hash,
        move |output, state_diff, transaction| {
            ensure!(transaction.status, "SELFDESTRUCT transaction failed");
            ensure!(transaction.to == source, "SELFDESTRUCT target mismatch");

            let view = transaction_view(output, transaction);
            ensure!(
                view.traces.len() == 2,
                "expected CALL root and suicide trace"
            );
            ensure!(
                view.error_traces.is_empty(),
                "SELFDESTRUCT produced an error trace"
            );
            ensure!(
                view.events.len() == 1,
                "expected one SELFDESTRUCT Transfer event"
            );
            ensure!(
                view.error_events.is_empty(),
                "SELFDESTRUCT produced an error event"
            );

            let root = view
                .traces
                .iter()
                .copied()
                .find(|trace| trace.parent_trace_id.is_empty())
                .ok_or_else(|| eyre::eyre!("SELFDESTRUCT root trace is missing"))?;
            let suicide = view
                .traces
                .iter()
                .copied()
                .find(|trace| trace.call_create_type == "suicide")
                .ok_or_else(|| eyre::eyre!("suicide trace is missing"))?;
            ensure!(root.call_create_type == "call", "root is not a CALL trace");
            ensure!(root.call_type == "call", "unexpected root call type");
            ensure!(root.to_addr == source, "root trace target mismatch");
            ensure!(root.subtraces == 1, "root subtrace count mismatch");
            ensure!(
                suicide.parent_trace_id == root.id,
                "suicide parent mismatch"
            );
            ensure!(
                suicide.trace_address == [0],
                "suicide trace address mismatch"
            );
            ensure!(
                suicide.pos_in_parent_trace == 1,
                "suicide position mismatch"
            );
            ensure!(
                suicide.call_type.is_empty(),
                "suicide trace has a call type"
            );
            ensure!(suicide.from_addr == source, "suicide source mismatch");
            ensure!(
                suicide.to_addr == beneficiary,
                "suicide beneficiary mismatch"
            );
            ensure!(suicide.value == endowment, "suicide value mismatch");
            ensure!(suicide.error.is_empty(), "suicide trace is marked failed");
            assert_eip7708_transfer(
                view.events[0],
                source,
                beneficiary,
                endowment,
                &root.id,
                0,
                1,
            )?;

            let source_hash = keccak256(source.as_slice());
            ensure!(
                state_diff
                    .deleted_accounts
                    .iter()
                    .all(|address| *address != source_hash),
                "pre-existing SELFDESTRUCT account was deleted"
            );
            let source_account = state_diff
                .new_accounts
                .iter()
                .find(|account| account.address == source_hash)
                .ok_or_else(|| eyre::eyre!("SELFDESTRUCT source is missing from StateDiff"))?;
            ensure!(
                source_account.balance.is_zero(),
                "SELFDESTRUCT source kept its balance"
            );
            ensure!(
                source_account.nonce == 1,
                "SELFDESTRUCT source nonce mismatch"
            );
            ensure!(
                source_account.code_hash == source_code_hash,
                "SELFDESTRUCT source code hash mismatch"
            );
            let matching_codes: Vec<_> = state_diff
                .new_codes
                .iter()
                .filter(|code| code.code_hash == source_code_hash)
                .collect();
            ensure!(
                matching_codes.len() == 1,
                "expected exactly one SELFDESTRUCT source NewCode"
            );
            let source_code = matching_codes
                .first()
                .ok_or_else(|| eyre::eyre!("SELFDESTRUCT source NewCode is missing"))?;
            ensure!(
                source_code.code == source_runtime,
                "SELFDESTRUCT source NewCode bytes mismatch"
            );
            let beneficiary_account = state_diff
                .new_accounts
                .iter()
                .find(|account| account.address == keccak256(beneficiary.as_slice()))
                .ok_or_else(|| eyre::eyre!("SELFDESTRUCT beneficiary is missing from StateDiff"))?;
            ensure!(
                beneficiary_account.balance == endowment,
                "SELFDESTRUCT beneficiary balance mismatch"
            );
            Ok(())
        },
    )
    .await
}

#[tokio::test]
async fn debank_block_reverted_value_call_preserves_event_only_in_error_events() -> Result<()> {
    reth_tracing::init_test_tracing();

    let mut node = ArcTestNode::start(ArcSetup::new()).await?;
    let signer = node.wallet_signer(0)?;
    let target = signer.address().create(0);
    let value = U256::from(1_000_000);
    let deploy_hash = node
        .send_tx(
            signer.clone(),
            TransactionRequest {
                from: Some(signer.address()),
                to: Some(TxKind::Create),
                value: Some(U256::ZERO),
                gas: Some(100_000),
                input: TransactionInput::new(reverting_contract_deploy_code()),
                ..Default::default()
            },
        )
        .await?;
    let call_hash = node
        .send_tx(
            signer.clone(),
            TransactionRequest {
                from: Some(signer.address()),
                to: Some(TxKind::Call(target)),
                value: Some(value),
                gas: Some(100_000),
                ..Default::default()
            },
        )
        .await?;
    node.produce_block().await?;
    let deploy_receipt = node.get_receipt(deploy_hash).await?;
    let call_receipt = node.get_receipt(call_hash).await?;
    ensure!(deploy_receipt.status(), "reverter deployment failed");
    ensure!(
        !call_receipt.status(),
        "reverted CALL unexpectedly succeeded"
    );
    ensure!(
        deploy_receipt.logs().is_empty(),
        "reverter deployment emitted logs"
    );
    ensure!(
        call_receipt.logs().is_empty(),
        "reverted CALL emitted receipt logs"
    );
    ensure!(
        deploy_receipt.block_number == call_receipt.block_number,
        "deployment and reverted CALL were not mined in the same block"
    );
    let block_number = call_receipt
        .block_number
        .ok_or_else(|| eyre::eyre!("mined transaction receipt has no block number"))?;

    assert_debank_block(
        &node,
        block_number,
        call_hash,
        move |output, state_diff, transaction| {
            ensure!(!transaction.status, "reverted CALL is marked successful");
            ensure!(transaction.to == target, "reverted CALL target mismatch");

            let view = transaction_view(output, transaction);
            ensure!(
                view.traces.is_empty(),
                "reverted CALL leaked a successful trace"
            );
            ensure!(
                view.events.is_empty(),
                "reverted CALL leaked a successful event"
            );
            ensure!(
                view.error_traces.len() == 1,
                "expected one reverted CALL trace"
            );
            ensure!(
                view.error_events.len() == 1,
                "expected one reverted Transfer event"
            );

            let root = view.error_traces[0];
            ensure!(root.parent_trace_id.is_empty(), "CALL root has a parent");
            ensure!(
                root.trace_address.is_empty(),
                "CALL root has a trace address"
            );
            ensure!(
                root.pos_in_parent_trace == 0,
                "unexpected CALL root position"
            );
            ensure!(root.call_create_type == "call", "root is not a CALL trace");
            ensure!(root.call_type == "call", "unexpected root call type");
            ensure!(root.from_addr == transaction.from, "unexpected CALL sender");
            ensure!(root.to_addr == target, "unexpected CALL target");
            ensure!(root.value == value, "unexpected CALL value");
            ensure!(root.subtraces == 0, "reverted CALL has child traces");
            ensure!(root.error == "Reverted", "unexpected CALL error");
            assert_eip7708_transfer(
                view.error_events[0],
                transaction.from,
                target,
                value,
                &root.id,
                0,
                0,
            )?;

            let target_account = state_diff
                .new_accounts
                .iter()
                .find(|account| account.address == keccak256(target.as_slice()))
                .ok_or_else(|| eyre::eyre!("reverter deployment is missing from StateDiff"))?;
            ensure!(
                target_account.balance.is_zero(),
                "reverted CALL leaked value"
            );
            ensure!(
                target_account.code_hash != KECCAK_EMPTY,
                "reverter code is missing"
            );
            Ok(())
        },
    )
    .await
}

#[tokio::test]
async fn debank_block_reverted_contract_log_is_only_in_error_events() -> Result<()> {
    reth_tracing::init_test_tracing();

    let mut node = ArcTestNode::start(ArcSetup::new()).await?;
    let signer = node.wallet_signer(0)?;
    let target = signer.address().create(0);
    let deploy_hash = node
        .send_tx(
            signer.clone(),
            TransactionRequest {
                from: Some(signer.address()),
                to: Some(TxKind::Create),
                value: Some(U256::ZERO),
                gas: Some(150_000),
                input: TransactionInput::new(log1_then_revert_contract_deploy_code()),
                ..Default::default()
            },
        )
        .await?;
    let call_hash = node
        .send_tx(
            signer.clone(),
            TransactionRequest {
                from: Some(signer.address()),
                to: Some(TxKind::Call(target)),
                value: Some(U256::ZERO),
                gas: Some(100_000),
                ..Default::default()
            },
        )
        .await?;
    node.produce_block().await?;
    let deploy_receipt = node.get_receipt(deploy_hash).await?;
    let call_receipt = node.get_receipt(call_hash).await?;
    ensure!(deploy_receipt.status(), "LOG reverter deployment failed");
    ensure!(
        !call_receipt.status(),
        "LOG reverter unexpectedly succeeded"
    );
    ensure!(
        deploy_receipt.logs().is_empty(),
        "LOG reverter deployment emitted logs"
    );
    ensure!(
        call_receipt.logs().is_empty(),
        "reverted LOG leaked into receipt"
    );
    ensure!(
        deploy_receipt.block_number == call_receipt.block_number,
        "deployment and reverted LOG were not mined in the same block"
    );
    let block_number = call_receipt
        .block_number
        .ok_or_else(|| eyre::eyre!("mined transaction receipt has no block number"))?;

    assert_debank_block(
        &node,
        block_number,
        call_hash,
        move |output, _state_diff, transaction| {
            let view = transaction_view(output, transaction);
            ensure!(view.traces.is_empty(), "reverted CALL leaked a trace");
            ensure!(view.events.is_empty(), "reverted LOG leaked an event");
            ensure!(view.error_traces.len() == 1, "expected one error trace");
            ensure!(
                view.error_events.len() == 1,
                "expected one transaction error event"
            );
            ensure!(
                output.block_file.events.is_empty(),
                "block contains an unexpected successful event"
            );
            ensure!(
                output.block_file.error_events.len() == 1,
                "block must contain exactly one error event"
            );

            let root = view.error_traces[0];
            let event = view
                .error_events
                .first()
                .ok_or_else(|| eyre::eyre!("LOG1 error event is missing"))?;
            ensure!(event.contract_id == target, "LOG1 emitter mismatch");
            ensure!(
                event.selector == REVERT_LOG_TOPIC.to_string(),
                "LOG1 topic mismatch"
            );
            ensure!(event.topics.is_empty(), "LOG1 has extra topics");
            ensure!(
                event.data.as_ref() == REVERT_LOG_DATA.as_slice(),
                "LOG1 data mismatch"
            );
            ensure!(
                event.parent_trace_id == root.id,
                "LOG1 parent trace mismatch"
            );
            ensure!(event.pos_in_parent_trace == 0, "LOG1 position mismatch");
            ensure!(event.idx == 0, "error event idx must remain zero");
            Ok(())
        },
    )
    .await
}

#[tokio::test]
async fn debank_block_nested_call_from_reverts_are_transparent_error_traces() -> Result<()> {
    reth_tracing::init_test_tracing();

    let mut node = ArcTestNode::start(ArcSetup::new()).await?;
    let signer = node.wallet_signer(0)?;
    let signer_address = signer.address();
    let (target, _) = deploy_and_mine(
        &mut node,
        signer.clone(),
        log1_then_revert_contract_deploy_code(),
        U256::ZERO,
        150_000,
    )
    .await?;

    let target_input = Bytes::from_static(b"reverted-child");
    let memo_id = B256::repeat_byte(0xab);
    let memo_input = Bytes::from(
        IMemo::memoCall {
            target,
            data: target_input.clone(),
            memoId: memo_id,
            memoData: Bytes::from_static(b"must-roll-back"),
        }
        .abi_encode(),
    );
    let input = Bytes::from(
        IMulticall3From::aggregate3Call {
            calls: vec![IMulticall3From::Call3 {
                target: MEMO_ADDRESS,
                allowFailure: true,
                callData: memo_input.clone(),
            }],
        }
        .abi_encode(),
    );
    let expected_memo_error = Bytes::from(
        IMemo::MemoFailed {
            returnData: Bytes::new(),
        }
        .abi_encode(),
    );

    let tx_hash = node
        .send_tx(
            signer,
            TransactionRequest {
                from: Some(signer_address),
                to: Some(TxKind::Call(MULTICALL3_FROM_ADDRESS)),
                value: Some(U256::ZERO),
                gas: Some(500_000),
                input: TransactionInput::new(input.clone()),
                ..Default::default()
            },
        )
        .await?;
    node.produce_block().await?;
    let receipt = node.get_receipt(tx_hash).await?;
    ensure!(receipt.status(), "failure-tolerant Multicall3From reverted");
    ensure!(
        receipt.logs().is_empty(),
        "reverted child logs leaked into receipt"
    );
    let block_number = receipt
        .block_number
        .ok_or_else(|| eyre::eyre!("mined transaction receipt has no block number"))?;

    assert_debank_block(
        &node,
        block_number,
        tx_hash,
        move |output, state_diff, transaction| {
            ensure!(transaction.status, "Multicall3From transaction failed");
            ensure!(
                transaction.to == MULTICALL3_FROM_ADDRESS,
                "root target mismatch"
            );
            ensure!(transaction.input == input, "root calldata mismatch");

            let view = transaction_view(output, transaction);
            ensure!(view.traces.len() == 1, "expected one successful root trace");
            ensure!(
                view.error_traces.len() == 2,
                "expected Memo and target errors"
            );
            ensure!(
                view.events.is_empty(),
                "reverted child logs leaked as events"
            );
            ensure!(view.error_events.len() == 2, "expected two reverted events");

            let root = view.traces[0];
            ensure!(root.parent_trace_id.is_empty(), "root has a parent");
            ensure!(root.from_addr == signer_address, "root caller mismatch");
            ensure!(
                root.to_addr == MULTICALL3_FROM_ADDRESS,
                "root target mismatch"
            );
            ensure!(root.input == input, "root calldata mismatch");
            ensure!(root.error.is_empty(), "root is marked failed");
            ensure!(root.subtraces == 1, "root subtrace count mismatch");
            ensure!(root.trace_address.is_empty(), "root has a trace address");
            ensure!(
                !root.self_storage_change,
                "root unexpectedly changed storage"
            );
            ensure!(!root.storage_change, "reverted storage propagated to root");
            let decoded = IMulticall3From::aggregate3Call::abi_decode_returns(&root.output)?;
            ensure!(decoded.len() == 1, "aggregate3 result count mismatch");
            ensure!(!decoded[0].success, "failed Memo is marked successful");
            ensure!(
                decoded[0].returnData == expected_memo_error,
                "Memo error output mismatch"
            );

            let memo = view
                .error_traces
                .iter()
                .copied()
                .find(|trace| trace.to_addr == MEMO_ADDRESS)
                .ok_or_else(|| eyre::eyre!("Memo error trace is missing"))?;
            ensure!(memo.parent_trace_id == root.id, "Memo parent mismatch");
            ensure!(memo.trace_address == [0], "Memo trace address mismatch");
            ensure!(memo.pos_in_parent_trace == 0, "Memo position mismatch");
            ensure!(memo.from_addr == signer_address, "Memo caller mismatch");
            ensure!(memo.input == memo_input, "Memo calldata mismatch");
            ensure!(memo.error == "Reverted", "Memo error mismatch");
            ensure!(memo.output == expected_memo_error, "Memo output mismatch");
            ensure!(memo.subtraces == 1, "Memo subtrace count mismatch");
            ensure!(memo.self_storage_change, "Memo SSTORE was not captured");
            ensure!(memo.storage_change, "Memo storage flag mismatch");

            let child = view
                .error_traces
                .iter()
                .copied()
                .find(|trace| trace.to_addr == target)
                .ok_or_else(|| eyre::eyre!("target error trace is missing"))?;
            ensure!(child.parent_trace_id == memo.id, "target parent mismatch");
            ensure!(
                child.trace_address == [0, 0],
                "target trace address mismatch"
            );
            ensure!(child.pos_in_parent_trace == 1, "target position mismatch");
            ensure!(child.from_addr == signer_address, "target caller mismatch");
            ensure!(child.input == target_input, "target calldata mismatch");
            ensure!(child.error == "Reverted", "target error mismatch");
            ensure!(child.output.is_empty(), "target returned unexpected output");
            ensure!(child.subtraces == 0, "target has child traces");
            ensure!(!child.self_storage_change, "target changed storage");
            ensure!(!child.storage_change, "target storage flag mismatch");

            ensure!(
                view.traces
                    .iter()
                    .chain(&view.error_traces)
                    .all(|trace| trace.from_addr != CALL_FROM_ADDRESS
                        && trace.to_addr != CALL_FROM_ADDRESS),
                "CallFrom precompile leaked into traces"
            );

            let before = view
                .error_events
                .iter()
                .copied()
                .find(|event| event.contract_id == MEMO_ADDRESS)
                .ok_or_else(|| eyre::eyre!("BeforeMemo error event is missing"))?;
            ensure!(
                before.selector == IMemo::BeforeMemo::SIGNATURE_HASH.to_string(),
                "BeforeMemo selector mismatch"
            );
            ensure!(
                before.topics == [B256::ZERO.to_string()],
                "BeforeMemo topics mismatch"
            );
            ensure!(before.data.is_empty(), "BeforeMemo data mismatch");
            ensure!(
                before.parent_trace_id == memo.id,
                "BeforeMemo parent mismatch"
            );
            ensure!(
                before.pos_in_parent_trace == 0,
                "BeforeMemo position mismatch"
            );
            ensure!(before.idx == 0, "BeforeMemo index mismatch");

            let target_log = view
                .error_events
                .iter()
                .copied()
                .find(|event| event.contract_id == target)
                .ok_or_else(|| eyre::eyre!("target error event is missing"))?;
            ensure!(
                target_log.selector == REVERT_LOG_TOPIC.to_string(),
                "target log selector mismatch"
            );
            ensure!(target_log.topics.is_empty(), "target log has extra topics");
            ensure!(
                target_log.data.as_ref() == REVERT_LOG_DATA.as_slice(),
                "target log data mismatch"
            );
            ensure!(
                target_log.parent_trace_id == child.id,
                "target log parent mismatch"
            );
            ensure!(
                target_log.pos_in_parent_trace == 0,
                "target log position mismatch"
            );
            ensure!(target_log.idx == 0, "target error event index mismatch");

            let memo_hash = keccak256(MEMO_ADDRESS.as_slice());
            ensure!(
                !output.block_file.storage_contracts.contains(&MEMO_ADDRESS),
                "reverted Memo storage leaked into BlockFile"
            );
            ensure!(
                state_diff
                    .storage_diffs
                    .iter()
                    .all(|diff| diff.address != memo_hash),
                "reverted Memo storage leaked into StateDiff"
            );
            Ok(())
        },
    )
    .await
}
