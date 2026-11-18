//! Confirms the Arc consensus branch for already verified historical execution.

use alloy_consensus::Header;
use alloy_primitives::B256;
use pipeline_rs_arc::BlockRef;
use reqwest::{Client, StatusCode, Url};
use reth_provider::{DatabaseProviderROFactory, HeaderProvider, StageCheckpointReader};
use reth_stages_types::StageId;
use serde::Deserialize;

/// The two fields needed from the commit certificate of a trusted Arc CL.
#[derive(Clone, Copy, Debug, Deserialize)]
pub struct DecidedBlock {
    pub height: u64,
    pub block_hash: B256,
}

pub async fn read_decided_block(
    client: &Client,
    consensus_url: &Url,
    height: Option<u64>,
) -> eyre::Result<Option<DecidedBlock>> {
    let mut url = consensus_url.clone();
    url.path_segments_mut()
        .map_err(|_| eyre::eyre!("invalid Arc Writer consensus base URL"))?
        .pop_if_empty()
        .push("commit");
    url.set_query(None);
    let mut request = client.get(url);
    if let Some(height) = height {
        request = request.query(&[("height", height)]);
    }
    let response = request.send().await?;
    if response.status() == StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let block: DecidedBlock = response.error_for_status()?.json().await?;
    if let Some(height) = height {
        eyre::ensure!(
            block.height == height,
            "Arc commit height mismatch: requested {height}, received {}",
            block.height
        );
    }
    Ok(Some(block))
}

/// Checks a pending history range against the original database's committed
/// checkpoint and the ancestry of a trusted Arc certificate.
///
/// A pending result can be retried while the caller has work. The checkpoint
/// and canonical headers come from one database read view. The caller must
/// still associate the returned hashes with captures from this database.
pub fn confirmed_range<P>(
    provider: &P,
    published: BlockRef,
    end: u64,
    certificate: DecidedBlock,
) -> eyre::Result<Option<Vec<BlockRef>>>
where
    P: DatabaseProviderROFactory<Provider: HeaderProvider<Header = Header> + StageCheckpointReader>,
{
    if end <= published.block_number || certificate.height < end {
        return Ok(None);
    }

    let view = provider.database_provider_ro()?;
    let completed = view
        .get_stage_checkpoint(StageId::Finish)?
        .map_or(0, |checkpoint| checkpoint.block_number);
    if completed < end {
        return Ok(None);
    }

    let mut hash = certificate.block_hash;
    let mut blocks = Vec::new();
    for height in (published.block_number..=certificate.height).rev() {
        let header = view
            .sealed_header(height)?
            .ok_or_else(|| eyre::eyre!("missing persisted Arc header at {height}"))?;
        eyre::ensure!(
            header.number == height && header.hash() == hash,
            "Arc certificate ancestry disagrees with the persisted chain at {height}: expected {hash}, got {}",
            header.hash()
        );
        if height == published.block_number {
            eyre::ensure!(
                hash == published.hash,
                "Arc certificate ancestry disagrees with Kafka tail at {height}: expected {}, got {hash}",
                published.hash
            );
        } else if height <= end {
            blocks.push(BlockRef {
                hash,
                parent_hash: header.parent_hash,
                block_number: height,
                timestamp: header.timestamp,
            });
        }
        hash = header.parent_hash;
    }
    blocks.reverse();
    Ok(Some(blocks))
}
