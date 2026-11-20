use std::sync::{Arc, Mutex};

use alloy_consensus::Header;
use alloy_primitives::B256;
use revm::database::BundleState;

use super::{capture_block, debank::BlockFile};
use crate::{CaptureSink, SpoolError};

// One slot per engine validator, used by its serial validate_payload/validate_block calls.
#[derive(Clone, Default)]
pub struct LiveCapture {
    pending: Arc<Mutex<Option<Result<BlockFile, String>>>>,
}

impl LiveCapture {
    pub fn discard(&self) {
        self.pending
            .lock()
            .expect("live Writer lock poisoned")
            .take();
    }

    pub fn accept(
        &self,
        header: &Header,
        hash: B256,
        parent_state_root: B256,
        bundle: BundleState,
        sink: &dyn CaptureSink,
    ) -> Result<(), SpoolError> {
        let output = self
            .pending
            .lock()
            .expect("live Writer lock poisoned")
            .take()
            .ok_or_else(|| SpoolError::Projection(format!("missing live output for {hash}")))?
            .map_err(SpoolError::Projection)?;
        sink.seal(capture_block(
            header,
            hash,
            parent_state_root,
            bundle,
            output,
        )?)
    }

    pub fn store(&self, output: Result<BlockFile, String>) {
        let replaced = self
            .pending
            .lock()
            .expect("live Writer lock poisoned")
            .replace(output);
        assert!(
            replaced.is_none(),
            "live Writer captured more than one block"
        );
    }
}
