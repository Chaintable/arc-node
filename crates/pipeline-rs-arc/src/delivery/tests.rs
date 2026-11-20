use std::{
    convert::Infallible,
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex,
    },
};

use alloy_primitives::{Bytes, U256};
use alloy_rlp::{Encodable, Header};
use rdkafka::mocking::MockCluster;

use super::*;
use crate::{ConditionalPut, KafkaTail, ObjectSpec, Validation};

struct Chain {
    finalized: AtomicU64,
    head: AtomicU64,
    replays: Mutex<Vec<u64>>,
}

impl DeliveryChain for Chain {
    fn finalized_height(&self) -> Result<Option<u64>, SpoolError> {
        Ok(Some(self.finalized.load(Ordering::Relaxed)))
    }

    async fn replay(&self, hash: B256) -> Result<CapturedArcBlock, Failure> {
        let height = u64::from_be_bytes(hash[24..].try_into().unwrap());
        self.replays.lock().unwrap().push(height);
        Ok(capture(height))
    }

    fn canonical_tip(&self) -> Result<BlockRef, SpoolError> {
        Ok(block(self.head.load(Ordering::Relaxed)))
    }

    fn canonical_block(&self, height: u64) -> Result<Option<BlockRef>, SpoolError> {
        Ok((height <= self.head.load(Ordering::Relaxed)).then(|| block(height)))
    }

    fn known_block(&self, _: B256) -> Result<Option<BlockRef>, SpoolError> {
        Ok(None)
    }
}

struct Objects;

impl ObjectStore for Objects {
    type Error = Infallible;

    async fn create_if_absent(
        &self,
        _: &ObjectSpec<'_>,
        _: &[u8],
    ) -> Result<ConditionalPut, Infallible> {
        Ok(ConditionalPut::Created)
    }

    async fn read(&self, _: &ObjectSpec<'_>) -> Result<Vec<u8>, Infallible> {
        unreachable!("all objects in this test are created")
    }
}

fn block(height: u64) -> BlockRef {
    BlockRef {
        block_number: height,
        hash: B256::from(U256::from(height)),
        parent_hash: B256::from(U256::from(height.saturating_sub(1))),
        timestamp: height,
    }
}

fn capture(height: u64) -> CapturedArcBlock {
    let root = B256::repeat_byte(1);
    let mut payload = Vec::new();
    root.encode(&mut payload);
    root.encode(&mut payload);
    payload.extend([0xc0; 4]);
    let mut diff = Vec::new();
    Header {
        list: true,
        payload_length: payload.len(),
    }
    .encode(&mut diff);
    diff.extend(payload);
    CapturedArcBlock {
        block: block(height),
        block_file: serde_json::value::to_raw_value(&serde_json::json!({
            "block": {"height": height, "process_start_timestamp": 1},
        }))
        .unwrap(),
        header: serde_json::value::to_raw_value(&serde_json::json!({
            "number": format!("{height:#x}"),
        }))
        .unwrap(),
        state_root: root,
        parent_state_root: root,
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

async fn broker_tail(config: ClientConfig) -> KafkaTail {
    tokio::task::spawn_blocking(move || kafka(&config).recover(true).unwrap().unwrap())
        .await
        .unwrap()
}

fn kafka(config: &ClientConfig) -> KafkaStream {
    KafkaStream::new(
        config,
        "writer".into(),
        0,
        Duration::from_secs(5),
        1_000_000,
    )
    .unwrap()
}

async fn wait_for_head(config: &ClientConfig, height: u64) -> KafkaTail {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let tail = broker_tail(config.clone()).await;
            if tail.block.block_number >= height {
                return tail;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn replays_only_finalized_gaps_and_restart_reads_broker_tail() {
    let broker = MockCluster::new(1).unwrap();
    broker.create_topic("writer", 1, 1).unwrap();
    let mut kafka_config = ClientConfig::new();
    kafka_config.set("bootstrap.servers", broker.bootstrap_servers());
    let mut old_writer = kafka(&kafka_config);
    assert!(old_writer.recover(false).unwrap().is_none());
    old_writer
        .send(
            &Notice {
                change_type: 1,
                new_blocks: vec![block(10)],
                drop_blocks: None,
            },
            None,
        )
        .await
        .unwrap();
    drop(old_writer);

    let root = tempfile::tempdir().unwrap();
    let config = DeliveryConfig {
        spool: root.path().into(),
        spool_bytes: 1_000_000,
        version: "test".into(),
        blockfile_bucket: "artifacts".into(),
        nodex_bucket: "state".into(),
        kafka_topic: "writer".into(),
        kafka_partition: 0,
        kafka: HashMap::new(),
        initialize_empty_topic: false,
        s3_endpoint: None,
    };
    let open = || Delivery::open(&config, 5042, B256::ZERO, "test-db".into()).unwrap();
    let delivery = open();
    delivery.spool.publication_initialized("writer", 0).unwrap();
    delivery.seal(capture(12)).unwrap();
    delivery.seal(capture(13)).unwrap();
    let chain = Arc::new(Chain {
        finalized: AtomicU64::new(10),
        head: AtomicU64::new(12),
        replays: Mutex::new(Vec::new()),
    });
    let start = |delivery: Arc<Delivery>, chain: Arc<Chain>| {
        let kafka = kafka(&kafka_config);
        tokio::spawn(async move {
            delivery
                .run_connected(kafka, true, &Objects, chain.as_ref(), block(0))
                .await
                .unwrap()
        })
    };
    let task = start(delivery.clone(), chain.clone());
    tokio::time::timeout(Duration::from_secs(10), async {
        while !delivery
            .spool
            .block(block(13).hash)
            .unwrap()
            .unwrap()
            .remote_ready()
        {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(broker_tail(kafka_config.clone()).await.block, block(10));
    assert!(chain.replays.lock().unwrap().is_empty());

    chain.finalized.store(12, Ordering::Relaxed);
    delivery.canonical_changed();
    let historical = wait_for_head(&kafka_config, 12).await;
    assert_eq!(historical.block, block(12));
    assert_eq!(historical.offset, 1);
    assert_eq!(*chain.replays.lock().unwrap(), [11]);
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    drop(delivery);

    let restarted = open();
    assert!(restarted
        .spool
        .publication_initialized("writer", 0)
        .unwrap());
    chain.head.store(13, Ordering::Relaxed);
    let task = start(restarted, chain);
    let live = wait_for_head(&kafka_config, 13).await;
    assert_eq!(live.block, block(13));
    assert_eq!(live.offset, historical.offset + 1);
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
}
