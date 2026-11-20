use alloy_primitives::{Bytes, B256};
use alloy_rlp::{Decodable, Header};
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;

/// Identifies a block even when multiple candidates exist at the same height.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockRef {
    pub hash: B256,
    pub parent_hash: B256,
    pub block_number: u64,
    pub timestamp: u64,
}

/// The legacy ETL's Validation object.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Validation {
    pub validation_hash: i64,
    pub is_fork: bool,
    pub txs_count: usize,
    pub events_count: usize,
    pub traces_count: usize,
    pub error_events_count: usize,
    pub error_traces_count: usize,
    pub storage_contracts_count: usize,
}

/// Complete, owned result of one Arc block execution.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CapturedArcBlock {
    pub block: BlockRef,
    pub state_root: B256,
    pub parent_state_root: B256,
    /// JSON object using the Writer's BlockFile schema.
    pub block_file: Box<RawValue>,
    /// Flattened Alloy RPC header JSON, including its sealed block hash.
    pub header: Box<RawValue>,
    /// Six-item Arc RLP, also retained when the protocol omits the S3 object.
    pub state_diff_rlp: Bytes,
    pub validation: Validation,
}

impl CapturedArcBlock {
    /// Checks the RLP envelope and its binding to both state roots.
    pub fn validate_state_diff(&self) -> Result<(), SpoolError> {
        let mut bytes = self.state_diff_rlp.as_ref();
        let mut items = Header::decode_bytes(&mut bytes, true)?;
        if B256::decode(&mut items)? != self.state_root
            || B256::decode(&mut items)? != self.parent_state_root
        {
            return Err(SpoolError::InvalidStateDiff("state root mismatch"));
        }
        // The remaining items are the account, deletion, storage and code lists.
        for _ in 0..4 {
            Header::decode_bytes(&mut items, true)?;
        }
        if !bytes.is_empty() || !items.is_empty() {
            return Err(SpoolError::InvalidStateDiff("trailing RLP data"));
        }
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SpoolError {
    #[error("capture I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("capture index: {0}")]
    Index(#[from] rusqlite::Error),
    #[error("Arc projection failed: {0}")]
    Projection(String),
    #[error("capture conflict for block {0:#x}")]
    Conflict(B256),
    #[error("spool belongs to a different chain, protocol or source database")]
    WrongIdentity,
    #[error("spool index disagrees with the capture at {0:#x}")]
    Corrupt(B256),
    #[error("capture JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid state diff RLP: {0}")]
    Rlp(#[from] alloy_rlp::Error),
    #[error("invalid state diff: {0}")]
    InvalidStateDiff(&'static str),
}

/// Synchronous capture boundary. A successful seal makes the capture recoverable.
pub trait CaptureSink: Send + Sync {
    fn seal(&self, block: CapturedArcBlock) -> Result<(), SpoolError>;
}
