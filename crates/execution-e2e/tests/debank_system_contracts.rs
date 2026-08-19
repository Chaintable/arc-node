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

//! Launched-node `trace_debankBlock` coverage for Arc system-contract mutations.

mod helpers;

use alloy_eips::eip2935::HISTORY_STORAGE_ADDRESS;
use alloy_primitives::{address, b256, keccak256, Address, Bytes, B256, U256};
use alloy_sol_types::{sol, SolCall, SolEvent, SolValue};
use arc_execution_config::{
    addresses_denylist::DEFAULT_DENYLIST_ADDRESS, protocol_config::PROTOCOL_CONFIG_ADDRESS,
};
use arc_execution_e2e::{
    actions::{AssertTxIncluded, AssertTxLogs, ProduceBlocks, SendTransaction, TxStatus},
    ArcSetup, ArcTestBuilder,
};
use arc_precompiles::{system_accounting::SYSTEM_ACCOUNTING_ADDRESS, NATIVE_COIN_CONTROL_ADDRESS};
use debank_rpc::debank_trace::{
    BlockStorageDiff, DebankEvent, DebankOutPut, DebankTrace, DebankTransaction,
};
use eyre::{ensure, Result};

use helpers::debank::{
    current_mainnet_test_chain_spec, transaction_view, AssertDebankBlock, DebankTxView,
};

const NATIVE_FIAT_TOKEN_ADDRESS: Address = address!("0x3600000000000000000000000000000000000000");
const VALIDATOR_REGISTRY_ADDRESS: Address = address!("0x3600000000000000000000000000000000000002");
const PERMISSIONED_VALIDATOR_MANAGER_ADDRESS: Address =
    address!("0x3600000000000000000000000000000000000003");
const DENYLIST_STORAGE_LOCATION: B256 =
    b256!("1d7e1388d3ae56f3d9c18b1ce8d2b3b1a238a0edf682d2053af5d8a1d2f12f00");
const NATIVE_COIN_CONTROL_BLOCKLIST_MAPPING_SLOT: B256 =
    b256!("0000000000000000000000000000000000000000000000000000000000000002");
const PROTOCOL_CONFIG_BLOCK_GAS_LIMIT_SLOT: B256 =
    b256!("668f09ce856848ead6cb1ddee963f15ef833cea8958030868f867aec84385203");
const VALIDATOR_REGISTRY_STORAGE_LOCATION: B256 =
    b256!("b58da0dce03316992faea3e12c60705b8ac05a309e27e3bc8421e5b271c9d200");
const VALIDATOR_REGISTERED_PUBLIC_KEYS_SLOT: B256 =
    b256!("b58da0dce03316992faea3e12c60705b8ac05a309e27e3bc8421e5b271c9d203");
const VALIDATOR_NEXT_REGISTRATION_ID_SLOT: B256 =
    b256!("b58da0dce03316992faea3e12c60705b8ac05a309e27e3bc8421e5b271c9d204");
const WALLET_OPERATOR_INDEX: usize = 7;
const WALLET_ADMIN_INDEX: usize = 8;

sol! {
    interface IProtocolConfig {
        struct FeeParams {
            uint64 alpha;
            uint64 kRate;
            uint64 inverseElasticityMultiplier;
            uint256 minBaseFee;
            uint256 maxBaseFee;
            uint256 blockGasLimit;
        }

        function updateBlockGasLimit(uint256 newBlockGasLimit) external;
        event FeeParamsUpdated(FeeParams params);
    }

    interface IDenylist {
        function denylist(address[] accounts) external;
        event Denylisted(address indexed account);
    }

    interface INativeFiatToken {
        function blacklist(address account) external;
        event Blacklisted(address indexed account);
    }

    interface INativeCoinControl {
        event Blocklisted(address indexed account);
    }

    interface IPermissionedValidatorManager {
        function registerValidator(bytes publicKey) external returns (uint256 registrationId);
    }

    interface IValidatorRegistry {
        event ValidatorRegistered(uint256 indexed registrationId, uint64 votingPower, bytes publicKey);
    }
}

