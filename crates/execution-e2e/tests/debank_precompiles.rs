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

//! Canonical-transaction `trace_debankBlock` coverage for stateless Arc precompiles.

mod helpers;

use alloy_consensus::constants::KECCAK_EMPTY;
use alloy_eips::eip2935::HISTORY_STORAGE_ADDRESS;
use alloy_primitives::{address, bytes, keccak256, Address, Bytes, B256, U256};
use alloy_sol_types::SolCall;
use arc_execution_e2e::{
    actions::{ProduceBlocks, SendTransaction},
    ArcSetup, ArcTestBuilder,
};
use arc_precompiles::{
    helpers::revert_message_to_bytes,
    pq::{IPQ, PQ_ADDRESS},
    pq_test_vectors::{self, MSG_HELLO_WORLD},
    system_accounting::SYSTEM_ACCOUNTING_ADDRESS,
};
use debank_rpc::debank_trace::{BlockStorageDiff, DebankOutPut, DebankTrace, DebankTransaction};
use eyre::{ensure, Result};
use helpers::debank::{current_mainnet_test_chain_spec, AssertDebankBlock};

const P256_PRECOMPILE_ADDRESS: Address = address!("0000000000000000000000000000000000000100");
const RETURN_TRUE: Bytes =
    bytes!("0000000000000000000000000000000000000000000000000000000000000001");
const P256_VALID_INPUT: Bytes = bytes!(
    "4cee90eb86eaa050036147a12d49004b6b9c72bd725d39d4785011fe190f0b4d\
     a73bd4903f0ce3b639bbbf6e8e80d16931ff4bcf5993d58468e8fb19086e8cac\
     36dbcd03009df8c59286b162af3bd7fcc0450c9aa81be5d10d312af6c66b1d604\
     aebd3099c618202fcfe16ae7770b0c49ab5eadf74b754204a3bb6060e44eff376\
     18b065f9832de4ca6ca971a7a1adc826d0f7c00181a5fb2ddf79ae00b4e10e"
);

#[derive(Clone, Debug)]
enum ExpectedOutcome {
    Success(Bytes),
    Revert(Bytes),
}

#[derive(Clone, Debug)]
struct PrecompileFixture {
    target: Address,
    input: Bytes,
    outcome: ExpectedOutcome,
}

fn assert_precompile_block(tx_name: &'static str, fixture: PrecompileFixture) -> AssertDebankBlock {
    AssertDebankBlock::new(tx_name, move |_env, output, state_diff, transaction| {
        assert_transaction(transaction, &fixture)?;
        assert_trace_buckets(output, transaction, &fixture)?;
        assert_no_business_events_or_state(output, state_diff, transaction, fixture.target)
    })
}

fn assert_transaction(transaction: &DebankTransaction, fixture: &PrecompileFixture) -> Result<()> {
    let expected_status = matches!(&fixture.outcome, ExpectedOutcome::Success(_));
    ensure!(
        transaction.to == fixture.target,
        "unexpected transaction target"
    );
    ensure!(
        transaction.input == fixture.input,
        "unexpected transaction input"
    );
    ensure!(
        transaction.value == U256::ZERO,
        "precompile transaction transferred value"
    );
    ensure!(
        transaction.status == expected_status,
        "unexpected transaction status"
    );
    ensure!(
        transaction.transaction_index == 0,
        "unexpected transaction index"
    );
    Ok(())
}

fn assert_trace_buckets(
    output: &DebankOutPut,
    transaction: &DebankTransaction,
    fixture: &PrecompileFixture,
) -> Result<()> {
    ensure!(
        output.block_file.transactions.len() == 1,
        "precompile fixture block must contain exactly one transaction"
    );
    let (traces, other_bucket, expected_output, expected_error) = match &fixture.outcome {
        ExpectedOutcome::Success(expected_output) => (
            &output.block_file.traces,
            &output.block_file.error_traces,
            expected_output,
            "",
        ),
        ExpectedOutcome::Revert(expected_output) => (
            &output.block_file.error_traces,
            &output.block_file.traces,
            expected_output,
            "Reverted",
        ),
    };

    ensure!(
        traces.len() == 1,
        "expected exactly one trace in the selected bucket"
    );
    ensure!(
        other_bucket.is_empty(),
        "trace was also emitted in the wrong bucket"
    );
    let trace = &traces[0];
    assert_root_precompile_trace(trace, transaction, fixture, expected_output, expected_error)
}

