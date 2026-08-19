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

mod helpers;

use alloy_consensus::constants::KECCAK_EMPTY;
use alloy_primitives::{address, keccak256, U256};
use arc_execution_e2e::{
    actions::{AssertTxIncluded, AssertTxLogs, ProduceBlocks, SendTransaction, TxStatus},
    ArcSetup, ArcTestBuilder,
};
use eyre::{ensure, Result};

use helpers::{
    constants::WALLET_FIRST_ADDRESS,
    contracts::{
        create2_balance_probe_calldata, create2_with_balance_probe,
        log1_then_revert_contract_deploy_code, reverting_constructor_code,
        reverting_contract_deploy_code, right_pad_address, selfdestruct_contract_deploy_code,
        selfdestruct_contract_runtime_code, REVERT_LOG_DATA, REVERT_LOG_TOPIC,
    },
    debank::{
        assert_eip7708_transfer, current_mainnet_test_chain_spec, transaction_view,
        AssertDebankBlock,
    },
};

#[tokio::test]
async fn debank_block_failed_create_keeps_trace_and_event_in_error_arrays() -> Result<()> {
    reth_tracing::init_test_tracing();

    let endowment = U256::from(1_000_000);
    ArcTestBuilder::new()
        .with_setup(ArcSetup::new().with_chain_spec(current_mainnet_test_chain_spec()))
        .with_action(
            SendTransaction::new("failed_create")
                .with_create()
                .with_data(reverting_constructor_code())
                .with_value(endowment)
                .with_gas_limit(100_000),
        )
        .with_action(ProduceBlocks::new(1))
        .with_action(AssertTxIncluded::new("failed_create").expect(TxStatus::Reverted))
        .with_action(AssertTxLogs::new("failed_create").expect_no_logs())
        .with_action(AssertDebankBlock::new(
            "failed_create",
            move |_env, output, state_diff, transaction| {
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
        ))
        .run()
        .await
}

#[tokio::test]
async fn debank_block_internal_create2_has_child_trace_event_and_state() -> Result<()> {
    reth_tracing::init_test_tracing();

    let probe = WALLET_FIRST_ADDRESS.create(0);
    let child = probe.create2([0u8; 32], keccak256([0u8]));
    ArcTestBuilder::new()
        .with_setup(ArcSetup::new().with_chain_spec(current_mainnet_test_chain_spec()))
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
        .with_action(ProduceBlocks::new(1))
        .with_action(AssertTxIncluded::new("deploy_probe").expect(TxStatus::Success))
        .with_action(AssertTxIncluded::new("run_probe").expect(TxStatus::Success))
        .with_action(AssertTxLogs::new("deploy_probe").expect_log_count(1))
        .with_action(AssertTxLogs::new("run_probe").expect_log_count(1))
        .with_action(AssertDebankBlock::new(
            "run_probe",
            move |_env, output, state_diff, transaction| {
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
                        diff.index == slot_zero
                            && diff.value == U256::from_be_slice(child.as_slice())
                    }),
                    "probe did not persist the CREATE2 child address"
                );
                Ok(())
            },
        ))
        .run()
        .await
}