fn successful_call<'a>(
    output: &'a DebankOutPut,
    transaction: &'a DebankTransaction,
    target: Address,
    input: &Bytes,
) -> Result<(DebankTxView<'a>, &'a DebankTrace)> {
    ensure!(transaction.status, "system-contract transaction failed");
    ensure!(transaction.to == target, "transaction target mismatch");
    ensure!(transaction.input == *input, "transaction calldata mismatch");
    ensure!(transaction.value == U256::ZERO, "transaction sent value");

    let view = transaction_view(output, transaction);
    ensure!(
        view.error_traces.is_empty(),
        "successful transaction contains error traces"
    );
    ensure!(
        view.error_events.is_empty(),
        "successful transaction contains error events"
    );

    let mut roots = view
        .traces
        .iter()
        .copied()
        .filter(|trace| trace.parent_trace_id.is_empty());
    let root = roots
        .next()
        .ok_or_else(|| eyre::eyre!("root trace is missing"))?;
    ensure!(
        roots.next().is_none(),
        "transaction has multiple root traces"
    );
    ensure!(root.from_addr == transaction.from, "root caller mismatch");
    ensure!(root.to_addr == target, "root target mismatch");
    ensure!(root.input == *input, "root calldata mismatch");
    ensure!(root.value == U256::ZERO, "root transferred value");
    ensure!(root.call_create_type == "call", "root is not a call");
    ensure!(root.call_type == "call", "unexpected root call type");
    ensure!(root.trace_address.is_empty(), "root has a trace address");
    ensure!(root.pos_in_parent_trace == 0, "unexpected root position");
    ensure!(root.error.is_empty(), "root trace is marked failed");
    Ok((view, root))
}

fn single_direct_child<'a>(
    view: &DebankTxView<'a>,
    parent: &DebankTrace,
    predicate: impl Fn(&DebankTrace) -> bool,
    label: &str,
) -> Result<&'a DebankTrace> {
    let mut matches = view
        .traces
        .iter()
        .copied()
        .filter(|trace| trace.parent_trace_id == parent.id && predicate(trace));
    let trace = matches
        .next()
        .ok_or_else(|| eyre::eyre!("{label} trace is missing"))?;
    ensure!(
        matches.next().is_none(),
        "transaction has multiple {label} traces"
    );
    Ok(trace)
}

fn assert_child_position(
    child: &DebankTrace,
    parent: &DebankTrace,
    trace_index: usize,
    member_index: usize,
) -> Result<()> {
    let mut expected_address = parent.trace_address.clone();
    expected_address.push(trace_index);
    ensure!(
        child.trace_address == expected_address,
        "child trace address mismatch"
    );
    ensure!(
        child.pos_in_parent_trace == member_index,
        "child position mismatch"
    );
    Ok(())
}

fn assert_event(
    event: &DebankEvent,
    emitter: Address,
    signature: B256,
    topics: Vec<String>,
    parent: &DebankTrace,
    member_index: usize,
    log_index: usize,
) -> Result<()> {
    ensure!(event.contract_id == emitter, "event emitter mismatch");
    ensure!(
        event.selector == signature.to_string(),
        "event selector mismatch"
    );
    ensure!(event.topics == topics, "event topics mismatch");
    ensure!(
        event.parent_trace_id == parent.id,
        "event parent trace mismatch"
    );
    ensure!(
        event.pos_in_parent_trace == member_index,
        "event position mismatch"
    );
    ensure!(event.idx == log_index, "event log index mismatch");
    Ok(())
}

fn address_topic(address: Address) -> String {
    B256::left_padding_from(address.as_slice()).to_string()
}

fn storage_slot_offset(slot: B256, offset: u64) -> B256 {
    let value = U256::from_be_slice(slot.as_slice()).saturating_add(U256::from(offset));
    value.to_be_bytes::<32>().into()
}