fn assert_root_precompile_trace(
    trace: &DebankTrace,
    transaction: &DebankTransaction,
    fixture: &PrecompileFixture,
    expected_output: &Bytes,
    expected_error: &str,
) -> Result<()> {
    ensure!(
        trace.tx_id == transaction.id,
        "trace belongs to another transaction"
    );
    ensure!(
        trace.parent_trace_id.is_empty(),
        "precompile root trace has a parent"
    );
    ensure!(
        trace.trace_address.is_empty(),
        "precompile root has a non-root trace address"
    );
    ensure!(
        trace.pos_in_parent_trace == 0,
        "unexpected root trace position"
    );
    ensure!(
        trace.from_addr == transaction.from,
        "unexpected trace caller"
    );
    ensure!(trace.to_addr == fixture.target, "unexpected trace target");
    ensure!(trace.input == fixture.input, "unexpected trace input");
    ensure!(trace.output == *expected_output, "unexpected trace output");
    ensure!(
        trace.value == U256::ZERO,
        "precompile trace transferred value"
    );
    ensure!(
        trace.call_create_type == "call",
        "precompile root is not a call"
    );
    ensure!(trace.call_type == "call", "unexpected precompile call type");
    ensure!(trace.error == expected_error, "unexpected trace error");
    ensure!(
        trace.subtraces == 0,
        "stateless precompile emitted child traces"
    );
    ensure!(
        !trace.self_storage_change,
        "precompile trace reports its own storage change"
    );
    ensure!(
        !trace.storage_change,
        "precompile trace reports a storage change"
    );
    Ok(())
}

fn assert_no_business_events_or_state(
    output: &DebankOutPut,
    state_diff: &BlockStorageDiff,
    transaction: &DebankTransaction,
    target: Address,
) -> Result<()> {
    ensure!(
        output.block_file.events.is_empty(),
        "stateless precompile emitted an event"
    );
    ensure!(
        output.block_file.error_events.is_empty(),
        "stateless precompile emitted an error event"
    );

    // Every non-genesis Arc block updates exactly the EIP-2935 history contract and
    // SystemAccounting. Those block-level hooks are not precompile business state.
    let system_contracts = vec![HISTORY_STORAGE_ADDRESS, SYSTEM_ACCOUNTING_ADDRESS];
    ensure!(
        output.block_file.storage_contracts == system_contracts,
        "stateless precompile changed business storage: {:?}",
        output.block_file.storage_contracts
    );
    let mut expected_storage_addresses: Vec<_> = system_contracts
        .iter()
        .map(|address| keccak256(address.as_slice()))
        .collect();
    expected_storage_addresses.sort_unstable();
    let storage_addresses: Vec<_> = state_diff
        .storage_diffs
        .iter()
        .map(|account| account.address)
        .collect();
    ensure!(
        storage_addresses == expected_storage_addresses,
        "StateDiff contains non-hook storage changes"
    );
    ensure!(
        state_diff
            .storage_diffs
            .iter()
            .all(|account| account.diffs.len() == 1),
        "each block hook must change exactly one slot"
    );
    ensure!(
        state_diff.deleted_accounts.is_empty(),
        "precompile deleted an account"
    );
    // The localdev fee recipient is absent from genesis, so block 1 may represent
    // its account creation with empty bytecode. Non-empty code is business state.
    ensure!(
        state_diff.new_codes.len() <= 1
            && state_diff
                .new_codes
                .iter()
                .all(|code| code.code_hash == KECCAK_EMPTY && code.code.is_empty()),
        "precompile installed non-empty code: {:?}",
        state_diff.new_codes
    );

    // Account summaries may contain transaction bookkeeping (sender nonce/balance and
    // beneficiary fees) plus the two block hooks. No other account, especially the
    // stateless precompile address, may enter the diff.
    let mut allowed_account_hashes = vec![
        keccak256(transaction.from.as_slice()),
        keccak256(output.block_file.block.miner.as_slice()),
        keccak256(HISTORY_STORAGE_ADDRESS.as_slice()),
        keccak256(SYSTEM_ACCOUNTING_ADDRESS.as_slice()),
    ];
    allowed_account_hashes.sort_unstable();
    allowed_account_hashes.dedup();
    let unexpected_accounts: Vec<B256> = state_diff
        .new_accounts
        .iter()
        .map(|account| account.address)
        .filter(|address| !allowed_account_hashes.contains(address))
        .collect();
    ensure!(
        unexpected_accounts.is_empty(),
        "StateDiff contains unexpected account summaries: {unexpected_accounts:?}"
    );
    let target_hash = keccak256(target.as_slice());
    ensure!(
        state_diff
            .new_accounts
            .iter()
            .all(|account| account.address != target_hash),
        "stateless precompile target appears in account StateDiff"
    );
    ensure!(
        state_diff
            .new_accounts
            .iter()
            .any(|account| { account.address == keccak256(transaction.from.as_slice()) }),
        "sender nonce/balance bookkeeping is missing from StateDiff"
    );
    Ok(())
}

