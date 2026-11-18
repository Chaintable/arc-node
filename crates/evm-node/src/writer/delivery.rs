use std::{error::Error, future::Future, sync::Arc};

use alloy_consensus::Header;
use alloy_primitives::B256;
use arc_execution_config::chainspec::ArcChainSpec;
use pipeline_rs_arc::{
    execution::capture_genesis, BlockRef, CaptureSink, CapturedArcBlock, Delivery, DeliveryChain,
    DeliveryConfig, SpoolError,
};
use reth_chainspec::EthChainSpec;
use reth_node_api::{FullNodeComponents, NodeAddOns, NodeTypes};
use reth_node_builder::FullNode;
use reth_provider::{BlockIdReader, CanonStateSubscriptions, HeaderProvider};

type ReplayResult = Result<CapturedArcBlock, Box<dyn Error + Send + Sync>>;

pub fn start_delivery<N, A, F, R>(
    node: &FullNode<N, A>,
    spec: &ArcChainSpec,
    delivery: Arc<Delivery>,
    config: DeliveryConfig,
    replay: F,
) -> eyre::Result<()>
where
    N: FullNodeComponents<Types: NodeTypes<ChainSpec = ArcChainSpec>>,
    A: NodeAddOns<N>,
    N::Provider: HeaderProvider<Header = Header>,
    F: Fn(B256) -> R + Send + Sync + 'static,
    R: Future<Output = ReplayResult> + Send,
{
    let header = spec.genesis_header();
    let genesis = reference(header, spec.genesis_hash());
    if config.initialize_empty_topic {
        delivery.seal(capture_genesis(spec.genesis(), header, genesis.hash)?)?;
    }
    let chain = ArcChain {
        provider: node.provider.clone(),
        replay,
    };
    let mut canonical_events = node.provider.subscribe_to_canonical_state();
    let wake = delivery.clone();
    node.task_executor.spawn_task(async move {
        loop {
            match canonical_events.recv().await {
                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    wake.canonical_changed();
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });
    node.task_executor
        .spawn_critical_task("arc-writer-delivery", async move {
            if let Err(error) = delivery.run(config, chain, genesis).await {
                panic!("Arc Writer delivery failed: {error}");
            }
        });
    Ok(())
}

struct ArcChain<P, F> {
    provider: P,
    replay: F,
}

impl<P, F, R> DeliveryChain for ArcChain<P, F>
where
    P: BlockIdReader + HeaderProvider<Header = Header>,
    F: Fn(B256) -> R + Send + Sync,
    R: Future<Output = ReplayResult> + Send,
{
    fn finalized_height(&self) -> Result<Option<u64>, SpoolError> {
        let Some(finalized) = self
            .provider
            .finalized_block_num_hash()
            .map_err(projection)?
        else {
            return Ok(None);
        };
        Ok(self
            .canonical_block(finalized.number)?
            .filter(|block| block.hash == finalized.hash)
            .map(|block| block.block_number))
    }

    fn replay(&self, hash: B256) -> impl Future<Output = ReplayResult> + Send {
        (self.replay)(hash)
    }

    fn canonical_tip(&self) -> Result<BlockRef, SpoolError> {
        let head = self.provider.chain_info().map_err(projection)?;
        self.known_block(head.best_hash)?
            .ok_or_else(|| projection(format!("missing Arc canonical tip {}", head.best_hash)))
    }

    fn canonical_block(&self, height: u64) -> Result<Option<BlockRef>, SpoolError> {
        Ok(self
            .provider
            .sealed_header(height)
            .map_err(projection)?
            .map(|header| reference(header.header(), header.hash())))
    }

    fn known_block(&self, hash: B256) -> Result<Option<BlockRef>, SpoolError> {
        Ok(self
            .provider
            .header(hash)
            .map_err(projection)?
            .map(|header| reference(&header, hash)))
    }
}

fn reference(header: &Header, hash: B256) -> BlockRef {
    BlockRef {
        block_number: header.number,
        hash,
        parent_hash: header.parent_hash,
        timestamp: header.timestamp,
    }
}

fn projection(error: impl ToString) -> SpoolError {
    SpoolError::Projection(error.to_string())
}
