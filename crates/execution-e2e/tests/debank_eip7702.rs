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

mod helpers;

use alloy_consensus::TxEnvelope;
use alloy_eips::{
    eip7702::constants::{EIP7702_DELEGATION_DESIGNATOR, EIP7702_TX_TYPE_ID},
    BlockId, Typed2718,
};
use alloy_network::eip2718::{Decodable2718, Encodable2718};
use alloy_primitives::{bytes, keccak256, Address, Bytes, TxKind, U256};
use alloy_rpc_types_eth::{TransactionInput, TransactionRequest};
use arc_execution_e2e::{
    actions::{AssertTxIncluded, AssertTxLogs, ProduceBlocks, SendTransaction, TxStatus},
    Action, ArcEnvironment, ArcSetup, ArcTestBuilder,
};
use eyre::{ensure, Result};
use futures_util::future::BoxFuture;
use jsonrpsee::{core::client::ClientT, rpc_params};
use reth_e2e_test_utils::transaction::TransactionTestContext;
use reth_ethereum_primitives::TransactionSigned;
use reth_primitives_traits::SignerRecoverable;
use reth_transaction_pool::{TransactionOrigin, TransactionPool};

use helpers::{
    constants::WALLET_FIRST_ADDRESS,
    contracts::payable_contract_deploy_code,
    debank::{
        assert_eip7708_transfer, current_mainnet_test_chain_spec, transaction_view,
        AssertDebankBlock,
    },
};

const SET_CODE_TX: &str = "set_code";
const AUTHORITY_ADDRESS: &str = "eip7702_authority";

#[derive(Debug)]
struct SendSponsoredEip7702 {
    delegate: Address,
    input: Bytes,
    value: U256,
}

impl Action for SendSponsoredEip7702 {
    fn execute<'a>(&'a mut self, env: &'a mut ArcEnvironment) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let (sponsor, authority, chain_id) = {
                let wallet = env.wallet_mut()?;
                let wallets = wallet.wallet_gen();
                let sponsor = wallets
                    .first()
                    .ok_or_else(|| eyre::eyre!("sponsor wallet is unavailable"))?
                    .clone();
                let authority = wallets
                    .get(1)
                    .ok_or_else(|| eyre::eyre!("authority wallet is unavailable"))?
                    .clone();
                (sponsor, authority, wallet.chain_id)
            };
            let sponsor_nonce = env.next_nonce_for_wallet(0)?;
            ensure!(
                sponsor_nonce == 1,
                "set-code sponsor nonce must follow the delegate deployment"
            );

            // Reth's test utility signs the authorization with `authority`. We discard its
            // dummy outer transaction and sponsor the same authorization from wallet 0 below.
            let authority_envelope =
                TransactionTestContext::set_code_tx(chain_id, self.delegate, authority.clone())
                    .await;
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
            let authority_address = authority.address();
            ensure!(
                signed_authorization.recover_authority()? == authority_address,
                "authorization recovered the wrong authority"
            );
            ensure!(
                *signed_authorization.chain_id() == U256::from(chain_id),
                "authorization chain id mismatch"
            );
            ensure!(
                *signed_authorization.address() == self.delegate,
                "authorization delegate mismatch"
            );
            ensure!(
                signed_authorization.nonce() == 0,
                "authorization nonce mismatch"
            );

            let request = TransactionRequest {
                nonce: Some(sponsor_nonce),
                value: Some(self.value),
                to: Some(TxKind::Call(authority_address)),
                gas: Some(100_000),
                max_fee_per_gas: Some(1_000_000_000_000),
                max_priority_fee_per_gas: Some(1_000_000_000),
                chain_id: Some(chain_id),
                transaction_type: Some(EIP7702_TX_TYPE_ID),
                input: TransactionInput {
                    input: None,
                    data: Some(self.input.clone()),
                },
                authorization_list: Some(vec![signed_authorization]),
                ..Default::default()
            };
            let signed_transaction = TransactionTestContext::sign_tx(sponsor, request).await;
            ensure!(
                matches!(signed_transaction, TxEnvelope::Eip7702(_)),
                "transaction builder did not produce an EIP-7702 envelope"
            );
            let tx_hash = *signed_transaction.tx_hash();
            let raw_transaction: Bytes = signed_transaction.encoded_2718().into();
            let transaction = TransactionSigned::decode_2718(&mut raw_transaction.as_ref())
                .map_err(|error| eyre::eyre!("failed to decode set-code transaction: {error}"))?;
            let recovered = transaction
                .try_into_recovered()
                .map_err(|error| eyre::eyre!("failed to recover set-code sponsor: {error}"))?;

            env.node()
                .inner
                .pool
                .add_consensus_transaction(recovered, TransactionOrigin::Local)
                .await
                .map_err(|error| eyre::eyre!("failed to submit set-code transaction: {error}"))?;
            env.insert_tx_hash(SET_CODE_TX.to_string(), tx_hash)?;
            env.insert_address(AUTHORITY_ADDRESS.to_string(), authority_address)
        })
    }
}

