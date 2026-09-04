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

//! Launched-node `trace_debankBlock` coverage for an EIP-7702 set-code transaction.

use alloy_consensus::TxEnvelope;
use alloy_eips::{
    eip7702::constants::{EIP7702_DELEGATION_DESIGNATOR, EIP7702_TX_TYPE_ID},
    BlockId, Typed2718,
};
use alloy_primitives::{bytes, keccak256, Address, Bytes, U256};
use alloy_rpc_types_eth::{BlockNumberOrTag, TransactionInput, TransactionRequest};
use arc_execution_e2e::{chainspec::LOCAL_DEV, ArcSetup, ArcTestNode, TxKind};
use eyre::{ensure, Result};
use jsonrpsee::{core::client::ClientT, rpc_params};
use reth_chainspec::EthChainSpec;
use reth_e2e_test_utils::transaction::TransactionTestContext;

use super::helpers::{
    contracts::payable_contract_deploy_code,
    debank::{assert_debank_block, assert_eip7708_transfer, transaction_view},
};

fn delegation_code(delegate: Address) -> Bytes {
    let mut code = Vec::with_capacity(23);
    code.extend_from_slice(&EIP7702_DELEGATION_DESIGNATOR);
    code.extend_from_slice(delegate.as_slice());
    code.into()
}

