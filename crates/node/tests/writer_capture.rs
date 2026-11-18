//! Targeted acceptance of the capture adapters against frozen Arc mainnet data.
#![allow(clippy::unwrap_used)]

use std::{
    fs,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use alloy_primitives::{Bytes, B256};
use alloy_rlp::Decodable;
use alloy_rpc_types_engine::ExecutionData;
use arc_evm::{ArcEvmConfig, ArcEvmFactory};
use arc_evm_node::{
    node::ArcNode,
    writer::{launcher::WriterNodeLauncher, WriterValidatorBuilder},
};
use arc_execution_config::{addresses_denylist::AddressesDenylistConfig, chainspec::MAINNET};
use debank_rpc::writer::{capture_genesis, WriterEvmConfig};
use pipeline_rs_arc::{CaptureSink, CaptureSource, CapturedArcBlock, SpoolError};
use reth_chainspec::EthChainSpec;
use reth_ethereum::{evm::EthEvmConfig, Block};
use reth_evm::{execute::Executor, ConfigureEvm};
use reth_node_builder::{DebugNodeLauncher, Node, NodeBuilder, NodeConfig};
use reth_primitives_traits::{AlloyBlockHeader, RecoveredBlock, SealedBlock};
use reth_provider::StateProviderFactory;
use reth_revm::database::StateProviderDatabase;
use reth_tasks::Runtime;
use serde_json::{json, Value};

#[derive(Default)]
struct CaptureLog(Mutex<Vec<(CapturedArcBlock, CaptureSource)>>);

impl CaptureSink for CaptureLog {
    fn seal(&self, block: CapturedArcBlock, source: CaptureSource) -> Result<(), SpoolError> {
        self.0.lock().unwrap().push((block, source));
        Ok(())
    }
}

fn fixture(height: u64) -> eyre::Result<Value> {
    let dir = PathBuf::from(
        std::env::var_os("ARC_WRITER_FIXTURES")
            .ok_or_else(|| eyre::eyre!("ARC_WRITER_FIXTURES is required"))?,
    );
    Ok(serde_json::from_slice(&fs::read(
        dir.join(format!("{height}.json")),
    )?)?)
}

fn equivalent(capture: &CapturedArcBlock, reference: &Value) -> eyre::Result<()> {
    let mut actual = json!({
        "block_file": capture.block_file,
        "header": capture.header,
        "state_diff": capture.state_diff_rlp,
        "validation_hash": capture.validation.validation_hash,
    });
    let mut reference = reference.clone();
    for payload in [&mut actual, &mut reference] {
        payload["block_file"]["block"]
            .as_object_mut()
            .unwrap()
            .remove("process_start_timestamp");
    }
    assert_eq!(actual, reference);
    Ok(())
}

#[tokio::test]
#[ignore = "requires ARC_WRITER_FIXTURES with frozen Writer 830ad37 blocks 0 and 1"]
async fn real_mainnet_genesis_and_block_one() -> eyre::Result<()> {
    let genesis_input = fixture(0)?;
    let input = fixture(1)?;
    let raw: Bytes = serde_json::from_value(input["raw_block"].clone())?;
    let mut encoded = raw.as_ref();
    let block = Block::decode(&mut encoded)?;
    assert!(encoded.is_empty());
    let block = RecoveredBlock::try_recover(block)?;
    assert_eq!(
        block.hash(),
        serde_json::from_value::<B256>(input["block"]["hash"].clone())?
    );

    let spec = MAINNET.clone();
    let genesis = SealedBlock::seal_slow(Block {
        header: spec.genesis_header().clone(),
        body: Default::default(),
    });
    let captured_genesis = capture_genesis(spec.genesis(), &genesis)?;
    equivalent(&captured_genesis, &genesis_input["writer"])?;
    assert_eq!(captured_genesis.validation.txs_count, 273);
    assert_eq!(captured_genesis.validation.traces_count, 273);
    assert_eq!(captured_genesis.validation.validation_hash, 356_679);

    let runtime = Runtime::test();
    let plain_config = ArcEvmConfig::new(EthEvmConfig::new_with_evm_factory(
        spec.clone(),
        ArcEvmFactory::new(spec.clone()),
    ));
    let mut config = NodeConfig::new(spec).with_unused_ports();
    config.network.discovery.disable_discovery = true;
    config.network.bootnodes = Some(Vec::new());
    let plain = NodeBuilder::new(config.clone())
        .testing_node(runtime.clone())
        .node(ArcNode::with_denylist_config(AddressesDenylistConfig::new(
            Default::default(),
            Default::default(),
            Vec::new(),
        )))
        .launch_with_debug_capabilities()
        .await?;

    let sink = Arc::new(CaptureLog::default());
    let arc = ArcNode::with_denylist_config(AddressesDenylistConfig::new(
        Default::default(),
        Default::default(),
        Vec::new(),
    ));
    let writer = NodeBuilder::new(config)
        .testing_node(runtime.clone())
        .with_types::<ArcNode>()
        .with_components(arc.components_builder())
        .with_add_ons(
            arc.add_ons()
                .with_engine_validator(WriterValidatorBuilder::new(sink.clone())),
        );
    let (history_changed, _history_progress) = tokio::sync::watch::channel(());
    let launcher = WriterNodeLauncher::new(
        runtime,
        writer.config().datadir(),
        writer.config().engine.tree_config(),
        sink.clone(),
        "capture-test".to_string(),
        history_changed,
    );
    let writer = writer.launch_with(DebugNodeLauncher::new(launcher)).await?;

    let mut invalid = block.sealed_block().clone_block();
    invalid.header.state_root = B256::repeat_byte(0xa5);
    let invalid = SealedBlock::seal_slow(invalid);
    let invalid_payload =
        ExecutionData::from_block_unchecked(invalid.hash(), &invalid.clone_block());
    let plain_rejected = plain
        .node
        .beacon_engine_handle
        .new_payload(invalid_payload.clone())
        .await?;
    let writer_rejected = writer
        .node
        .beacon_engine_handle
        .new_payload(invalid_payload)
        .await?;
    assert!(!plain_rejected.is_valid());
    assert_eq!(plain_rejected, writer_rejected);
    assert!(sink.0.lock().unwrap().is_empty());

    let engine_input =
        ExecutionData::from_block_unchecked(block.hash(), &block.sealed_block().clone_block());
    let plain_status = plain
        .node
        .beacon_engine_handle
        .new_payload(engine_input.clone())
        .await?;
    let writer_status = writer
        .node
        .beacon_engine_handle
        .new_payload(engine_input.clone())
        .await?;
    let duplicate_status = writer
        .node
        .beacon_engine_handle
        .new_payload(engine_input)
        .await?;
    assert!(duplicate_status.is_valid());
    assert_eq!(plain_status, writer_status);
    assert!(writer_status.is_valid(), "{writer_status:?}");

    let parent_root = genesis.state_root();
    let history = WriterEvmConfig::history(
        plain_config.clone(),
        Arc::new(move |_| Ok(parent_root)),
        sink.clone(),
    );
    let state = || {
        plain
            .node
            .provider
            .state_by_block_hash(block.parent_hash())
            .map(StateProviderDatabase::new)
    };
    let expected = plain_config.batch_executor(state()?).execute(&block)?;
    let actual = history.batch_executor(state()?).execute(&block)?;
    assert_eq!(actual, expected);
    let parent_state = plain
        .node
        .provider
        .state_by_block_hash(block.parent_hash())?;
    assert_eq!(
        parent_state.state_root(parent_state.hashed_post_state(&actual.state))?,
        block.state_root(),
    );
    let records = sink.0.lock().unwrap();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].1, CaptureSource::LiveAccepted);
    assert_eq!(records[1].1, CaptureSource::HistoryAwaitingMerkle);
    for (captured, _) in records.iter() {
        equivalent(captured, &input["writer"])?;
    }
    println!(
        "H1 {} gas_used={} receipts={} state_root={}",
        block.hash(),
        actual.result.gas_used,
        actual.result.receipts.len(),
        block.state_root()
    );
    Ok(())
}

