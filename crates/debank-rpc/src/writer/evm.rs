use alloy_evm::{
    eth::EthEvmContext, precompiles::PrecompilesMap, Database, Evm, EvmEnv, EvmFactory,
};
use alloy_primitives::{Address, Bytes, Log, U256};
use arc_evm::{ArcEvmFactory, SubcallTraceCompletion};
use revm::{
    context::{BlockEnv, CfgEnv, TxEnv},
    context_interface::result::{EVMError, HaltReason, ResultAndState},
    handler::FrameResult,
    inspector::Inspector,
    interpreter::{CallInputs, CallOutcome, CreateInputs, CreateOutcome, FrameInput, Interpreter},
    primitives::hardfork::SpecId,
};
use revm_inspectors::tracing::TracingInspector;

use crate::{
    event_inspector::{record_subcall_trace_completion, ArcEventInspector},
    trace_block::new_debank_inspector,
};

pub struct WriterInspector<I> {
    external: I,
    // Disabled during prewarming, construction and block-level system calls.
    capture: Option<(TracingInspector, ArcEventInspector)>,
}

impl<DB: Database, I: Inspector<EthEvmContext<DB>>> Inspector<EthEvmContext<DB>>
    for WriterInspector<I>
{
    fn initialize_interp(&mut self, interp: &mut Interpreter, ctx: &mut EthEvmContext<DB>) {
        self.external.initialize_interp(interp, ctx);
        if let Some(capture) = &mut self.capture {
            capture.initialize_interp(interp, ctx);
        }
    }

    fn step(&mut self, interp: &mut Interpreter, ctx: &mut EthEvmContext<DB>) {
        self.external.step(interp, ctx);
        if let Some(capture) = &mut self.capture {
            capture.step(interp, ctx);
        }
    }

    fn step_end(&mut self, interp: &mut Interpreter, ctx: &mut EthEvmContext<DB>) {
        self.external.step_end(interp, ctx);
        if let Some(capture) = &mut self.capture {
            capture.step_end(interp, ctx);
        }
    }

    fn log(&mut self, ctx: &mut EthEvmContext<DB>, log: Log) {
        if let Some(capture) = &mut self.capture {
            capture.log(ctx, log.clone());
        }
        self.external.log(ctx, log);
    }

    fn log_full(&mut self, interp: &mut Interpreter, ctx: &mut EthEvmContext<DB>, log: Log) {
        if let Some(capture) = &mut self.capture {
            capture.log_full(interp, ctx, log.clone());
        }
        self.external.log_full(interp, ctx, log);
    }

    fn frame_start(
        &mut self,
        ctx: &mut EthEvmContext<DB>,
        input: &mut FrameInput,
    ) -> Option<FrameResult> {
        let outcome = self.external.frame_start(ctx, input);
        if let Some(capture) = &mut self.capture {
            capture.frame_start(ctx, input);
        }
        outcome
    }

    fn frame_end(
        &mut self,
        ctx: &mut EthEvmContext<DB>,
        input: &FrameInput,
        result: &mut FrameResult,
    ) {
        self.external.frame_end(ctx, input, result);
        if let Some(capture) = &mut self.capture {
            capture.frame_end(ctx, input, result);
        }
    }

    fn call(&mut self, ctx: &mut EthEvmContext<DB>, input: &mut CallInputs) -> Option<CallOutcome> {
        let outcome = self.external.call(ctx, input);
        if let Some(capture) = &mut self.capture {
            capture.call(ctx, input);
        }
        outcome
    }

    fn call_end(
        &mut self,
        ctx: &mut EthEvmContext<DB>,
        input: &CallInputs,
        result: &mut CallOutcome,
    ) {
        self.external.call_end(ctx, input, result);
        if let Some(capture) = &mut self.capture {
            capture.call_end(ctx, input, result);
        }
    }

    fn create(
        &mut self,
        ctx: &mut EthEvmContext<DB>,
        input: &mut CreateInputs,
    ) -> Option<CreateOutcome> {
        let outcome = self.external.create(ctx, input);
        if let Some(capture) = &mut self.capture {
            capture.create(ctx, input);
        }
        outcome
    }

    fn create_end(
        &mut self,
        ctx: &mut EthEvmContext<DB>,
        input: &CreateInputs,
        result: &mut CreateOutcome,
    ) {
        self.external.create_end(ctx, input, result);
        if let Some(capture) = &mut self.capture {
            capture.create_end(ctx, input, result);
        }
    }

    fn selfdestruct(&mut self, contract: Address, target: Address, value: U256) {
        self.external.selfdestruct(contract, target, value);
        if let Some(capture) = &mut self.capture {
            <(TracingInspector, ArcEventInspector) as Inspector<EthEvmContext<DB>>>::selfdestruct(
                capture, contract, target, value,
            );
        }
    }
}

