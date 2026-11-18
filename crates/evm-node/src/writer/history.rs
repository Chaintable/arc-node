//! Confirms the Arc consensus branch for already verified historical execution.

use alloy_consensus::Header;
use alloy_primitives::B256;
use pipeline_rs_arc::BlockRef;
use reqwest::{Client, StatusCode, Url};
use reth_provider::{HeaderProvider, StageCheckpointReader};
use reth_stages::StageId;
use serde::Deserialize;

use super::merkle::VerifiedRange;

/// The two fields needed from the commit certificate of a trusted Arc CL.
#[derive(Clone, Copy, Debug, Deserialize)]
pub struct DecidedBlock {
    pub height: u64,
    pub block_hash: B256,
}

pub async fn read_decided_block(
    client: &Client,
    consensus_url: &Url,
    height: u64,
) -> eyre::Result<Option<DecidedBlock>> {
    let mut url = consensus_url.clone();
    url.path_segments_mut()
        .map_err(|_| eyre::eyre!("invalid Arc Writer consensus base URL"))?
        .pop_if_empty()
        .push("commit");
    url.set_query(None);
    let response = client.get(url).query(&[("height", height)]).send().await?;
    if response.status() == StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let block: DecidedBlock = response.error_for_status()?.json().await?;
    eyre::ensure!(
        block.height == height,
        "Arc commit height mismatch: requested {height}, received {}",
        block.height
    );
    Ok(Some(block))
}

/// Confirms the whole Merkle range with one decided execution block and its
/// exact ancestors. The Finish checkpoint makes restart after the final
/// pipeline commit independent of delivery of the backfill notification.
pub fn confirmed_range<P>(
    provider: &P,
    range: &VerifiedRange,
    certificate: DecidedBlock,
    published: Option<BlockRef>,
) -> eyre::Result<Vec<BlockRef>>
where
    P: HeaderProvider<Header = Header> + StageCheckpointReader,
{
    let completed = provider
        .get_stage_checkpoint(StageId::Finish)?
        .map_or(0, |checkpoint| checkpoint.block_number);
    if range.start == 0 || range.end < range.start || range.end > completed {
        eyre::bail!("Arc Merkle range has no completed backfill");
    }
    if certificate.height < range.end {
        eyre::bail!("Arc certificate predates the end of the Merkle range");
    }

    let mut hash = certificate.block_hash;
    let mut blocks = Vec::new();
    let check_from = match published {
        Some(block) if block.block_number < range.start => block.block_number,
        _ => range.start,
    };
    let mut published_seen = false;
    for height in (check_from..=certificate.height).rev() {
        let header = provider
            .header(hash)?
            .ok_or_else(|| eyre::eyre!("missing Arc certificate ancestor at {height} {hash}"))?;
        eyre::ensure!(
            header.number == height && header.hash_slow() == hash,
            "invalid Arc certificate ancestor at {height} {hash}"
        );
        if height == range.end {
            eyre::ensure!(
                hash == range.end_hash && header.state_root == range.end_state_root,
                "Arc certificate does not confirm the Merkle endpoint at {height}"
            );
        }
        if height == range.start {
            eyre::ensure!(
                header.parent_hash == range.start_parent,
                "Arc certificate does not confirm the Merkle range parent"
            );
        }
        if let Some(tail) = published
            && height == tail.block_number
        {
            eyre::ensure!(
                hash == tail.hash,
                "Arc certificate ancestry disagrees with the published block at {height}"
            );
            published_seen = true;
        }
        if height >= range.start && height <= range.end {
            blocks.push(BlockRef {
                hash,
                parent_hash: header.parent_hash,
                block_number: height,
                timestamp: header.timestamp,
            });
        }
        hash = header.parent_hash;
    }
    if published.is_some_and(|tail| tail.block_number <= certificate.height) {
        eyre::ensure!(
            published_seen,
            "published Arc block had no confirmed ancestry"
        );
    }
    blocks.reverse();
    Ok(blocks)
}
