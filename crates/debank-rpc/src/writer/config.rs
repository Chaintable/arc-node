use std::{
    cell::RefCell,
    fmt,
    sync::{Arc, Mutex},
};

use alloy_consensus::{BlockHeader, Header};
use alloy_evm::{
    block::{state::StateDB, BlockExecutorFactory},
    eth::{EthBlockExecutionCtx, EthEvmContext},
    precompiles::PrecompilesMap,
    Database, Evm, EvmEnv, EvmFactory,
};
use alloy_primitives::B256;
use alloy_rpc_types_engine::ExecutionData;
use arc_evm::{assembler::ArcBlockAssembler, executor::ArcBlockExecutor, ArcEvmConfig};
use arc_execution_config::chainspec::ArcChainSpec;
use pipeline_rs_arc::{CaptureSink, CaptureSource, CapturedArcBlock, SpoolError};
use reth_ethereum_primitives::{EthPrimitives, Receipt, TransactionSigned};
use reth_evm::{
    BlockExecutorForEvm, ConfigureEngineEvm, ConfigureEvm, EvmEnvFor, ExecutableTxIterator,
    ExecutionCtxFor,
};
use reth_primitives_traits::{BlockTy, SealedBlock, SealedHeader};
use revm::{
    context::{BlockEnv, TxEnv},
    context_interface::result::{EVMError, HaltReason},
    database::{states::bundle_state::BundleRetention, BundleState, State},
    inspector::{Inspector, NoOpInspector},
    primitives::hardfork::SpecId,
};

use super::{
    capture_block,
    evm::WriterEvm,
    executor::{FinishCapture, WriterExecutor},
};
use crate::debank_trace::BlockFile;

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
        block: &SealedBlock<BlockTy<EthPrimitives>>,
        parent_state_root: B256,
        bundle: BundleState,
    ) -> Result<CapturedArcBlock, SpoolError> {
        let output = self
            .pending
            .lock()
            .expect("live Writer lock poisoned")
            .take()
            .ok_or_else(|| {
                SpoolError::Projection(format!("missing live output for {:?}", block.hash()))
            })?
            .map_err(SpoolError::Projection)?;
        capture_block(block, parent_state_root, bundle, output)
    }

    fn store(&self, output: Result<BlockFile, String>) {
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

type ParentRoot = Arc<dyn Fn(B256) -> Result<B256, SpoolError> + Send + Sync>;

#[derive(Clone)]
enum Mode {
    Live(LiveCapture),
    History {
        root: ParentRoot,
        sink: Arc<dyn CaptureSink>,
    },
}

#[derive(Clone)]
pub struct WriterEvmConfig {
    inner: ArcEvmConfig,
    mode: Mode,
}

impl fmt::Debug for WriterEvmConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WriterEvmConfig")
            .field("inner", &self.inner)
            .finish_non_exhaustive()
    }
}

impl WriterEvmConfig {
    pub fn live(inner: ArcEvmConfig, capture: LiveCapture) -> Self {
        Self {
            inner,
            mode: Mode::Live(capture),
        }
    }

    pub fn history(inner: ArcEvmConfig, root: ParentRoot, sink: Arc<dyn CaptureSink>) -> Self {
        Self {
            inner,
            mode: Mode::History { root, sink },
        }
    }
}

type ArcWriterExecutor<'a, DB, I> = ArcBlockExecutor<
    'a,
    WriterEvm<DB, I>,
    &'a Arc<ArcChainSpec>,
    &'a reth_ethereum::evm::RethReceiptBuilder,
>;

impl EvmFactory for WriterEvmConfig {
    type Evm<DB: Database, I: Inspector<EthEvmContext<DB>>> = WriterEvm<DB, I>;
    type Tx = TxEnv;
    type Error<DBError: core::error::Error + Send + Sync + 'static> = EVMError<DBError>;
    type HaltReason = HaltReason;
    type Context<DB: Database> = EthEvmContext<DB>;
    type Spec = SpecId;
    type BlockEnv = BlockEnv;
    type Precompiles = PrecompilesMap;

    fn create_evm<DB: Database>(&self, db: DB, input: EvmEnv) -> Self::Evm<DB, NoOpInspector> {
        WriterEvm::new(
            ConfigureEvm::evm_factory(&self.inner),
            db,
            input,
            NoOpInspector {},
            false,
        )
    }

    fn create_evm_with_inspector<DB: Database, I: Inspector<EthEvmContext<DB>>>(
        &self,
        db: DB,
        input: EvmEnv,
        inspector: I,
    ) -> Self::Evm<DB, I> {
        WriterEvm::new(
            ConfigureEvm::evm_factory(&self.inner),
            db,
            input,
            inspector,
            true,
        )
    }
}