#[derive(Debug)]
struct AssertAuthorityCode {
    expected: Bytes,
}

impl Action for AssertAuthorityCode {
    fn execute<'a>(&'a mut self, env: &'a mut ArcEnvironment) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let authority = *env
                .get_address(AUTHORITY_ADDRESS)
                .ok_or_else(|| eyre::eyre!("authority address is unavailable"))?;
            let client = env
                .node()
                .rpc_client()
                .ok_or_else(|| eyre::eyre!("RPC client not available"))?;
            let actual: Bytes = client
                .request(
                    "eth_getCode",
                    rpc_params![authority, BlockId::number(env.block_number())],
                )
                .await?;
            ensure!(
                actual == self.expected,
                "authority delegation code mismatch"
            );
            Ok(())
        })
    }
}

fn delegation_code(delegate: Address) -> Bytes {
    let mut code = Vec::with_capacity(23);
    code.extend_from_slice(&EIP7702_DELEGATION_DESIGNATOR);
    code.extend_from_slice(delegate.as_slice());
    code.into()
}

#[tokio::test]
async fn debank_block_eip7702_records_execution_and_delegation_state() -> Result<()> {
    reth_tracing::init_test_tracing();

    let delegate = WALLET_FIRST_ADDRESS.create(0);
    let input = bytes!("deadbeef");
    let value = U256::from(12_345);
    let expected_code = delegation_code(delegate);
    let expected_code_hash = keccak256(&expected_code);
    let expected_state_code = expected_code.clone();

    ArcTestBuilder::new()
        .with_setup(ArcSetup::new().with_chain_spec(current_mainnet_test_chain_spec()))
        .with_action(
            SendTransaction::new("deploy_delegate")
                .with_create()
                .with_data(payable_contract_deploy_code())
                .with_value(U256::ZERO)
                .with_gas_limit(100_000),
        )
        .with_action(SendSponsoredEip7702 {
            delegate,
            input: input.clone(),
            value,
        })
        .with_action(ProduceBlocks::new(1))
        .with_action(AssertTxIncluded::new("deploy_delegate").expect(TxStatus::Success))
        .with_action(AssertTxIncluded::new(SET_CODE_TX).expect(TxStatus::Success))
        .with_action(AssertTxLogs::new(SET_CODE_TX).expect_log_count(1))
        .with_action(AssertDebankBlock::new(
            SET_CODE_TX,
            move |env, output, state_diff, transaction| {
                let authority = *env
                    .get_address(AUTHORITY_ADDRESS)
                    .ok_or_else(|| eyre::eyre!("authority address is unavailable"))?;
                ensure!(transaction.status, "set-code transaction failed");
                ensure!(
                    transaction.from == WALLET_FIRST_ADDRESS,
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
        ))
        .with_action(AssertAuthorityCode {
            expected: expected_code,
        })
        .run()
        .await
}