fn address_mapping_slot(key: Address, slot: B256) -> B256 {
    keccak256((key, slot).abi_encode())
}

fn assert_storage_changes(
    output: &DebankOutPut,
    state_diff: &BlockStorageDiff,
    business_contract: Address,
    expected_slots: &[(B256, U256)],
) -> Result<()> {
    let mut expected_contracts = vec![
        HISTORY_STORAGE_ADDRESS,
        SYSTEM_ACCOUNTING_ADDRESS,
        business_contract,
    ];
    expected_contracts.sort_unstable();
    ensure!(
        output.block_file.storage_contracts == expected_contracts,
        "unexpected storage_contracts: {:?}",
        output.block_file.storage_contracts
    );

    let mut expected_storage_addresses: Vec<_> = expected_contracts
        .iter()
        .map(|address| keccak256(address.as_slice()))
        .collect();
    expected_storage_addresses.sort_unstable();
    let actual_storage_addresses: Vec<_> = state_diff
        .storage_diffs
        .iter()
        .map(|storage| storage.address)
        .collect();
    ensure!(
        actual_storage_addresses == expected_storage_addresses,
        "StateDiff contains an unexpected storage account"
    );

    let business_storage = state_diff
        .storage_diffs
        .iter()
        .find(|storage| storage.address == keccak256(business_contract.as_slice()))
        .ok_or_else(|| eyre::eyre!("{business_contract} is missing from StateDiff"))?;
    let actual_slots: Vec<_> = business_storage
        .diffs
        .iter()
        .map(|diff| (diff.index, diff.value))
        .collect();
    let mut expected_hashed_slots: Vec<_> = expected_slots
        .iter()
        // BlockStorageDiff stores keccak256(raw Solidity slot), not the raw slot.
        .map(|(raw_slot, value)| (keccak256(raw_slot.as_slice()), *value))
        .collect();
    expected_hashed_slots.sort_unstable_by_key(|(slot, _)| *slot);
    ensure!(
        actual_slots == expected_hashed_slots,
        "{business_contract} StateDiff slot/value mismatch"
    );
    Ok(())
}

#[tokio::test]
async fn debank_block_protocol_config_update_tracks_proxy_storage_and_event() -> Result<()> {
    reth_tracing::init_test_tracing();

    let new_block_gas_limit = U256::from(31_000_000);
    let input = Bytes::from(
        IProtocolConfig::updateBlockGasLimitCall {
            newBlockGasLimit: new_block_gas_limit,
        }
        .abi_encode(),
    );

    ArcTestBuilder::new()
        .with_setup(ArcSetup::new().with_chain_spec(current_mainnet_test_chain_spec()))
        .with_action(
            SendTransaction::new("update_block_gas_limit")
                .with_to(PROTOCOL_CONFIG_ADDRESS)
                .with_data(input.clone())
                .with_value(U256::ZERO)
                .with_gas_limit(200_000)
                .with_wallet_index(WALLET_ADMIN_INDEX),
        )
        .with_action(ProduceBlocks::new(1))
        .with_action(AssertTxIncluded::new("update_block_gas_limit").expect(TxStatus::Success))
        .with_action(AssertTxLogs::new("update_block_gas_limit").expect_log_count(1))
        .with_action(AssertDebankBlock::new(
            "update_block_gas_limit",
            move |_env, output, state_diff, transaction| {
                let (view, root) =
                    successful_call(output, transaction, PROTOCOL_CONFIG_ADDRESS, &input)?;
                ensure!(
                    view.traces.len() == 2,
                    "unexpected ProtocolConfig trace count"
                );
                ensure!(
                    view.events.len() == 1,
                    "unexpected ProtocolConfig event count"
                );
                ensure!(
                    root.subtraces == 1,
                    "ProtocolConfig root child count mismatch"
                );
                ensure!(
                    root.storage_change,
                    "ProtocolConfig root missed storage change"
                );
                ensure!(
                    !root.self_storage_change,
                    "ProtocolConfig proxy root reports its own SSTORE"
                );

                let implementation = single_direct_child(
                    &view,
                    root,
                    |trace| trace.call_type == "delegatecall",
                    "ProtocolConfig implementation",
                )?;
                assert_child_position(implementation, root, 0, 0)?;
                ensure!(
                    implementation.self_storage_change,
                    "ProtocolConfig implementation missed its SSTORE"
                );
                ensure!(
                    implementation.storage_change,
                    "ProtocolConfig implementation missed storage change"
                );
                ensure!(
                    implementation.subtraces == 0,
                    "ProtocolConfig implementation has unexpected children"
                );

                let event = view.events[0];
                assert_event(
                    event,
                    PROTOCOL_CONFIG_ADDRESS,
                    IProtocolConfig::FeeParamsUpdated::SIGNATURE_HASH,
                    vec![],
                    implementation,
                    0,
                    0,
                )?;
                ensure!(
                    event.data.len() == 6 * 32,
                    "FeeParamsUpdated data length mismatch"
                );
                ensure!(
                    &event.data[event.data.len() - 32..]
                        == new_block_gas_limit.to_be_bytes::<32>().as_slice(),
                    "FeeParamsUpdated block gas limit mismatch"
                );
                assert_storage_changes(
                    output,
                    state_diff,
                    PROTOCOL_CONFIG_ADDRESS,
                    &[(PROTOCOL_CONFIG_BLOCK_GAS_LIMIT_SLOT, new_block_gas_limit)],
                )
            },
        ))
        .run()
        .await
}

