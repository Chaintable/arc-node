use alloy_eips::BlockId;
use alloy_primitives::{address, Bytes, B256, U256};
use alloy_rpc_types_eth::{state::StateOverridesBuilder, TransactionInput, TransactionRequest};
use alloy_sol_types::{sol, SolCall};
use arc_execution_e2e::{ArcSetup, ArcTestNode, TxKind};
const MEMO_ADDRESS: alloy_primitives::Address =
    address!("5294E9927c3306DcBaDb03fe70b92e01cCede505");
use debank_rpc::{
    erc20_handle::NATIVE_TOKEN_ADDRESS,
    types::{MultiCallResp, PreResult},
};
use eyre::{ensure, Result};
use jsonrpsee::{core::client::ClientT, rpc_params};

sol! { function memo(address target, bytes data, bytes32 memoId, bytes memoData) external; }

#[test]
fn native_balance_database_failure() {
    use reth_ethereum::evm::revm::{
        bytecode::Bytecode, database_interface::DBErrorMarker, state::AccountInfo, DatabaseRef,
    };
    #[derive(Debug)]
    struct ReadFailure;
    impl std::fmt::Display for ReadFailure {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("injected account read failure")
        }
    }
    impl std::error::Error for ReadFailure {}
    impl DBErrorMarker for ReadFailure {}
    struct FailingDb;
    impl DatabaseRef for FailingDb {
        type Error = ReadFailure;
        fn basic_ref(
            &self,
            _: alloy_primitives::Address,
        ) -> std::result::Result<Option<AccountInfo>, ReadFailure> {
            Err(ReadFailure)
        }
        fn code_by_hash_ref(&self, _: B256) -> std::result::Result<Bytecode, ReadFailure> {
            unreachable!()
        }
        fn storage_ref(
            &self,
            _: alloy_primitives::Address,
            _: U256,
        ) -> std::result::Result<U256, ReadFailure> {
            unreachable!()
        }
        fn block_hash_ref(&self, _: u64) -> std::result::Result<B256, ReadFailure> {
            unreachable!()
        }
    }
    let mut input = vec![0x70, 0xa0, 0x82, 0x31];
    input.extend_from_slice(&[0u8; 12]);
    input.extend_from_slice(&[0x11; 20]);
    let result = debank_rpc::erc20_handle::eth_erc20_handle(&FailingDb, Some(&input));
    assert_eq!(
        result.code,
        debank_rpc::types::MultiCallErrorCode::NativeMethodStateError as i32
    );
    assert!(result.err.contains("injected account read failure"));
    assert!(result.result.is_empty());
}

#[tokio::test]
async fn pre_trace_many_memo_eoa_output() -> Result<()> {
    let mut node = ArcTestNode::start(ArcSetup::new()).await?;
    node.produce_block().await?;
    let from = node.wallet_signer(0)?.address();
    let target = address!("000000000000000000000000000000000000bEEF");
    let input = Bytes::from(
        memoCall {
            target,
            data: Bytes::new(),
            memoId: B256::ZERO,
            memoData: Bytes::new(),
        }
        .abi_encode(),
    );
    let tx = TransactionRequest {
        from: Some(from),
        to: Some(TxKind::Call(MEMO_ADDRESS)),
        gas: Some(250_000),
        input: TransactionInput::new(input),
        ..Default::default()
    };
    let client = node.node.rpc_client().unwrap();
    let code: Bytes = client
        .request("eth_getCode", rpc_params![target, "latest"])
        .await?;
    ensure!(code.is_empty(), "EOA fixture must have no bytecode");
    let results: Vec<PreResult> = client
        .request("pre_traceMany", rpc_params![vec![tx], BlockId::latest()])
        .await?;
    ensure!(results[0].error.is_none());
    ensure!(results[0].trace.len() == 2);
    let child = &results[0].trace[1].trace;
    ensure!(
        child.result.as_ref().unwrap().output().is_empty(),
        "empty EOA child exposes wrapper ABI output"
    );
    ensure!(child.result.as_ref().unwrap().gas_used() == 0);
    ensure!(child.trace_address == vec![0]);
    Ok(())
}

#[tokio::test]
async fn native_balance_override_order() -> Result<()> {
    let mut node = ArcTestNode::start(ArcSetup::new()).await?;
    node.produce_block().await?;
    let from = node.wallet_signer(0)?.address();
    let target = address!("000000000000000000000000000000000000bEEF");
    let mut input = vec![0x70, 0xa0, 0x82, 0x31];
    input.extend_from_slice(&[0u8; 12]);
    input.extend_from_slice(target.as_slice());
    let native = TransactionRequest {
        from: Some(from),
        to: Some(TxKind::Call(NATIVE_TOKEN_ADDRESS)),
        gas: Some(200_000),
        input: TransactionInput::new(Bytes::from(input)),
        ..Default::default()
    };
    let noop = TransactionRequest {
        from: Some(from),
        to: Some(TxKind::Call(target)),
        gas: Some(200_000),
        ..Default::default()
    };
    let overrides = StateOverridesBuilder::default()
        .with_balance(target, U256::from(123))
        .build();
    let client = node.node.rpc_client().unwrap();
    let initial_balance: U256 = client
        .request("eth_getBalance", rpc_params![target, "latest"])
        .await?;
    ensure!(
        initial_balance.is_zero(),
        "balance fixture must initially be zero"
    );
    let first: MultiCallResp = client
        .request(
            "eth_multiCall",
            rpc_params![
                vec![native.clone()],
                BlockId::latest(),
                false,
                false,
                true,
                Some(overrides.clone())
            ],
        )
        .await?;
    let second: MultiCallResp = client
        .request(
            "eth_multiCall",
            rpc_params![
                vec![native.clone(), noop, native],
                BlockId::latest(),
                false,
                false,
                true,
                Some(overrides)
            ],
        )
        .await?;
    ensure!(first.stats.success && second.stats.success);
    ensure!(first.results.len() == 1 && second.results.len() == 3);
    ensure!(first
        .results
        .iter()
        .chain(&second.results)
        .all(|r| r.code == 0));
    ensure!(U256::from_be_slice(&second.results[0].result) == U256::from(123));
    ensure!(U256::from_be_slice(&second.results[2].result) == U256::from(123));
    ensure!(
        U256::from_be_slice(&first.results[0].result) == U256::from(123),
        "native sentinel ignores balance override before first ordinary call"
    );
    let final_balance: U256 = client
        .request("eth_getBalance", rpc_params![target, "latest"])
        .await?;
    ensure!(
        final_balance == initial_balance,
        "override must not modify canonical state"
    );
    Ok(())
}
