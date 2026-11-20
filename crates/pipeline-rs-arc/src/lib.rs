//! Arc execution artifacts and their object storage protocol.

mod capture;
mod objects;

pub use capture::{BlockRef, CaptureSink, CapturedArcBlock, SpoolError, Validation};
pub use objects::{ObjectEncoding, ObjectKind, ObjectNamespace, ObjectSpec};

mod spool;
pub use spool::{Spool, SpoolBlock, SpoolIdentity};

mod uploader;
pub use uploader::{upload_block, ConditionalPut, ObjectStore, UploadError};

mod notice;
pub use notice::{next_notice, Notice, NoticeError, PublicationView, NOTIFICATION_KEY};

#[cfg(feature = "s3")]
mod s3;
#[cfg(feature = "s3")]
pub use s3::{S3Error, S3ObjectStore};

#[cfg(feature = "kafka")]
mod kafka;
#[cfg(feature = "kafka")]
pub use kafka::{KafkaStream, KafkaStreamError, KafkaTail};

#[cfg(feature = "execution")]
pub mod execution;

#[cfg(feature = "delivery")]
mod delivery;
#[cfg(feature = "delivery")]
pub use delivery::{Delivery, DeliveryChain, DeliveryConfig};