impl BlockExecutorFactory for WriterEvmConfig {
    type EvmFactory = Self;
    type ExecutionCtx<'a> = EthBlockExecutionCtx<'a>;
    type Transaction = TransactionSigned;
    type Receipt = Receipt;
    type TxExecutionResult = <ArcEvmConfig as BlockExecutorFactory>::TxExecutionResult;
    type Executor<'a, DB: StateDB, I: Inspector<EthEvmContext<DB>>> =
        WriterExecutor<'a, ArcWriterExecutor<'a, DB, I>>;

    fn evm_factory(&self) -> &Self {
        self
    }

    fn create_executor<'a, DB, I>(
        &'a self,
        evm: WriterEvm<DB, I>,
        ctx: EthBlockExecutionCtx<'a>,
    ) -> Self::Executor<'a, DB, I>
    where
        DB: StateDB,
        I: Inspector<EthEvmContext<DB>>,
    {
        let mut executor = WriterExecutor {
            inner: self.inner.create_executor_with_evm(evm, ctx),
            capture: None,
            projection: BlockFile::default(),
            capture_error: None,
            pending: None,
            log_index: RefCell::new(0),
        };
        if let Mode::Live(slot) = &self.mode {
            executor.capture = Some(Box::new(|_, output| slot.store(output)));
        }
        executor
    }
}

impl ConfigureEvm for WriterEvmConfig {
    type Primitives = EthPrimitives;
    type Error = <ArcEvmConfig as ConfigureEvm>::Error;
    type NextBlockEnvCtx = <ArcEvmConfig as ConfigureEvm>::NextBlockEnvCtx;
    type BlockExecutorFactory = Self;
    type BlockAssembler = ArcBlockAssembler<ArcChainSpec>;

    fn block_executor_factory(&self) -> &Self {
        self
    }

    fn block_assembler(&self) -> &Self::BlockAssembler {
        self.inner.block_assembler()
    }

    fn evm_env(&self, header: &Header) -> Result<EvmEnvFor<Self>, Self::Error> {
        self.inner.evm_env(header)
    }

    fn next_evm_env(
        &self,
        parent: &Header,
        attributes: &Self::NextBlockEnvCtx,
    ) -> Result<EvmEnvFor<Self>, Self::Error> {
        self.inner.next_evm_env(parent, attributes)
    }

    fn context_for_block<'a>(
        &self,
        block: &'a SealedBlock<BlockTy<Self::Primitives>>,
    ) -> Result<ExecutionCtxFor<'a, Self>, Self::Error> {
        self.inner.context_for_block(block)
    }

    fn context_for_next_block(
        &self,
        parent: &SealedHeader,
        attributes: Self::NextBlockEnvCtx,
    ) -> Result<ExecutionCtxFor<'_, Self>, Self::Error> {
        self.inner.context_for_next_block(parent, attributes)
    }

    fn executor_for_block<'a, DB: Database>(
        &'a self,
        db: &'a mut State<DB>,
        block: &'a SealedBlock<BlockTy<Self::Primitives>>,
    ) -> Result<BlockExecutorForEvm<'a, Self, DB>, Self::Error> {
        let evm = self.evm_for_block(db, block.header())?;
        let ctx = self.context_for_block(block)?;
        let mut executor = ConfigureEvm::create_executor(self, evm, ctx);
        if let Mode::History { root, sink } = &self.mode {
            let capture: FinishCapture<'a, WriterEvm<&'a mut State<DB>, NoOpInspector>> =
                Box::new(move |evm, output| {
                    let mut bundle = BundleState::default();
                    let transitions = evm
                        .db()
                        .transition_state
                        .as_ref()
                        .expect("historical Arc Writer requires bundle updates");
                    bundle.apply_transitions_and_create_reverts(
                        transitions.clone(),
                        BundleRetention::PlainState,
                    );
                    let identity = block.hash();
                    let result = output
                        .map_err(SpoolError::Projection)
                        .and_then(|output| {
                            root(block.parent_hash())
                                .and_then(|parent| capture_block(block, parent, bundle, output))
                        })
                        .and_then(|capture| {
                            sink.seal(capture, CaptureSource::HistoryAwaitingMerkle)
                        });
                    if let Err(err) = result {
                        panic!(
                            "Arc Writer history capture failed at {} {identity}: {err}",
                            block.number()
                        );
                    }
                });
            executor.capture = Some(capture);
        }
        Ok(executor)
    }
}

impl ConfigureEngineEvm<ExecutionData> for WriterEvmConfig {
    fn evm_env_for_payload(&self, payload: &ExecutionData) -> Result<EvmEnvFor<Self>, Self::Error> {
        self.inner.evm_env_for_payload(payload)
    }

    fn context_for_payload<'a>(
        &self,
        payload: &'a ExecutionData,
    ) -> Result<ExecutionCtxFor<'a, Self>, Self::Error> {
        self.inner.context_for_payload(payload)
    }

    fn tx_iterator_for_payload(
        &self,
        payload: &ExecutionData,
    ) -> Result<impl ExecutableTxIterator<Self>, Self::Error> {
        self.inner.tx_iterator_for_payload(payload)
    }
}
