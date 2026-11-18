use std::{error::Error, sync::Arc, time::Duration};

use alloy_consensus::Header;
use alloy_primitives::B256;
use arc_execution_config::chainspec::ArcChainSpec;
use pipeline_rs_arc::{
    execution::capture_genesis, BlockRef, CaptureSink, CaptureSource, Delivery, DeliveryChain,
    DeliveryConfig, History, SpoolError,
};
use reqwest::{Client, Url};
use reth_chainspec::EthChainSpec;
use reth_node_api::{FullNodeComponents, NodeAddOns, NodeTypes};
use reth_node_builder::FullNode;
use reth_provider::{
    BlockNumReader, CanonStateSubscriptions, DatabaseProviderROFactory, HeaderProvider,
    StageCheckpointReader,
};
use reth_stages_types::StageId;

use super::history::{confirmed_range, read_decided_block};

pub fn start_delivery<N, A>(
    node: &FullNode<N, A>,
    spec: &ArcChainSpec,
    delivery: Arc<Delivery>,
    config: DeliveryConfig,
    consensus_url: Url,
) -> eyre::Result<()>
where
    N: FullNodeComponents<Types: NodeTypes<ChainSpec = ArcChainSpec>>,
    A: NodeAddOns<N>,
    N::Provider: HeaderProvider<Header = Header>
        + DatabaseProviderROFactory<Provider: HeaderProvider<Header = Header> + StageCheckpointReader>,
{
    let header = spec.genesis_header();
    let genesis = reference(header, spec.genesis_hash());
    if config.initialize_empty_topic {
        delivery.seal(
            capture_genesis(spec.genesis(), header, genesis.hash)?,
            CaptureSource::LiveAccepted,
        )?;
    }
    let chain = ArcChain {
        provider: node.provider.clone(),
        consensus_url,
        client: Client::builder().timeout(Duration::from_secs(10)).build()?,
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

struct ArcChain<P> {
    provider: P,
    client: Client,
    consensus_url: Url,
}

impl<P> DeliveryChain for ArcChain<P>
where
    P: BlockNumReader
        + HeaderProvider<Header = Header>
        + DatabaseProviderROFactory<Provider: HeaderProvider<Header = Header> + StageCheckpointReader>
        + Clone
        + Send
        + Sync
        + 'static,
{
    async fn history(
        &self,
        tail: BlockRef,
        captured_through: u64,
    ) -> Result<History, Box<dyn Error + Send + Sync>> {
        let finished = {
            let view = self.provider.database_provider_ro()?;
            view.get_stage_checkpoint(StageId::Finish)?
                .map_or(0, |checkpoint| checkpoint.block_number)
        };
        let end = captured_through.min(finished);
        if end <= tail.block_number {
            return Ok(History::Pending);
        }
        // Use the committed end as the trust anchor, which may be newer than the
        // last captured block on a short range after resuming the node.
        let certificate =
            match read_decided_block(&self.client, &self.consensus_url, Some(finished)).await? {
                Some(certificate) => certificate,
                None => {
                    let Some(certificate) =
                        read_decided_block(&self.client, &self.consensus_url, None).await?
                    else {
                        return Ok(History::Pending);
                    };
                    if certificate.height < end {
                        return Ok(History::Pending);
                    }
                    let view = self.provider.database_provider_ro()?;
                    if view.sealed_header(certificate.height)?.is_none() {
                        return Ok(History::Pending);
                    }
                    certificate
                }
            };
        let provider = self.provider.clone();
        let confirmed =
            tokio::task::spawn_blocking(move || confirmed_range(&provider, tail, end, certificate))
                .await??;
        Ok(match confirmed {
            Some(blocks) => History::Ready(blocks),
            None => History::Pending,
        })
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
