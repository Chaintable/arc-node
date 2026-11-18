use std::{fmt, sync::Arc};

use crate::{assembler::ArcBlockAssembler, executor::ArcBlockExecutor, ArcEvmConfig};
use alloy_consensus::Header;
use alloy_evm::{
    block::{state::StateDB, BlockExecutorFactory},
    eth::{EthBlockExecutionCtx, EthEvmContext},
    precompiles::PrecompilesMap,
    Database, EvmEnv, EvmFactory,
};
use alloy_rpc_types_engine::ExecutionData;
use arc_execution_config::chainspec::ArcChainSpec;
use pipeline_rs_arc::execution::{LiveCapture, WriterExecutor};
use reth_ethereum_primitives::{EthPrimitives, Receipt, TransactionSigned};
use reth_evm::{
    execute::{BlockExecutionError, Executor},
    ConfigureEngineEvm, ConfigureEvm, EvmEnvFor, ExecutableTxIterator, ExecutionCtxFor,
};
use reth_primitives_traits::{BlockTy, SealedBlock, SealedHeader};
use revm::{
    context::{BlockEnv, TxEnv},
    context_interface::result::{EVMError, HaltReason},
    inspector::{Inspector, NoOpInspector},
    primitives::hardfork::SpecId,
};

use super::evm::{writer_evm, WriterEvm};

#[derive(Clone)]
pub struct WriterEvmConfig {
    inner: ArcEvmConfig,
    capture: LiveCapture,
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
        Self { inner, capture }
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
        executor.set_capture(|output| self.capture.store(output));
        executor
    }
}

impl ConfigureEvm for WriterEvmConfig {
    type Primitives = EthPrimitives;
    type Error = <ArcEvmConfig as ConfigureEvm>::Error;
    type NextBlockEnvCtx = <ArcEvmConfig as ConfigureEvm>::NextBlockEnvCtx;
    type BlockExecutorFactory = Self;
    type BlockAssembler = ArcBlockAssembler<ArcChainSpec>;

    fn batch_executor<DB: Database>(
        &self,
        db: DB,
    ) -> impl Executor<DB, Primitives = Self::Primitives, Error = BlockExecutionError> {
        // The native invalid-block witness hook uses a batch executor to replay
        // the rejected block. Keep that replay out of the live capture slot.
        self.inner.batch_executor(db)
    }

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