#[derive(Clone, Debug, serde::Deserialize)]
struct WitnessAccount {
    nonce: u64,
    balance: alloy_primitives::U256,
    code_hash: B256,
    code: Option<Bytes>,
    storage: std::collections::BTreeMap<B256, alloy_primitives::U256>,
}

#[derive(Clone, Debug, serde::Deserialize)]
struct WitnessDb(std::collections::BTreeMap<alloy_primitives::Address, Option<WitnessAccount>>);

#[derive(Debug, thiserror::Error)]
#[error("missing Arc parent witness input {0}")]
struct WitnessMiss(String);

impl revm::database_interface::DBErrorMarker for WitnessMiss {}

impl revm::Database for WitnessDb {
    type Error = WitnessMiss;

    fn basic(
        &mut self,
        address: alloy_primitives::Address,
    ) -> Result<Option<revm::state::AccountInfo>, Self::Error> {
        Ok(self
            .0
            .get(&address)
            .ok_or_else(|| WitnessMiss(address.to_string()))?
            .as_ref()
            .map(|account| revm::state::AccountInfo {
                nonce: account.nonce,
                balance: account.balance,
                code_hash: account.code_hash,
                code: account.code.clone().map(revm::state::Bytecode::new_raw),
                account_id: None,
            }))
    }

