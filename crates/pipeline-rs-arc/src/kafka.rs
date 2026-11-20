use std::{
    io::Read,
    time::{Duration, Instant},
};

use flate2::read::GzDecoder;
use rdkafka::{
    consumer::{BaseConsumer, Consumer},
    error::KafkaError,
    message::{Header, OwnedHeaders},
    producer::{FutureProducer, FutureRecord, Producer},
    ClientConfig, Message, Offset, TopicPartitionList,
};

use crate::{BlockRef, Notice, SpoolError, NOTIFICATION_KEY};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KafkaTail {
    pub offset: i64,
    pub block: BlockRef,
}

#[derive(Debug, thiserror::Error)]
pub enum KafkaStreamError {
    #[error(transparent)]
    Broker(#[from] KafkaError),
    #[error(transparent)]
    Encoding(#[from] SpoolError),
    #[error("no valid NewBlock notice remains in the initialized partition")]
    MissingTail,
    #[error("could not establish the partition tail before the read deadline")]
    TailTimeout,
    #[error("invalid NewBlock notice at Kafka offset {0}")]
    InvalidNotice(i64),
    #[error("the partition tail has to be recovered before publishing")]
    NotRecovered,
    #[error("notification is {actual} bytes, exceeding {maximum}")]
    Oversize { actual: usize, maximum: usize },
}

/// One ordered topic partition with a single in-flight notification.
pub struct KafkaStream {
    consumer: BaseConsumer,
    producer: FutureProducer,
    topic: String,
    partition: i32,
    timeout: Duration,
    max_bytes: usize,
    recovered: bool,
}

impl KafkaStream {
    /// Connection, SASL, and TLS settings are supplied by the caller.
    pub fn new(
        config: &ClientConfig,
        topic: String,
        partition: i32,
        timeout: Duration,
        max_bytes: usize,
    ) -> Result<Self, KafkaStreamError> {
        let mut consumer_config = config.clone();
        if consumer_config.get("group.id").is_none() {
            consumer_config.set("group.id", "arc-writer-recovery");
        }
        let consumer = consumer_config
            .set("enable.partition.eof", "true")
            .set("enable.auto.commit", "false")
            .set("enable.auto.offset.store", "false")
            .create()?;
        let producer = config
            .clone()
            .set("enable.idempotence", "true")
            .set("acks", "all")
            .create()?;
        Ok(Self {
            consumer,
            producer,
            topic,
            partition,
            timeout,
            max_bytes,
            recovered: false,
        })
    }

    /// Run off the async executor. Stops after the initial watermark even if
    /// unrelated keys are written to the same partition during recovery.
    /// For a handover the caller must first drain the previous owner.
    pub fn recover(&mut self, initialized: bool) -> Result<Option<KafkaTail>, KafkaStreamError> {
        self.recovered = false;
        self.producer.flush(self.timeout)?;
        let (low, high) =
            self.consumer
                .fetch_watermarks(&self.topic, self.partition, self.timeout)?;
        let deadline = Instant::now() + self.timeout;
        let mut end = high;
        while end > low {
            let start = end.saturating_sub(256).max(low);
            let mut partitions = TopicPartitionList::new();
            partitions.add_partition_offset(&self.topic, self.partition, Offset::Offset(start))?;
            self.consumer.assign(&partitions)?;
            let mut tail = None;
            loop {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Err(KafkaStreamError::TailTimeout);
                }
                match self.consumer.poll(remaining) {
                    None => return Err(KafkaStreamError::TailTimeout),
                    Some(Err(KafkaError::PartitionEOF(part))) if part == self.partition => break,
                    Some(Err(error)) => return Err(error.into()),
                    Some(Ok(message)) => {
                        let offset = message.offset();
                        if offset >= end {
                            break;
                        }
                        if offset >= start && message.key() == Some(NOTIFICATION_KEY) {
                            tail = Some(decode_tail(offset, message.payload(), self.max_bytes));
                        }
                        if offset == end - 1 {
                            break;
                        }
                    }
                }
            }
            if let Some(tail) = tail {
                let tail = tail?;
                self.recovered = true;
                return Ok(Some(tail));
            }
            end = start;
        }
        if initialized {
            return Err(KafkaStreamError::MissingTail);
        }
        self.recovered = true;
        Ok(None)
    }

    /// Holds mutable access over the producer acknowledgement. Broker errors
    /// freeze sending until ownership and the actual tail have been recovered.
    pub async fn send(
        &mut self,
        notice: &Notice,
        first_seen_ms: Option<&str>,
    ) -> Result<KafkaTail, KafkaStreamError> {
        if !self.recovered {
            return Err(KafkaStreamError::NotRecovered);
        }
        let head = notice
            .new_blocks
            .last()
            .copied()
            .ok_or(KafkaStreamError::InvalidNotice(-1))?;
        let mut payload = Vec::new();
        notice.encode(&mut payload)?;
        if payload.len() > self.max_bytes {
            return Err(KafkaStreamError::Oversize {
                actual: payload.len(),
                maximum: self.max_bytes,
            });
        }
        let mut record = FutureRecord::to(&self.topic)
            .partition(self.partition)
            .key(NOTIFICATION_KEY)
            .payload(payload.as_slice());
        if let Some(value) = first_seen_ms {
            record = record.headers(OwnedHeaders::new().insert(Header {
                key: "block-p2p-header-recv",
                value: Some(value),
            }));
        }
        self.recovered = false;
        let (_, offset) = self
            .producer
            .send(record, self.timeout)
            .await
            .map_err(|(err, _)| err)?;
        self.recovered = true;
        Ok(KafkaTail {
            offset,
            block: head,
        })
    }
}

fn decode_tail(
    offset: i64,
    payload: Option<&[u8]>,
    max_bytes: usize,
) -> Result<KafkaTail, KafkaStreamError> {
    fn contiguous(blocks: &[BlockRef]) -> bool {
        blocks.windows(2).all(|pair| {
            pair[0].block_number.checked_add(1) == Some(pair[1].block_number)
                && pair[0].hash == pair[1].parent_hash
        })
    }

    let invalid = || KafkaStreamError::InvalidNotice(offset);
    let data = payload
        .filter(|data| data.len() <= max_bytes)
        .ok_or_else(invalid)?;
    let mut json = Vec::new();
    GzDecoder::new(data)
        .take((max_bytes as u64).saturating_mul(32))
        .read_to_end(&mut json)
        .map_err(|_| invalid())?;
    let notice: Notice = serde_json::from_slice(&json).map_err(|_| invalid())?;
    let head = *notice.new_blocks.last().ok_or_else(invalid)?;
    match (notice.change_type, notice.drop_blocks.as_deref()) {
        (1, None | Some([])) if contiguous(&notice.new_blocks) => {}
        (2, Some(old))
            if !old.is_empty()
                && contiguous(old)
                && contiguous(&notice.new_blocks)
                && old[0].block_number == notice.new_blocks[0].block_number
                && old[0].parent_hash == notice.new_blocks[0].parent_hash
                && old[old.len() - 1].block_number < head.block_number => {}
        _ => return Err(invalid()),
    }
    Ok(KafkaTail {
        offset,
        block: head,
    })
}