#[tokio::test]
async fn debank_block_eip7702_records_execution_and_delegation_state() -> Result<()> {
    reth_tracing::init_test_tracing();

    let mut node = ArcTestNode::start(ArcSetup::new()).await?;
    let sponsor = node.wallet_signer(0)?;
    let authority_signer = node.wallet_signer(1)?;
    let sponsor_address = sponsor.address();
    let authority = authority_signer.address();
    let chain_id = LOCAL_DEV.chain().id();
    let delegate = sponsor_address.create(0);
    let input = bytes!("deadbeef");
    let value = U256::from(12_345);
    let expected_code = delegation_code(delegate);
    let expected_code_hash = keccak256(&expected_code);
    let expected_state_code = expected_code.clone();

    let deploy_hash = node
        .send_tx(
            sponsor.clone(),
            TransactionRequest {
                from: Some(sponsor_address),
                to: Some(TxKind::Create),
                value: Some(U256::ZERO),
                gas: Some(100_000),
                input: TransactionInput::new(payable_contract_deploy_code()),
                ..Default::default()
            },
        )
        .await?;

    let sponsor_nonce = node
        .nonce(
            sponsor_address,
            Some(BlockId::Number(BlockNumberOrTag::Pending)),
        )
        .await?;
    ensure!(
        sponsor_nonce == 1,
        "set-code sponsor nonce must follow the delegate deployment"
    );

    // Reth's test utility signs the authorization with `authority_signer`. We discard its
    // dummy outer transaction and sponsor the same authorization from wallet 0 below.
    let authority_envelope =
        TransactionTestContext::set_code_tx(chain_id, delegate, authority_signer).await;
    let signed_authorization = match authority_envelope {
        TxEnvelope::Eip7702(transaction) => transaction
            .tx()
            .authorization_list
            .first()
            .cloned()
            .ok_or_else(|| eyre::eyre!("set-code helper returned no authorization"))?,
        other => {
            return Err(eyre::eyre!(
                "set-code helper returned transaction type {}",
                other.ty()
            ));
        }
    };
    ensure!(
        signed_authorization.recover_authority()? == authority,
        "authorization recovered the wrong authority"
    );
    ensure!(
        *signed_authorization.chain_id() == U256::from(chain_id),
        "authorization chain id mismatch"
    );
    ensure!(
        *signed_authorization.address() == delegate,
        "authorization delegate mismatch"
    );
    ensure!(
        signed_authorization.nonce() == 0,
        "authorization nonce mismatch"
    );

    let request = TransactionRequest {
        from: Some(sponsor_address),
        nonce: Some(sponsor_nonce),
        value: Some(value),
        to: Some(TxKind::Call(authority)),
        gas: Some(100_000),
        max_fee_per_gas: Some(1_000_000_000_000),
        max_priority_fee_per_gas: Some(1_000_000_000),
        chain_id: Some(chain_id),
        transaction_type: Some(EIP7702_TX_TYPE_ID),
        input: TransactionInput::new(input.clone()),
        authorization_list: Some(vec![signed_authorization]),
        ..Default::default()
    };
    let signed_transaction = TransactionTestContext::sign_tx(sponsor, request).await;
    ensure!(
        matches!(&signed_transaction, TxEnvelope::Eip7702(_)),
        "transaction builder did not produce an EIP-7702 envelope"
    );
    let set_code_hash = node.send_signed_tx(signed_transaction.into()).await?;

    // The delegate deployment and sponsored type-4 transaction intentionally share a block.
    node.produce_block().await?;
    let deploy_receipt = node.get_receipt(deploy_hash).await?;
    let set_code_receipt = node.get_receipt(set_code_hash).await?;
    ensure!(deploy_receipt.status(), "delegate deployment failed");
    ensure!(
        deploy_receipt.logs().is_empty(),
        "delegate deployment emitted logs"
    );
    ensure!(set_code_receipt.status(), "set-code transaction failed");
    ensure!(
        set_code_receipt.logs().len() == 1,
        "expected one set-code receipt log"
    );
    ensure!(
        deploy_receipt.block_number == set_code_receipt.block_number,
        "delegate deployment and set-code transaction landed in different blocks"
    );
    let block_number = set_code_receipt
        .block_number
        .ok_or_else(|| eyre::eyre!("set-code receipt has no block number"))?;

    assert_debank_block(
        &node,
        block_number,
        set_code_hash,
        move |output, state_diff, transaction| {
            ensure!(transaction.status, "set-code transaction failed");
            ensure!(
                transaction.from == sponsor_address,
                "unexpected set-code sponsor"
            );
            ensure!(transaction.to == authority, "unexpected set-code target");
            ensure!(transaction.input == input, "set-code input mismatch");
            ensure!(transaction.value == value, "set-code value mismatch");
            ensure!(transaction.nonce == 1, "set-code nonce mismatch");
            ensure!(
                transaction.transaction_index == 1,
                "set-code transaction must follow delegate deployment"
            );

            let view = transaction_view(output, transaction);
            ensure!(view.traces.len() == 1, "expected one set-code root trace");
            ensure!(
                view.error_traces.is_empty(),
                "set-code transaction produced an error trace"
            );
            ensure!(view.events.len() == 1, "expected one native Transfer event");
            ensure!(
                view.error_events.is_empty(),
                "set-code transaction produced an error event"
            );
            let root = view
                .traces
                .first()
                .ok_or_else(|| eyre::eyre!("set-code root trace is missing"))?;
            ensure!(
                root.parent_trace_id.is_empty(),
                "set-code root has a parent"
            );
            ensure!(
                root.trace_address.is_empty(),
                "set-code root has a trace address"
            );
            ensure!(root.from_addr == transaction.from, "root caller mismatch");
            ensure!(root.to_addr == authority, "root target mismatch");
            ensure!(root.input == input, "root input mismatch");
            ensure!(root.value == value, "root value mismatch");
            ensure!(root.output.is_empty(), "no-op delegate returned data");
            ensure!(root.call_create_type == "call", "root is not a CALL");
            ensure!(root.call_type == "call", "unexpected root call type");
            ensure!(root.subtraces == 0, "set-code call produced child traces");
            ensure!(root.error.is_empty(), "set-code root is marked failed");
            let event = view
                .events
                .first()
                .ok_or_else(|| eyre::eyre!("native Transfer event is missing"))?;
            assert_eip7708_transfer(event, transaction.from, authority, value, &root.id, 0, 0)?;

            let authority_account = state_diff
                .new_accounts
                .iter()
                .find(|account| account.address == keccak256(authority.as_slice()))
                .ok_or_else(|| eyre::eyre!("authority is missing from StateDiff"))?;
            ensure!(authority_account.nonce == 1, "authority nonce mismatch");
            ensure!(
                authority_account.code_hash == expected_code_hash,
                "authority code hash mismatch"
            );
            let matching_codes: Vec<_> = state_diff
                .new_codes
                .iter()
                .filter(|code| code.code_hash == expected_code_hash)
                .collect();
            ensure!(
                matching_codes.len() == 1,
                "expected exactly one delegation NewCode"
            );
            let delegation = matching_codes
                .first()
                .ok_or_else(|| eyre::eyre!("delegation NewCode is missing"))?;
            ensure!(
                delegation.code == expected_state_code,
                "delegation NewCode bytes mismatch"
            );

            // BlockFile has no standard EIP-7702 authorization-list field. In particular,
            // the Tempo AA field must remain unset for a standard type-4 transaction.
            ensure!(
                transaction.aa_authorization_list.is_none(),
                "standard authorization was projected into the Tempo AA field"
            );
            let transaction_json = serde_json::to_value(transaction)?;
            ensure!(
                transaction_json.get("authorization_list").is_none()
                    && transaction_json.get("authorizationList").is_none(),
                "BlockFile unexpectedly carries a standard authorization list"
            );
            Ok(())
        },
    )
    .await?;

    let client = node
        .node
        .rpc_client()
        .ok_or_else(|| eyre::eyre!("RPC client not available"))?;
    let actual_code: Bytes = client
        .request(
            "eth_getCode",
            rpc_params![authority, BlockId::number(block_number)],
        )
        .await?;
    ensure!(
        actual_code == expected_code,
        "authority delegation code mismatch"
    );

    Ok(())
}