    fn code_by_hash(&mut self, hash: B256) -> Result<revm::state::Bytecode, Self::Error> {
        self.0
            .values()
            .flatten()
            .find(|account| account.code_hash == hash)
            .and_then(|account| account.code.clone())
            .map(revm::state::Bytecode::new_raw)
            .ok_or_else(|| WitnessMiss(format!("code {hash}")))
    }

    fn storage(
        &mut self,
        address: alloy_primitives::Address,
        key: alloy_primitives::U256,
    ) -> Result<alloy_primitives::U256, Self::Error> {
        let account = self
            .0
            .get(&address)
            .ok_or_else(|| WitnessMiss(address.to_string()))?;
        if let Some(account) = account {
            account
                .storage
                .get(&B256::from(key))
                .copied()
                .ok_or_else(|| WitnessMiss(format!("storage {address}/{key}")))
        } else {
            Ok(alloy_primitives::U256::ZERO)
        }
    }

    fn block_hash(&mut self, height: u64) -> Result<B256, Self::Error> {
        Err(WitnessMiss(format!("block hash {height}")))
    }
}

#[test]
#[ignore = "requires ARC_WRITER_FIXTURES with the account-deletion witness"]
fn deletion_block_execution_projection() -> eyre::Result<()> {
    let fixture = fixture(20_329_532)?;
    let rpc_block: alloy_rpc_types_eth::Block = serde_json::from_value(fixture["block"].clone())?;
    let block = RecoveredBlock::try_recover(rpc_block.into_consensus().convert_transactions())?;
    assert_eq!(
        block.hash(),
        serde_json::from_value::<B256>(fixture["block"]["hash"].clone())?
    );
    let parent_root = serde_json::from_value::<B256>(fixture["parent"]["stateRoot"].clone())?;
    let db: WitnessDb = serde_json::from_value(fixture["accounts"].clone())?;
    let spec = MAINNET.clone();
    let plain = ArcEvmConfig::new(EthEvmConfig::new_with_evm_factory(
        spec.clone(),
        ArcEvmFactory::new(spec),
    ));
    let sink = Arc::new(CaptureLog::default());
    let history = WriterEvmConfig::history(
        plain.clone(),
        Arc::new(move |_| Ok(parent_root)),
        sink.clone(),
    );
    let ordinary = plain.batch_executor(db.clone()).execute(&block)?;
    let captured = history.batch_executor(db).execute(&block)?;
    assert_eq!(captured, ordinary);
    assert_eq!(captured.result.receipts.len(), 1);
    assert_eq!(captured.result.gas_used, block.gas_used());
    let records = sink.0.lock().unwrap();
    assert_eq!(records.len(), 1);
    equivalent(&records[0].0, &fixture["writer"])?;
    Ok(())
}
