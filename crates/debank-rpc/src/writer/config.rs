use std::{fmt, sync::Arc};

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
use pipeline_rs_arc::{
    execution::{seal_history, LiveCapture, WriterExecutor},
    CaptureSink, SpoolError,
};
use reth_ethereum_primitives::{EthPrimitives, Receipt, TransactionSigned};
use reth_evm::{
    BlockExecutorForEvm, ConfigureEngineEvm, ConfigureEvm, EvmEnvFor, ExecutableTxIterator,
    ExecutionCtxFor,
};
use reth_primitives_traits::{BlockTy, SealedBlock, SealedHeader};
use revm::{
    context::{BlockEnv, TxEnv},
    context_interface::result::{EVMError, HaltReason},
    database::State,
    inspector::{Inspector, NoOpInspector},
    primitives::hardfork::SpecId,
};

use super::evm::{writer_evm, WriterEvm};

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
        writer_evm(
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
        writer_evm(
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
        let mut executor = WriterExecutor::new(self.inner.create_executor_with_evm(evm, ctx));
        if let Mode::Live(slot) = &self.mode {
            executor.set_capture(|_, output| slot.store(output));
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
            executor.set_capture(move |evm, output| {
                let result = seal_history(
                    block.header(),
                    block.hash(),
                    evm.db().transition_state.as_ref(),
                    output,
                    || root(block.parent_hash()),
                    sink.as_ref(),
                );
                if let Err(err) = result {
                    panic!(
                        "Arc Writer history capture failed at {} {}: {err}",
                        block.number(),
                        block.hash()
                    );
                }
            });
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
