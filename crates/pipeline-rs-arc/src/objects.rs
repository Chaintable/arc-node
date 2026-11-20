use std::io::{self, Write};

use flate2::{write::GzEncoder, Compression};
use serde::{Deserialize, Serialize};

use crate::{CapturedArcBlock, SpoolError};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ObjectKind {
    BlockFile,
    Validation,
    Header,
    StateDiff,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObjectEncoding {
    GzipJson,
    Rlp,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObjectSpec<'a> {
    pub kind: ObjectKind,
    pub bucket: &'a str,
    pub key: String,
    pub encoding: ObjectEncoding,
}

pub struct ObjectNamespace<'a> {
    pub chain_id: u64,
    pub version: &'a str,
    pub blockfile_bucket: &'a str,
    pub nodex_bucket: &'a str,
}

impl ObjectNamespace<'_> {
    /// Returns the exact set of objects required by the Arc wire protocol.
    pub fn manifest(&self, block: &CapturedArcBlock) -> Result<Vec<ObjectSpec<'_>>, SpoolError> {
        block.validate_state_diff()?;
        let prefix = if self.version.is_empty() {
            self.chain_id.to_string()
        } else {
            format!("{}/{}", self.chain_id, self.version)
        };
        let hash = block.block.hash;
        let number = block.block.block_number;
        let mut objects = vec![
            ObjectSpec {
                kind: ObjectKind::BlockFile,
                bucket: self.blockfile_bucket,
                key: format!("{prefix}/{hash:#x}"),
                encoding: ObjectEncoding::GzipJson,
            },
            ObjectSpec {
                kind: ObjectKind::Validation,
                bucket: self.blockfile_bucket,
                key: format!("{prefix}/{number}/{hash:#x}"),
                encoding: ObjectEncoding::GzipJson,
            },
            ObjectSpec {
                kind: ObjectKind::Header,
                bucket: self.nodex_bucket,
                key: format!("{prefix}/{hash:#x}/block"),
                encoding: ObjectEncoding::GzipJson,
            },
        ];
        if block.state_root != block.parent_state_root {
            objects.push(ObjectSpec {
                kind: ObjectKind::StateDiff,
                bucket: self.nodex_bucket,
                key: format!("{prefix}/{:#x}/stateDiff", block.state_root),
                encoding: ObjectEncoding::Rlp,
            });
        }
        Ok(objects)
    }
}

impl ObjectKind {
    pub(crate) const fn bit(self) -> u8 {
        match self {
            Self::BlockFile => 1,
            Self::Validation => 2,
            Self::Header => 4,
            Self::StateDiff => 8,
        }
    }

    /// Writes one manifest entry to the destination in its wire encoding.
    pub fn encode(
        self,
        block: &CapturedArcBlock,
        mut output: impl Write,
    ) -> Result<(), SpoolError> {
        if self == Self::StateDiff {
            block.validate_state_diff()?;
            if block.state_root == block.parent_state_root {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "Arc omits the state diff object for equal roots",
                )
                .into());
            }
            output.write_all(&block.state_diff_rlp)?;
            return Ok(());
        }

        let mut gzip = GzEncoder::new(output, Compression::default());
        match self {
            Self::BlockFile => gzip.write_all(block.block_file.get().as_bytes())?,
            Self::Validation => serde_json::to_writer(&mut gzip, &block.validation)?,
            Self::Header => gzip.write_all(block.header.get().as_bytes())?,
            Self::StateDiff => unreachable!("RLP is written without compression"),
        }
        gzip.finish()?;
        Ok(())
    }
}
