use std::task::{Context, Poll};

use alloy_consensus::{BlockHeader, Header};
use alloy_primitives::B256;
use reth_provider::{HeaderProvider, ProviderError, StageCheckpointReader, StageCheckpointWriter};
use reth_stages::{
    stages::MerkleStage, ExecInput, ExecOutput, Stage, StageError, StageId, UnwindInput,
    UnwindOutput,
};
use serde::{Deserialize, Serialize};
use tokio::sync::watch;

const WRITER_MERKLE: StageId = StageId::Other("ArcWriterMerkleV1");

/// A successfully checked range of the native historical execution pipeline.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifiedRange {
    pub start: u64,
    pub start_parent: B256,
    pub end: u64,
    pub end_hash: B256,
    pub end_state_root: B256,
}

#[derive(Serialize, Deserialize)]
struct Proofs {
    source_id: String,
    ranges: Vec<VerifiedRange>,
}

pub fn committed_ranges(
    provider: &impl StageCheckpointReader,
    source_id: &str,
) -> Result<Vec<VerifiedRange>, StageError> {
    let Some(bytes) = provider.get_stage_checkpoint_progress(WRITER_MERKLE)? else {
        return Ok(Vec::new());
    };
    let proofs: Proofs =
        serde_json::from_slice(&bytes).map_err(|err| StageError::Fatal(err.into()))?;
    if proofs.source_id != source_id {
        return Err(StageError::Fatal(
            "Arc Writer Merkle source ID mismatch".into(),
        ));
    }
    Ok(proofs.ranges)
}

/// Adds Writer evidence to the same committed transaction as MerkleExecute.
pub struct WriterMerkleStage {
    inner: MerkleStage,
    source_id: String,
    changed: watch::Sender<()>,
}

impl WriterMerkleStage {
    pub fn new(inner: MerkleStage, source_id: String, changed: watch::Sender<()>) -> Self {
        Self {
            inner,
            source_id,
            changed,
        }
    }

    fn save(
        &self,
        provider: &impl StageCheckpointWriter,
        ranges: Vec<VerifiedRange>,
    ) -> Result<(), StageError> {
        let proofs = Proofs {
            source_id: self.source_id.clone(),
            ranges,
        };
        let bytes = serde_json::to_vec(&proofs).map_err(|err| StageError::Fatal(err.into()))?;
        provider.save_stage_checkpoint_progress(WRITER_MERKLE, bytes)?;
        Ok(())
    }
}

impl<P> Stage<P> for WriterMerkleStage
where
    P: HeaderProvider<Header = Header> + StageCheckpointReader + StageCheckpointWriter,
    MerkleStage: Stage<P>,
{
    fn id(&self) -> StageId {
        <MerkleStage as Stage<P>>::id(&self.inner)
    }

    fn poll_execute_ready(
        &mut self,
        cx: &mut Context<'_>,
        input: ExecInput,
    ) -> Poll<Result<(), StageError>> {
        <MerkleStage as Stage<P>>::poll_execute_ready(&mut self.inner, cx, input)
    }

    fn execute(&mut self, provider: &P, input: ExecInput) -> Result<ExecOutput, StageError> {
        let out = self.inner.execute(provider, input)?;
        let checkpoint = input.checkpoint().block_number;
        let end = out.checkpoint.block_number;
        if out.done && end > checkpoint {
            let start = input.next_block();
            let first = provider
                .header_by_number(start)?
                .ok_or_else(|| ProviderError::HeaderNotFound(start.into()))?;
            let last = provider
                .sealed_header(end)?
                .ok_or_else(|| ProviderError::HeaderNotFound(end.into()))?;
            let mut ranges = committed_ranges(provider, &self.source_id)?;
            ranges.push(VerifiedRange {
                start,
                start_parent: first.parent_hash(),
                end,
                end_hash: last.hash(),
                end_state_root: last.state_root(),
            });
            self.save(provider, ranges)?;
        }
        Ok(out)
    }

    fn post_execute_commit(&mut self) -> Result<(), StageError> {
        <MerkleStage as Stage<P>>::post_execute_commit(&mut self.inner)?;
        self.changed.send_modify(|_| {});
        Ok(())
    }

    fn unwind(&mut self, provider: &P, input: UnwindInput) -> Result<UnwindOutput, StageError> {
        let out = self.inner.unwind(provider, input)?;
        let mut ranges = committed_ranges(provider, &self.source_id)?;
        let count = ranges.len();
        ranges.retain(|range| range.end <= out.checkpoint.block_number);
        if ranges.len() != count {
            self.save(provider, ranges)?;
        }
        Ok(out)
    }

    fn post_unwind_commit(&mut self) -> Result<(), StageError> {
        <MerkleStage as Stage<P>>::post_unwind_commit(&mut self.inner)?;
        self.changed.send_modify(|_| {});
        Ok(())
    }
}
