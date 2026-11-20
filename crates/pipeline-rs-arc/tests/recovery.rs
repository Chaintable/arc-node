use alloy_primitives::{Bytes, B256};
use alloy_rlp::{Encodable, Header};
use pipeline_rs_arc::{
    BlockRef, CaptureSink, CapturedArcBlock, ObjectKind, Spool, SpoolIdentity, Validation,
};

fn capture() -> CapturedArcBlock {
    let state_root = B256::repeat_byte(3);
    let parent_state_root = B256::repeat_byte(2);
    let mut payload = Vec::new();
    state_root.encode(&mut payload);
    parent_state_root.encode(&mut payload);
    payload.extend([0xc0; 4]);
    let mut diff = Vec::new();
    Header {
        list: true,
        payload_length: payload.len(),
    }
    .encode(&mut diff);
    diff.extend(payload);
    CapturedArcBlock {
        block: BlockRef {
            block_number: 11,
            hash: B256::repeat_byte(11),
            parent_hash: B256::repeat_byte(10),
            timestamp: 1234,
        },
        state_root,
        parent_state_root,
        block_file: serde_json::value::to_raw_value(&serde_json::json!({
            "block": {"height": 11, "process_start_timestamp": 5},
            "txs": [],
        }))
        .unwrap(),
        header: serde_json::value::to_raw_value(&serde_json::json!({
            "number": "0xb",
            "stateRoot": state_root,
        }))
        .unwrap(),
        state_diff_rlp: Bytes::from(diff),
        validation: Validation {
            validation_hash: 1,
            is_fork: false,
            txs_count: 0,
            events_count: 0,
            traces_count: 0,
            error_events_count: 0,
            error_traces_count: 0,
            storage_contracts_count: 0,
        },
    }
}

fn identity() -> SpoolIdentity {
    SpoolIdentity {
        chain_id: 5042,
        genesis_hash: B256::repeat_byte(9),
        version: "shadow".into(),
        source_id: "writer-test".into(),
        blockfile_bucket: "business".into(),
        nodex_bucket: "nodex".into(),
    }
}

#[test]
fn recovers_only_missing_objects_and_releases_raw_after_last_ack() {
    let root = tempfile::tempdir().unwrap();
    let block = capture();
    let hash = block.block.hash;
    let open = || Spool::open(root.path(), &identity(), 4096).unwrap();
    let spool = open();
    spool.seal(block.clone()).unwrap();
    assert!(spool.block(hash).unwrap().unwrap().upload_eligible());
    assert_eq!(
        spool.raw(hash).unwrap().state_diff_rlp,
        block.state_diff_rlp
    );
    spool.acknowledge(hash, ObjectKind::BlockFile).unwrap();
    spool.acknowledge(hash, ObjectKind::Header).unwrap();
    drop(spool);

    let spool = open();
    let row = spool.block(hash).unwrap().unwrap();
    assert!(row.upload_eligible());
    assert!(!row.remote_ready());
    assert_eq!(
        row.missing_objects().collect::<Vec<_>>(),
        [ObjectKind::Validation, ObjectKind::StateDiff]
    );
    spool.acknowledge(hash, ObjectKind::Validation).unwrap();
    assert!(spool.raw(hash).is_ok());
    spool.acknowledge(hash, ObjectKind::StateDiff).unwrap();
    drop(spool);

    let spool = open();
    assert!(spool.block(hash).unwrap().unwrap().remote_ready());
    assert_eq!(
        std::fs::read_dir(root.path().join("raw")).unwrap().count(),
        0
    );
    spool.seal(block).unwrap();
    assert!(spool.block(hash).unwrap().unwrap().upload_eligible());
    assert_eq!(
        std::fs::read_dir(root.path().join("raw")).unwrap().count(),
        0
    );
}

#[test]
fn quota_failure_does_not_register_incomplete_capture() {
    let root = tempfile::tempdir().unwrap();
    let block = capture();
    let hash = block.block.hash;
    let spool = Spool::open(root.path(), &identity(), 100).unwrap();
    assert!(spool.seal(block).is_err());
    assert!(spool.block(hash).unwrap().is_none());
    assert_eq!(
        std::fs::read_dir(root.path().join("raw")).unwrap().count(),
        0
    );
}

#[test]
fn old_unconfirmed_capture_is_held_until_replayed() {
    let root = tempfile::tempdir().unwrap();
    let block = capture();
    let hash = block.block.hash;
    let open = || Spool::open(root.path(), &identity(), 4096).unwrap();
    let spool = open();
    spool.seal(block.clone()).unwrap();
    drop(spool);
    let index = rusqlite::Connection::open(root.path().join("index.sqlite")).unwrap();
    index
        .execute_batch(
            "UPDATE identity SET schema_version = 1;
         UPDATE captures SET block = json_remove(json_set(
             block, '$.historical_capture', json('true'), '$.live_accepted', json('false'),
             '$.history_confirmed', json('false')), '$.eligible');",
        )
        .unwrap();
    drop(index);

    let spool = open();
    assert!(!spool.block(hash).unwrap().unwrap().upload_eligible());
    assert_eq!(spool.raw(hash).unwrap().block, block.block);
    spool.seal(block).unwrap();
    assert!(spool.block(hash).unwrap().unwrap().upload_eligible());
}