#[tokio::test]
async fn debank_block_denylist_tracks_proxy_storage_and_event() -> Result<()> {
    reth_tracing::init_test_tracing();

    let denied = Address::repeat_byte(0xd1);
    let input = Bytes::from(
        IDenylist::denylistCall {
            accounts: vec![denied],
        }
        .abi_encode(),
    );

    ArcTestBuilder::new()
        .with_setup(ArcSetup::new().with_chain_spec(current_mainnet_test_chain_spec()))
        .with_action(
            SendTransaction::new("denylist_account")
                .with_to(DEFAULT_DENYLIST_ADDRESS)
                .with_data(input.clone())
                .with_value(U256::ZERO)
                .with_gas_limit(200_000)
                .with_wallet_index(WALLET_OPERATOR_INDEX),
        )
        .with_action(ProduceBlocks::new(1))
        .with_action(AssertTxIncluded::new("denylist_account").expect(TxStatus::Success))
        .with_action(AssertTxLogs::new("denylist_account").expect_log_count(1))
        .with_action(AssertDebankBlock::new(
            "denylist_account",
            move |_env, output, state_diff, transaction| {
                let (view, root) =
                    successful_call(output, transaction, DEFAULT_DENYLIST_ADDRESS, &input)?;
                ensure!(view.traces.len() == 2, "unexpected Denylist trace count");
                ensure!(view.events.len() == 1, "unexpected Denylist event count");
                ensure!(root.subtraces == 1, "Denylist root child count mismatch");
                ensure!(root.storage_change, "Denylist root missed storage change");

                let implementation = single_direct_child(
                    &view,
                    root,
                    |trace| trace.call_type == "delegatecall",
                    "Denylist implementation",
                )?;
                assert_child_position(implementation, root, 0, 0)?;
                ensure!(
                    implementation.self_storage_change,
                    "Denylist implementation missed its SSTORE"
                );
                ensure!(
                    implementation.storage_change,
                    "Denylist implementation missed storage change"
                );
                ensure!(
                    implementation.subtraces == 0,
                    "Denylist implementation has unexpected children"
                );

                let event = view.events[0];
                assert_event(
                    event,
                    DEFAULT_DENYLIST_ADDRESS,
                    IDenylist::Denylisted::SIGNATURE_HASH,
                    vec![address_topic(denied)],
                    implementation,
                    0,
                    0,
                )?;
                ensure!(event.data.is_empty(), "Denylisted event has data");
                assert_storage_changes(
                    output,
                    state_diff,
                    DEFAULT_DENYLIST_ADDRESS,
                    &[(
                        address_mapping_slot(denied, DENYLIST_STORAGE_LOCATION),
                        U256::from(1),
                    )],
                )
            },
        ))
        .run()
        .await
}