async fn run_fixture(tx_name: &'static str, fixture: PrecompileFixture) -> Result<()> {
    let chain_spec = current_mainnet_test_chain_spec();

    ArcTestBuilder::new()
        .with_setup(ArcSetup::new().with_chain_spec(chain_spec))
        .with_action(
            SendTransaction::new(tx_name)
                .with_to(fixture.target)
                .with_value(U256::ZERO)
                .with_data(fixture.input.clone())
                .with_gas_limit(750_000),
        )
        .with_action(ProduceBlocks::new(1))
        .with_action(assert_precompile_block(tx_name, fixture))
        .run()
        .await
}

#[tokio::test]
async fn debank_pq_valid_canonical_transaction() -> Result<()> {
    reth_tracing::init_test_tracing();

    let vectors = pq_test_vectors::cached_vectors();
    let input = IPQ::verifySlhDsaSha2128sCall {
        vk: vectors.verifying_key.clone().into(),
        message: Bytes::copy_from_slice(MSG_HELLO_WORLD),
        sig: vectors.sig_hello_world.clone().into(),
    }
    .abi_encode()
    .into();
    run_fixture(
        "pq_valid",
        PrecompileFixture {
            target: PQ_ADDRESS,
            input,
            outcome: ExpectedOutcome::Success(RETURN_TRUE.clone()),
        },
    )
    .await
}

#[tokio::test]
async fn debank_pq_malformed_canonical_transaction() -> Result<()> {
    reth_tracing::init_test_tracing();

    let vectors = pq_test_vectors::cached_vectors();
    let input = IPQ::verifySlhDsaSha2128sCall {
        vk: Bytes::from(vec![0u8; 100]),
        message: Bytes::copy_from_slice(MSG_HELLO_WORLD),
        sig: vectors.sig_hello_world.clone().into(),
    }
    .abi_encode()
    .into();
    run_fixture(
        "pq_malformed",
        PrecompileFixture {
            target: PQ_ADDRESS,
            input,
            outcome: ExpectedOutcome::Revert(revert_message_to_bytes(
                "Invalid verifying key length",
            )),
        },
    )
    .await
}

#[tokio::test]
async fn debank_p256_valid_canonical_transaction() -> Result<()> {
    reth_tracing::init_test_tracing();

    run_fixture(
        "p256_valid",
        PrecompileFixture {
            target: P256_PRECOMPILE_ADDRESS,
            input: P256_VALID_INPUT.clone(),
            outcome: ExpectedOutcome::Success(RETURN_TRUE.clone()),
        },
    )
    .await
}
