use std::{
    collections::HashMap, error::Error, future::Future, num::NonZeroUsize, path::PathBuf,
    sync::Arc, time::Duration,
};

use alloy_primitives::B256;
use rdkafka::ClientConfig;
use serde::Deserialize;
use tokio::sync::Notify;

use crate::{
    upload_block, BlockRef, CaptureSink, CapturedArcBlock, KafkaStream, Notice, ObjectStore,
    PublicationView, S3ObjectStore, Spool, SpoolError, SpoolIdentity, UploadError,
};

use crate::notice::publication_step;

type Failure = Box<dyn Error + Send + Sync>;

const WINDOW: NonZeroUsize = NonZeroUsize::new(256).unwrap();
const WAIT: Duration = Duration::from_secs(1);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeliveryConfig {
    pub spool: PathBuf,
    pub spool_bytes: u64,
    pub version: String,
    pub blockfile_bucket: String,
    pub nodex_bucket: String,
    pub kafka_topic: String,
    pub kafka_partition: i32,
    pub kafka: HashMap<String, String>,
    #[serde(default)]
    pub initialize_empty_topic: bool,
    pub s3_endpoint: Option<String>,
}

/// Chain facts and block replay for the single delivery worker.
pub trait DeliveryChain: Send + Sync {
    fn finalized_height(&self) -> Result<Option<u64>, SpoolError>;
    fn replay(&self, hash: B256) -> impl Future<Output = Result<CapturedArcBlock, Failure>> + Send;
    fn canonical_tip(&self) -> Result<BlockRef, SpoolError>;
    fn canonical_block(&self, height: u64) -> Result<Option<BlockRef>, SpoolError>;
    fn known_block(&self, hash: B256) -> Result<Option<BlockRef>, SpoolError>;
}

pub struct Delivery {
    spool: Spool,
    wake: Notify,
}

impl Delivery {
    pub fn open(
        config: &DeliveryConfig,
        chain_id: u64,
        genesis_hash: B256,
        source_id: String,
    ) -> Result<Arc<Self>, SpoolError> {
        let identity = SpoolIdentity {
            chain_id,
            genesis_hash,
            version: config.version.clone(),
            source_id,
            blockfile_bucket: config.blockfile_bucket.clone(),
            nodex_bucket: config.nodex_bucket.clone(),
        };
        Ok(Arc::new(Self {
            spool: Spool::open(&config.spool, &identity, config.spool_bytes)?,
            wake: Notify::new(),
        }))
    }

    /// Wake the worker after the node updates its canonical branch.
    pub fn canonical_changed(&self) {
        self.wake.notify_one();
    }

    pub async fn run(
        self: Arc<Self>,
        config: DeliveryConfig,
        chain: impl DeliveryChain,
        genesis: BlockRef,
    ) -> Result<(), Failure> {
        let mut kafka_config = ClientConfig::new();
        for (key, value) in config.kafka {
            kafka_config.set(key, value);
        }
        let initialized = self
            .spool
            .publication_initialized(&config.kafka_topic, config.kafka_partition)?
            || !config.initialize_empty_topic;
        let kafka = KafkaStream::new(
            &kafka_config,
            config.kafka_topic,
            config.kafka_partition,
            Duration::from_secs(30),
            1_000_000,
        )?;
        let shared = aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await;
        let mut s3 = aws_sdk_s3::config::Builder::from(&shared);
        if let Some(endpoint) = config.s3_endpoint {
            s3 = s3.endpoint_url(endpoint).force_path_style(true);
        }
        let store = S3ObjectStore(aws_sdk_s3::Client::from_conf(s3.build()));

        self.run_connected(kafka, initialized, &store, &chain, genesis)
            .await
    }