pub struct WriterEvm<DB: Database, I: Inspector<EthEvmContext<DB>>> {
    inner: <ArcEvmFactory as EvmFactory>::Evm<DB, WriterInspector<I>>,
    external_enabled: bool,
}

impl<DB: Database, I: Inspector<EthEvmContext<DB>>> WriterEvm<DB, I> {
    pub(super) fn start_capture(&mut self) {
        self.inner.inspector_mut().capture = Some(new_debank_inspector());
        self.inner.set_inspector_enabled(true);
    }

    pub(super) fn finish_capture(&mut self) -> Option<(TracingInspector, ArcEventInspector)> {
        self.inner.set_inspector_enabled(self.external_enabled);
        self.inner.inspector_mut().capture.take()
    }
}

impl<DB: Database, I: Inspector<EthEvmContext<DB>>> Evm for WriterEvm<DB, I> {
    type DB = DB;
    type Tx = TxEnv;
    type Error = EVMError<DB::Error>;
    type HaltReason = HaltReason;
    type Spec = SpecId;
    type BlockEnv = BlockEnv;
    type Precompiles = PrecompilesMap;
    type Inspector = I;

    fn block(&self) -> &BlockEnv {
        self.inner.block()
    }

    fn cfg_env(&self) -> &CfgEnv<SpecId> {
        self.inner.cfg_env()
    }

    fn chain_id(&self) -> u64 {
        self.inner.chain_id()
    }

    fn transact_raw(&mut self, tx: TxEnv) -> Result<ResultAndState<HaltReason>, Self::Error> {
        self.inner.transact_raw(tx)
    }

    fn transact_system_call(
        &mut self,
        caller: Address,
        contract: Address,
        data: Bytes,
    ) -> Result<ResultAndState<HaltReason>, Self::Error> {
        self.inner.transact_system_call(caller, contract, data)
    }

    fn finish(self) -> (DB, EvmEnv<SpecId>) {
        self.inner.finish()
    }

    fn set_inspector_enabled(&mut self, enabled: bool) {
        self.external_enabled = enabled;
        self.inner.set_inspector_enabled(enabled);
    }

    fn components(&self) -> (&DB, &I, &PrecompilesMap) {
        let (db, inspector, precompiles) = self.inner.components();
        (db, &inspector.external, precompiles)
    }

    fn components_mut(&mut self) -> (&mut DB, &mut I, &mut PrecompilesMap) {
        let (db, inspector, precompiles) = self.inner.components_mut();
        (db, &mut inspector.external, precompiles)
    }
}

impl<DB: Database, I: Inspector<EthEvmContext<DB>>> WriterEvm<DB, I> {
    pub(super) fn new(
        factory: &ArcEvmFactory,
        db: DB,
        env: EvmEnv,
        external: I,
        external_enabled: bool,
    ) -> Self {
        let inspector = WriterInspector {
            external,
            capture: None,
        };
        let mut inner = factory.create_evm_with_inspector(db, env, inspector);
        inner.set_inspector_enabled(external_enabled);
        inner.set_subcall_trace_completion_hook(|inspector, completion: SubcallTraceCompletion| {
            if let Some(capture) = &mut inspector.capture {
                record_subcall_trace_completion(capture, completion);
            }
        });
        Self {
            inner,
            external_enabled,
        }
    }
}
