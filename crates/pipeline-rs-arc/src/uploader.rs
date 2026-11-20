use std::{future::Future, io::Read};

use alloy_primitives::B256;
use flate2::read::GzDecoder;
use serde_json::Value;

use crate::{ObjectKind, ObjectSpec, Spool, SpoolError};

#[derive(Debug, PartialEq, Eq)]
pub enum ConditionalPut {
    Created,
    AlreadyExists,
    ConcurrentWrite,
}

/// Object-store operations with atomic create-if-absent semantics.
pub trait ObjectStore: Sync {
    type Error: std::error::Error + Send + Sync + 'static;

    fn create_if_absent(
        &self,
        object: &ObjectSpec<'_>,
        data: &[u8],
    ) -> impl Future<Output = Result<ConditionalPut, Self::Error>> + Send;

    fn read(
        &self,
        object: &ObjectSpec<'_>,
    ) -> impl Future<Output = Result<Vec<u8>, Self::Error>> + Send;
}

#[derive(Debug, thiserror::Error)]
pub enum UploadError<E: std::error::Error> {
    #[error(transparent)]
    Local(#[from] SpoolError),
    #[error("remote object operation failed: {0}")]
    Remote(E),
    #[error("conditional write raced for {0}")]
    Retry(String),
    #[error("existing object differs for block {hash:#x} at {key}")]
    Conflict { hash: B256, key: String },
}

/// Upload missing objects for one block. Each successful object is committed
/// independently so an interrupted attempt can resume from its remaining keys.
pub async fn upload_block<S: ObjectStore>(
    spool: &Spool,
    store: &S,
    hash: B256,
) -> Result<bool, UploadError<S::Error>> {
    let row = spool.block(hash)?.ok_or(SpoolError::Corrupt(hash))?;
    if !row.upload_eligible() {
        return Ok(false);
    }
    if row.remote_ready() {
        return Ok(true);
    }
    let capture = spool.raw(hash)?;
    let namespace = spool.namespace();
    let manifest = namespace.manifest(&capture)?;
    for kind in row.missing_objects() {
        if !spool
            .block(hash)?
            .ok_or(SpoolError::Corrupt(hash))?
            .upload_eligible()
        {
            return Ok(false);
        }
        let object = manifest
            .iter()
            .find(|object| object.kind == kind)
            .ok_or(SpoolError::Corrupt(hash))?;
        let mut data = Vec::new();
        kind.encode(&capture, &mut data)?;
        match store
            .create_if_absent(object, &data)
            .await
            .map_err(UploadError::Remote)?
        {
            ConditionalPut::Created => {}
            ConditionalPut::ConcurrentWrite => {
                return Err(UploadError::Retry(object.key.clone()));
            }
            ConditionalPut::AlreadyExists => {
                let existing = store.read(object).await.map_err(UploadError::Remote)?;
                if !same_content(kind, &data, &existing) {
                    return Err(UploadError::Conflict {
                        hash,
                        key: object.key.clone(),
                    });
                }
            }
        }
        spool.acknowledge(hash, kind)?;
    }
    Ok(true)
}

fn same_content(kind: ObjectKind, expected: &[u8], existing: &[u8]) -> bool {
    if kind == ObjectKind::StateDiff {
        return expected == existing;
    }
    let Some(mut expected) = read_json(expected) else {
        return false;
    };
    let Some(mut existing) = read_json(existing) else {
        return false;
    };
    match kind {
        ObjectKind::BlockFile => {
            for value in [&mut expected, &mut existing] {
                if let Some(block) = value.get_mut("block").and_then(Value::as_object_mut) {
                    block.remove("process_start_timestamp");
                }
            }
        }
        ObjectKind::Validation => {
            // Fork status belongs to the checker after the initial write.
            if !existing.get("is_fork").is_some_and(Value::is_boolean) {
                return false;
            }
            existing["is_fork"] = Value::Bool(false);
        }
        _ => {}
    }
    expected == existing
}

fn read_json(compressed: &[u8]) -> Option<Value> {
    let mut gzip = GzDecoder::new(compressed);
    let mut bytes = Vec::new();
    gzip.read_to_end(&mut bytes).ok()?;
    serde_json::from_slice(&bytes).ok()
}