    async fn run_connected(
        &self,
        mut kafka: KafkaStream,
        initialized: bool,
        store: &impl ObjectStore,
        chain: &impl DeliveryChain,
        genesis: BlockRef,
    ) -> Result<(), Failure> {
        let (mut kafka, tail) = tokio::task::spawn_blocking(move || {
            let tail = kafka.recover(initialized)?;
            Ok::<_, crate::KafkaStreamError>((kafka, tail))
        })
        .await??;
        let mut tail = tail.map(|tail| tail.block);
        if tail.is_some() {
            self.spool.mark_publication_initialized()?;
        }

        loop {
            let (uploaded, retry_upload) = self.upload_pending(store).await?;
            let (notice, missing) = if tail.is_some() {
                self.canonical_step(chain, tail)?
            } else {
                let notice = self
                    .spool
                    .block(genesis.hash)?
                    .filter(|row| row.remote_ready())
                    .map(|_| Notice {
                        change_type: 1,
                        new_blocks: vec![genesis],
                        drop_blocks: None,
                    });
                (notice, None)
            };

            if let Some(notice) = notice {
                // On a send error the task exits. Restart recovers the actual broker tail.
                tail = Some(kafka.send(&notice, None).await?.block);
                self.spool.mark_publication_initialized()?;
                continue;
            }
            if let Some(block) = missing
                && !self.has_capture(block.hash)?
                && chain
                    .finalized_height()?
                    .is_some_and(|height| block.block_number <= height)
                && chain.canonical_block(block.block_number)? == Some(block)
            {
                match chain.replay(block.hash).await {
                    Ok(capture) => {
                        if capture.block != block {
                            return Err(SpoolError::Projection(format!(
                                "replay returned a different block for {}",
                                block.hash
                            ))
                            .into());
                        }
                        if chain.canonical_block(block.block_number)? == Some(block)
                            && !self.has_capture(block.hash)?
                        {
                            self.seal(capture)?;
                        }
                        continue;
                    }
                    Err(error) => {
                        tracing::warn!(%error, hash = %block.hash, height = block.block_number,
                        "Arc Writer could not replay the publication gap")
                    }
                }
            }
            if uploaded && !retry_upload {
                continue;
            }
            if retry_upload {
                tokio::time::sleep(WAIT).await;
            } else {
                // Native backfill can move the tracked head without a canonical notification.
                let _ = tokio::time::timeout(WAIT, self.wake.notified()).await;
            }
        }
    }

    fn has_capture(&self, hash: B256) -> Result<bool, SpoolError> {
        Ok(self
            .spool
            .block(hash)?
            .is_some_and(|row| row.upload_eligible()))
    }

    async fn upload_pending(&self, store: &impl ObjectStore) -> Result<(bool, bool), Failure> {
        let hashes = self.spool.pending_uploads(WINDOW.get() as u32)?;
        let mut uploaded = false;
        let mut retry = false;
        for hash in hashes {
            match upload_block(&self.spool, store, hash).await {
                Ok(ready) => uploaded |= ready,
                Err(error @ (UploadError::Remote(_) | UploadError::Retry(_))) => {
                    tracing::warn!(%error, %hash, "Arc Writer upload will retry");
                    retry = true;
                    // Preserve the first missing block's place in this batch.
                    break;
                }
                Err(error) => return Err(error.into()),
            }
        }
        Ok((uploaded, retry))
    }

    fn canonical_step(
        &self,
        chain: &impl DeliveryChain,
        tail: Option<BlockRef>,
    ) -> Result<(Option<Notice>, Option<BlockRef>), Failure> {
        let tip = chain.canonical_tip()?;
        let view = LiveView {
            chain,
            spool: &self.spool,
            tip,
        };
        let step = publication_step(&view, tail, false, WINDOW);
        // Read each candidate again before sending. The initial reads can span a
        // canonical update, including a branch that was replaced and restored.
        if let Ok((Some(notice), _)) = &step {
            for block in &notice.new_blocks {
                if chain.canonical_block(block.block_number)? != Some(*block) {
                    return Ok((None, None));
                }
            }
        }
        if chain.canonical_tip()? != tip {
            return Ok((None, None));
        }
        Ok(step?)
    }
}

impl CaptureSink for Delivery {
    fn seal(&self, block: CapturedArcBlock) -> Result<(), SpoolError> {
        self.spool.seal(block)?;
        self.wake.notify_one();
        Ok(())
    }
}

struct LiveView<'a, C> {
    chain: &'a C,
    spool: &'a Spool,
    tip: BlockRef,
}

impl<C: DeliveryChain> PublicationView for LiveView<'_, C> {
    fn selected_tip(&self) -> BlockRef {
        self.tip
    }

    fn selected_at(&self, height: u64) -> Result<Option<BlockRef>, SpoolError> {
        self.chain.canonical_block(height)
    }

    fn published_by_hash(&self, hash: B256) -> Result<Option<BlockRef>, SpoolError> {
        if let Some(block) = self.spool.block(hash)? {
            return Ok(Some(block.block));
        }
        self.chain.known_block(hash)
    }

    fn remote_ready(&self, hash: B256) -> Result<bool, SpoolError> {
        Ok(self
            .spool
            .block(hash)?
            .is_some_and(|row| row.remote_ready()))
    }
}

#[cfg(test)]
mod tests;
