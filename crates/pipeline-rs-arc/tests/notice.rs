use std::{
    collections::{BTreeMap, BTreeSet},
    num::NonZeroUsize,
};

use alloy_primitives::B256;
use pipeline_rs_arc::{next_notice, BlockRef, PublicationView, SpoolError};

fn block(height: u64, id: u8, parent: u8) -> BlockRef {
    BlockRef {
        block_number: height,
        hash: B256::repeat_byte(id),
        parent_hash: B256::repeat_byte(parent),
        timestamp: height,
    }
}

struct View {
    tip: BlockRef,
    selected: BTreeMap<u64, BlockRef>,
    old: BTreeMap<B256, BlockRef>,
    ready: BTreeSet<B256>,
}

impl PublicationView for View {
    fn selected_tip(&self) -> BlockRef {
        self.tip
    }
    fn selected_at(&self, height: u64) -> Result<Option<BlockRef>, SpoolError> {
        Ok(self.selected.get(&height).copied())
    }
    fn published_by_hash(&self, hash: B256) -> Result<Option<BlockRef>, SpoolError> {
        Ok(self.old.get(&hash).copied())
    }
    fn remote_ready(&self, hash: B256) -> Result<bool, SpoolError> {
        Ok(self.ready.contains(&hash))
    }
}

#[test]
fn does_not_skip_the_first_missing_object() {
    let p = block(118, 118, 117);
    let a = block(119, 119, 118);
    let b = block(120, 120, 119);
    let mut view = View {
        tip: b,
        selected: [(118, p), (119, a), (120, b)].into(),
        old: BTreeMap::new(),
        ready: [b.hash].into(),
    };
    let limit = NonZeroUsize::new(8).unwrap();
    assert!(next_notice(&view, Some(p), false, limit).unwrap().is_none());
    view.ready.insert(a.hash);
    let notice = next_notice(&view, Some(p), false, limit).unwrap().unwrap();
    assert_eq!(notice.new_blocks, [a, b]);
    assert_eq!(notice.drop_blocks, None);
}

#[test]
fn same_height_reorg_waits_for_ready_higher_tip() {
    let ancestor = block(99, 99, 98);
    let old = block(100, 100, 99);
    let replacement = block(100, 200, 99);
    let higher = block(101, 201, 200);
    let mut view = View {
        tip: replacement,
        selected: [(99, ancestor), (100, replacement)].into(),
        old: [(ancestor.hash, ancestor)].into(),
        ready: [replacement.hash].into(),
    };
    let limit = NonZeroUsize::new(8).unwrap();
    assert!(next_notice(&view, Some(old), false, limit)
        .unwrap()
        .is_none());
    view.tip = higher;
    view.selected.insert(101, higher);
    assert!(next_notice(&view, Some(old), false, limit)
        .unwrap()
        .is_none());
    view.ready.insert(higher.hash);
    let notice = next_notice(&view, Some(old), false, limit)
        .unwrap()
        .unwrap();
    assert_eq!(notice.change_type, 2);
    assert_eq!(notice.new_blocks, [replacement, higher]);
    assert_eq!(notice.drop_blocks, Some(vec![old]));
}