#[tokio::test]
async fn debank_block_selfdestruct_has_suicide_trace_without_account_deletion() -> Result<()> {
    reth_tracing::init_test_tracing();

    let source = WALLET_FIRST_ADDRESS.create(0);
    let beneficiary = address!("0x000000000000000000000000000000000000BEEF");
    let endowment = U256::from(1_000_000);
    let source_runtime = selfdestruct_contract_runtime_code();
    let source_code_hash = keccak256(&source_runtime);
    ArcTestBuilder::new()
        .with_setup(ArcSetup::new().with_chain_spec(current_mainnet_test_chain_spec()))
        .with_action(
            SendTransaction::new("deploy_selfdestruct")
                .with_create()
                .with_data(selfdestruct_contract_deploy_code())
                .with_value(endowment)
                .with_gas_limit(200_000),
        )
        .with_action(
            SendTransaction::new("selfdestruct")
                .with_to(source)
                .with_data(right_pad_address(beneficiary))
                .with_value(U256::ZERO)
                .with_gas_limit(200_000),
        )
        .with_action(ProduceBlocks::new(1))
        .with_action(AssertTxIncluded::new("deploy_selfdestruct").expect(TxStatus::Success))
        .with_action(AssertTxIncluded::new("selfdestruct").expect(TxStatus::Success))
        .with_action(AssertTxLogs::new("deploy_selfdestruct").expect_log_count(1))
        .with_action(AssertTxLogs::new("selfdestruct").expect_log_count(1))
        .with_action(AssertDebankBlock::new(
            "selfdestruct",
            move |_env, output, state_diff, transaction| {
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
                    .ok_or_else(|| {
                        eyre::eyre!("SELFDESTRUCT beneficiary is missing from StateDiff")
                    })?;
                ensure!(
                    beneficiary_account.balance == endowment,
                    "SELFDESTRUCT beneficiary balance mismatch"
                );
                Ok(())
            },
        ))
        .run()
        .await
}

#[tokio::test]
async fn debank_block_reverted_value_call_preserves_event_only_in_error_events() -> Result<()> {
    reth_tracing::init_test_tracing();

    let target = WALLET_FIRST_ADDRESS.create(0);
    let value = U256::from(1_000_000);
    ArcTestBuilder::new()
        .with_setup(ArcSetup::new().with_chain_spec(current_mainnet_test_chain_spec()))
        .with_action(
            SendTransaction::new("deploy_reverter")
                .with_create()
                .with_data(reverting_contract_deploy_code())
                .with_value(U256::ZERO)
                .with_gas_limit(100_000),
        )
        .with_action(
            SendTransaction::new("reverted_call")
                .with_to(target)
                .with_value(value)
                .with_gas_limit(100_000),
        )
        .with_action(ProduceBlocks::new(1))
        .with_action(AssertTxIncluded::new("deploy_reverter").expect(TxStatus::Success))
        .with_action(AssertTxIncluded::new("reverted_call").expect(TxStatus::Reverted))
        .with_action(AssertTxLogs::new("deploy_reverter").expect_no_logs())
        .with_action(AssertTxLogs::new("reverted_call").expect_no_logs())
        .with_action(AssertDebankBlock::new(
            "reverted_call",
            move |_env, output, state_diff, transaction| {
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
        ))
        .run()
        .await
}

#[tokio::test]
async fn debank_block_reverted_contract_log_is_only_in_error_events() -> Result<()> {
    reth_tracing::init_test_tracing();

    let target = WALLET_FIRST_ADDRESS.create(0);
    ArcTestBuilder::new()
        .with_setup(ArcSetup::new().with_chain_spec(current_mainnet_test_chain_spec()))
        .with_action(
            SendTransaction::new("deploy_log_reverter")
                .with_create()
                .with_data(log1_then_revert_contract_deploy_code())
                .with_value(U256::ZERO)
                .with_gas_limit(150_000),
        )
        .with_action(
            SendTransaction::new("log_then_revert")
                .with_to(target)
                .with_value(U256::ZERO)
                .with_gas_limit(100_000),
        )
        .with_action(ProduceBlocks::new(1))
        .with_action(AssertTxIncluded::new("deploy_log_reverter").expect(TxStatus::Success))
        .with_action(AssertTxIncluded::new("log_then_revert").expect(TxStatus::Reverted))
        .with_action(AssertTxLogs::new("deploy_log_reverter").expect_no_logs())
        .with_action(AssertTxLogs::new("log_then_revert").expect_no_logs())
        .with_action(AssertDebankBlock::new(
            "log_then_revert",
            move |_env, output, _state_diff, transaction| {
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
        ))
        .run()
        .await
}
