use std::{io::Write, num::NonZeroUsize};

use alloy_primitives::B256;
use flate2::{write::GzEncoder, Compression};
use serde::{Deserialize, Serialize};

use crate::{BlockRef, SpoolError};

pub const NOTIFICATION_KEY: &[u8] = b"NewBlock";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Notice {
    pub change_type: u8,
    pub new_blocks: Vec<BlockRef>,
    pub drop_blocks: Option<Vec<BlockRef>>,
}

impl Notice {
    pub fn encode(&self, output: impl Write) -> Result<(), SpoolError> {
        let mut writer = GzEncoder::new(output, Compression::default());
        serde_json::to_writer(&mut writer, self)?;
        writer.finish()?;
        Ok(())
    }
}

/// One consistent view of the eligible historical and live branches. A
/// historical view is anchored by a decided Arc block, and a live view by
/// Reth's current canonical choice.
pub trait PublicationView {
    fn selected_tip(&self) -> BlockRef;
    fn selected_at(&self, height: u64) -> Result<Option<BlockRef>, SpoolError>;
    fn published_by_hash(&self, hash: B256) -> Result<Option<BlockRef>, SpoolError>;
    fn remote_ready(&self, hash: B256) -> Result<bool, SpoolError>;
}

#[derive(Debug, thiserror::Error)]
pub enum NoticeError {
    #[error(transparent)]
    Storage(#[from] SpoolError),
    #[error("missing selected block at height {0}")]
    MissingSelected(u64),
    #[error("missing previously published parent {0:#x}")]
    MissingPublished(B256),
    #[error("disconnected publication branch at {0:#x}")]
    Disconnected(B256),
    #[error("first reorg notification requires more than {0} block references")]
    ReorgTooLarge(usize),
}

/// Calculate one notification from the broker's authoritative tail. The
/// publisher must recheck the selected view immediately before transmission.
pub fn next_notice(
    view: &impl PublicationView,
    published: Option<BlockRef>,
    initialize: bool,
    limit: NonZeroUsize,
) -> Result<Option<Notice>, NoticeError> {
    Ok(publication_step(view, published, initialize, limit)?.0)
}

pub(crate) fn publication_step(
    view: &impl PublicationView,
    published: Option<BlockRef>,
    initialize: bool,
    limit: NonZeroUsize,
) -> Result<(Option<Notice>, Option<BlockRef>), NoticeError> {
    let max_blocks = limit.get();
    let tip = view.selected_tip();
    let Some(published) = published else {
        if !initialize {
            return Ok((None, None));
        }
        let genesis = selected(view, 0)?;
        return Ok((
            view.remote_ready(genesis.hash)?.then(|| Notice {
                change_type: 1,
                new_blocks: vec![genesis],
                drop_blocks: None,
            }),
            None,
        ));
    };
    if tip.block_number <= published.block_number {
        return Ok((None, None));
    }

    let mut ancestor = published;
    let mut drops = Vec::new();
    while selected(view, ancestor.block_number)?.hash != ancestor.hash {
        if ancestor.block_number == 0 {
            return Err(NoticeError::Disconnected(ancestor.hash));
        }
        if drops.len() == max_blocks {
            return Err(NoticeError::ReorgTooLarge(max_blocks));
        }
        drops.push(ancestor);
        let parent = view
            .published_by_hash(ancestor.parent_hash)?
            .ok_or(NoticeError::MissingPublished(ancestor.parent_hash))?;
        if !child_of(ancestor, parent) {
            return Err(NoticeError::Disconnected(ancestor.hash));
        }
        ancestor = parent;
    }

    let available = max_blocks - drops.len();
    if !drops.is_empty() && published.block_number - ancestor.block_number >= available as u64 {
        return Err(NoticeError::ReorgTooLarge(max_blocks));
    }

    let mut next = Vec::new();
    let mut parent = ancestor;
    let mut missing = None;
    for height in ancestor.block_number + 1..=tip.block_number {
        let block = selected(view, height)?;
        if !child_of(block, parent) {
            return Err(NoticeError::Disconnected(block.hash));
        }
        if !view.remote_ready(block.hash)? {
            missing = Some(block);
            break;
        }
        next.push(block);
        if next.len() == available {
            break;
        }
        parent = block;
    }

    if next
        .last()
        .is_none_or(|head| head.block_number <= published.block_number)
    {
        return Ok((None, missing));
    }
    drops.reverse();
    Ok((
        Some(Notice {
            change_type: if drops.is_empty() { 1 } else { 2 },
            new_blocks: next,
            drop_blocks: (!drops.is_empty()).then_some(drops),
        }),
        missing,
    ))
}

fn selected(view: &impl PublicationView, height: u64) -> Result<BlockRef, NoticeError> {
    view.selected_at(height)?
        .filter(|block| block.block_number == height)
        .ok_or(NoticeError::MissingSelected(height))
}

fn child_of(child: BlockRef, parent: BlockRef) -> bool {
    child.parent_hash == parent.hash
        && child.block_number.checked_sub(1) == Some(parent.block_number)
}