#[tokio::test]
async fn debank_block_native_blacklist_tracks_nested_precompile_state_and_events() -> Result<()> {
    reth_tracing::init_test_tracing();

    let blacklisted = Address::repeat_byte(0xb1);
    let input = Bytes::from(
        INativeFiatToken::blacklistCall {
            account: blacklisted,
        }
        .abi_encode(),
    );

    ArcTestBuilder::new()
        .with_setup(ArcSetup::new().with_chain_spec(current_mainnet_test_chain_spec()))
        .with_action(
            SendTransaction::new("blacklist_account")
                .with_to(NATIVE_FIAT_TOKEN_ADDRESS)
                .with_data(input.clone())
                .with_value(U256::ZERO)
                .with_gas_limit(250_000)
                .with_wallet_index(WALLET_OPERATOR_INDEX),
        )
        .with_action(ProduceBlocks::new(1))
        .with_action(AssertTxIncluded::new("blacklist_account").expect(TxStatus::Success))
        .with_action(AssertTxLogs::new("blacklist_account").expect_log_count(2))
        .with_action(AssertDebankBlock::new(
            "blacklist_account",
            move |_env, output, state_diff, transaction| {
                let (view, root) =
                    successful_call(output, transaction, NATIVE_FIAT_TOKEN_ADDRESS, &input)?;
                ensure!(
                    view.traces.len() == 5,
                    "unexpected NativeFiatToken trace count: {:?}",
                    view.traces
                );
                ensure!(
                    view.events.len() == 2,
                    "unexpected NativeFiatToken event count"
                );
                ensure!(
                    root.subtraces == 1,
                    "NativeFiatToken root child count mismatch"
                );
                ensure!(
                    !root.storage_change,
                    "NativeFiatToken root unexpectedly reports an EVM SSTORE"
                );

                let implementation = single_direct_child(
                    &view,
                    root,
                    |trace| trace.call_type == "delegatecall",
                    "NativeFiatToken implementation",
                )?;
                assert_child_position(implementation, root, 0, 0)?;
                ensure!(
                    !implementation.self_storage_change,
                    "NativeFiatToken implementation unexpectedly executed SSTORE"
                );
                ensure!(
                    !implementation.storage_change,
                    "NativeFiatToken implementation unexpectedly reports an EVM SSTORE"
                );
                ensure!(
                    implementation.subtraces == 2,
                    "NativeFiatToken implementation child count mismatch"
                );

                let owner_lookup = single_direct_child(
                    &view,
                    implementation,
                    |trace| {
                        trace.to_addr == NATIVE_FIAT_TOKEN_ADDRESS
                            && trace.call_type == "staticcall"
                    },
                    "NativeFiatToken owner lookup",
                )?;
                assert_child_position(owner_lookup, implementation, 0, 0)?;
                ensure!(
                    owner_lookup.subtraces == 1,
                    "owner lookup proxy child count mismatch"
                );
                let owner_implementation = single_direct_child(
                    &view,
                    owner_lookup,
                    |trace| trace.call_type == "delegatecall",
                    "NativeFiatToken owner implementation",
                )?;
                assert_child_position(owner_implementation, owner_lookup, 0, 0)?;
                ensure!(
                    owner_implementation.subtraces == 0,
                    "owner implementation has unexpected children"
                );

                let native_control = single_direct_child(
                    &view,
                    implementation,
                    |trace| trace.to_addr == NATIVE_COIN_CONTROL_ADDRESS,
                    "NativeCoinControl",
                )?;
                assert_child_position(native_control, implementation, 1, 1)?;
                ensure!(
                    native_control.from_addr == NATIVE_FIAT_TOKEN_ADDRESS,
                    "NativeCoinControl caller mismatch"
                );
                ensure!(
                    native_control.call_type == "call",
                    "NativeCoinControl call type mismatch"
                );
                ensure!(
                    native_control.subtraces == 0,
                    "NativeCoinControl has unexpected children"
                );
                ensure!(
                    native_control.output.as_ref() == U256::from(1).to_be_bytes::<32>().as_slice(),
                    "NativeCoinControl did not return true"
                );

                let native_event = view.events[0];
                assert_event(
                    native_event,
                    NATIVE_COIN_CONTROL_ADDRESS,
                    INativeCoinControl::Blocklisted::SIGNATURE_HASH,
                    vec![address_topic(blacklisted)],
                    native_control,
                    0,
                    0,
                )?;
                ensure!(native_event.data.is_empty(), "Blocklisted event has data");

                let token_event = view.events[1];
                assert_event(
                    token_event,
                    NATIVE_FIAT_TOKEN_ADDRESS,
                    INativeFiatToken::Blacklisted::SIGNATURE_HASH,
                    vec![address_topic(blacklisted)],
                    implementation,
                    2,
                    1,
                )?;
                ensure!(token_event.data.is_empty(), "Blacklisted event has data");

                ensure!(
                    !output
                        .block_file
                        .storage_contracts
                        .contains(&NATIVE_FIAT_TOKEN_ADDRESS),
                    "NativeFiatToken duplicated NativeCoinControl blacklist state"
                );
                assert_storage_changes(
                    output,
                    state_diff,
                    NATIVE_COIN_CONTROL_ADDRESS,
                    &[(
                        address_mapping_slot(
                            blacklisted,
                            NATIVE_COIN_CONTROL_BLOCKLIST_MAPPING_SLOT,
                        ),
                        U256::from(1),
                    )],
                )
            },
        ))
        .run()
        .await
}

#[tokio::test]
async fn debank_block_validator_registration_tracks_nested_registry_state_and_event() -> Result<()>
{
    reth_tracing::init_test_tracing();

    let public_key = Bytes::from(vec![0xa5; 32]);
    let input = Bytes::from(
        IPermissionedValidatorManager::registerValidatorCall {
            publicKey: public_key.clone(),
        }
        .abi_encode(),
    );

    ArcTestBuilder::new()
        .with_setup(ArcSetup::new().with_chain_spec(current_mainnet_test_chain_spec()))
        .with_action(
            SendTransaction::new("register_validator")
                .with_to(PERMISSIONED_VALIDATOR_MANAGER_ADDRESS)
                .with_data(input.clone())
                .with_value(U256::ZERO)
                .with_gas_limit(800_000)
                .with_wallet_index(WALLET_ADMIN_INDEX),
        )
        .with_action(ProduceBlocks::new(1))
        .with_action(AssertTxIncluded::new("register_validator").expect(TxStatus::Success))
        .with_action(AssertTxLogs::new("register_validator").expect_log_count(1))
        .with_action(AssertDebankBlock::new(
            "register_validator",
            move |_env, output, state_diff, transaction| {
                let (view, root) = successful_call(
                    output,
                    transaction,
                    PERMISSIONED_VALIDATOR_MANAGER_ADDRESS,
                    &input,
                )?;
                ensure!(
                    view.traces.len() == 4,
                    "unexpected validator registration trace count"
                );
                ensure!(
                    view.events.len() == 1,
                    "unexpected validator registration event count"
                );
                ensure!(root.subtraces == 1, "manager root child count mismatch");
                ensure!(
                    root.storage_change,
                    "manager root missed nested storage change"
                );
                ensure!(
                    !root.self_storage_change,
                    "manager proxy root reports its own SSTORE"
                );

                let manager_implementation = single_direct_child(
                    &view,
                    root,
                    |trace| trace.call_type == "delegatecall",
                    "manager implementation",
                )?;
                assert_child_position(manager_implementation, root, 0, 0)?;
                ensure!(
                    !manager_implementation.self_storage_change,
                    "manager implementation unexpectedly changed manager storage"
                );
                ensure!(
                    manager_implementation.storage_change,
                    "manager implementation missed nested registry change"
                );
                ensure!(
                    manager_implementation.subtraces == 1,
                    "manager implementation child count mismatch"
                );

                let registry = single_direct_child(
                    &view,
                    manager_implementation,
                    |trace| trace.to_addr == VALIDATOR_REGISTRY_ADDRESS,
                    "ValidatorRegistry proxy",
                )?;
                assert_child_position(registry, manager_implementation, 0, 0)?;
                ensure!(
                    registry.call_type == "call",
                    "registry proxy call type mismatch"
                );
                ensure!(
                    registry.subtraces == 1,
                    "registry proxy child count mismatch"
                );
                ensure!(
                    registry.storage_change,
                    "registry proxy missed storage change"
                );
                ensure!(
                    !registry.self_storage_change,
                    "registry proxy reports its own SSTORE"
                );

                let registry_implementation = single_direct_child(
                    &view,
                    registry,
                    |trace| trace.call_type == "delegatecall",
                    "ValidatorRegistry implementation",
                )?;
                assert_child_position(registry_implementation, registry, 0, 0)?;
                ensure!(
                    registry_implementation.self_storage_change,
                    "registry implementation missed its SSTORE"
                );
                ensure!(
                    registry_implementation.storage_change,
                    "registry implementation missed storage change"
                );
                ensure!(
                    registry_implementation.subtraces == 0,
                    "registry implementation has unexpected children"
                );

                ensure!(
                    root.output.len() == 32,
                    "registration return length mismatch"
                );
                ensure!(
                    manager_implementation.output == root.output
                        && registry.output == root.output
                        && registry_implementation.output == root.output,
                    "registration id was not forwarded through the call tree"
                );
                let registration_id = U256::from_be_slice(root.output.as_ref());
                let registration_id_topic = B256::from_slice(root.output.as_ref()).to_string();

                let event = view.events[0];
                assert_event(
                    event,
                    VALIDATOR_REGISTRY_ADDRESS,
                    IValidatorRegistry::ValidatorRegistered::SIGNATURE_HASH,
                    vec![registration_id_topic],
                    registry_implementation,
                    0,
                    0,
                )?;
                ensure!(
                    event.data.len() == 4 * 32,
                    "ValidatorRegistered data length mismatch"
                );
                ensure!(
                    event.data[0..32] == [0u8; 32],
                    "new validator voting power is not zero"
                );
                ensure!(
                    event.data[32..64] == U256::from(64).to_be_bytes::<32>(),
                    "ValidatorRegistered bytes offset mismatch"
                );
                ensure!(
                    event.data[64..96] == U256::from(32).to_be_bytes::<32>(),
                    "ValidatorRegistered public-key length mismatch"
                );
                ensure!(
                    event.data[96..128] == public_key[..],
                    "ValidatorRegistered public key mismatch"
                );

                let validator_slot =
                    keccak256((registration_id, VALIDATOR_REGISTRY_STORAGE_LOCATION).abi_encode());
                let public_key_slot = storage_slot_offset(validator_slot, 1);
                let public_key_hash = keccak256(public_key.as_ref());
                let registered_public_key_slot = keccak256(
                    (public_key_hash, VALIDATOR_REGISTERED_PUBLIC_KEYS_SLOT).abi_encode(),
                );
                assert_storage_changes(
                    output,
                    state_diff,
                    VALIDATOR_REGISTRY_ADDRESS,
                    &[
                        (validator_slot, U256::from(1)),
                        (public_key_slot, U256::from(65)),
                        (
                            keccak256(public_key_slot.as_slice()),
                            U256::from_be_slice(public_key.as_ref()),
                        ),
                        (registered_public_key_slot, U256::from(1)),
                        (
                            VALIDATOR_NEXT_REGISTRATION_ID_SLOT,
                            registration_id.saturating_add(U256::from(1)),
                        ),
                    ],
                )
            },
        ))
        .run()
        .await
}
