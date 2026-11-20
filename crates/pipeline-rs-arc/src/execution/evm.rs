use alloy_evm::{Evm, EvmEnv};
use alloy_primitives::{Address, Bytes, Log, U256};
use revm::{
    context::CfgEnv,
    context_interface::result::ResultAndState,
    handler::FrameResult,
    inspector::Inspector,
    interpreter::{CallInputs, CallOutcome, CreateInputs, CreateOutcome, FrameInput, Interpreter},
};

use super::{new_debank_inspector, TraceInspector};

pub struct CaptureInspector<I> {
    external: I,
    // Disabled during prewarming, construction and block-level system calls.
    capture: Option<TraceInspector>,
}

impl<I> CaptureInspector<I> {
    pub fn new(external: I) -> Self {
        Self {
            external,
            capture: None,
        }
    }

    pub fn capture_mut(&mut self) -> Option<&mut TraceInspector> {
        self.capture.as_mut()
    }
}

impl<CTX, I: Inspector<CTX>> Inspector<CTX> for CaptureInspector<I>
where
    TraceInspector: Inspector<CTX>,
{
    fn initialize_interp(&mut self, interp: &mut Interpreter, ctx: &mut CTX) {
        self.external.initialize_interp(interp, ctx);
        if let Some(capture) = &mut self.capture {
            capture.initialize_interp(interp, ctx);
        }
    }

    fn step(&mut self, interp: &mut Interpreter, ctx: &mut CTX) {
        self.external.step(interp, ctx);
        if let Some(capture) = &mut self.capture {
            capture.step(interp, ctx);
        }
    }

    fn step_end(&mut self, interp: &mut Interpreter, ctx: &mut CTX) {
        self.external.step_end(interp, ctx);
        if let Some(capture) = &mut self.capture {
            capture.step_end(interp, ctx);
        }
    }

    fn log(&mut self, ctx: &mut CTX, log: Log) {
        if let Some(capture) = &mut self.capture {
            capture.log(ctx, log.clone());
        }
        self.external.log(ctx, log);
    }

    fn log_full(&mut self, interp: &mut Interpreter, ctx: &mut CTX, log: Log) {
        if let Some(capture) = &mut self.capture {
            capture.log_full(interp, ctx, log.clone());
        }
        self.external.log_full(interp, ctx, log);
    }

    fn frame_start(&mut self, ctx: &mut CTX, input: &mut FrameInput) -> Option<FrameResult> {
        let outcome = self.external.frame_start(ctx, input);
        if let Some(capture) = &mut self.capture {
            capture.frame_start(ctx, input);
        }
        outcome
    }

    fn frame_end(&mut self, ctx: &mut CTX, input: &FrameInput, result: &mut FrameResult) {
        self.external.frame_end(ctx, input, result);
        if let Some(capture) = &mut self.capture {
            capture.frame_end(ctx, input, result);
        }
    }

    fn call(&mut self, ctx: &mut CTX, input: &mut CallInputs) -> Option<CallOutcome> {
        let outcome = self.external.call(ctx, input);
        if let Some(capture) = &mut self.capture {
            capture.call(ctx, input);
        }
        outcome
    }

    fn call_end(&mut self, ctx: &mut CTX, input: &CallInputs, result: &mut CallOutcome) {
        self.external.call_end(ctx, input, result);
        if let Some(capture) = &mut self.capture {
            capture.call_end(ctx, input, result);
        }
    }

    fn create(&mut self, ctx: &mut CTX, input: &mut CreateInputs) -> Option<CreateOutcome> {
        let outcome = self.external.create(ctx, input);
        if let Some(capture) = &mut self.capture {
            capture.create(ctx, input);
        }
        outcome
    }

    fn create_end(&mut self, ctx: &mut CTX, input: &CreateInputs, result: &mut CreateOutcome) {
        self.external.create_end(ctx, input, result);
        if let Some(capture) = &mut self.capture {
            capture.create_end(ctx, input, result);
        }
    }

    fn selfdestruct(&mut self, contract: Address, target: Address, value: U256) {
        self.external.selfdestruct(contract, target, value);
        if let Some(capture) = &mut self.capture {
            <TraceInspector as Inspector<CTX>>::selfdestruct(capture, contract, target, value);
        }
    }
}

/// Wraps a native EVM with a transaction-scoped capture inspector.
pub struct CaptureEvm<E, I> {
    inner: E,
    external_enabled: bool,
    inspector: std::marker::PhantomData<fn() -> I>,
}

impl<E, I> CaptureEvm<E, I>
where
    E: Evm<Inspector = CaptureInspector<I>>,
{
    pub fn new(mut inner: E, external_enabled: bool) -> Self {
        inner.set_inspector_enabled(external_enabled);
        Self {
            inner,
            external_enabled,
            inspector: std::marker::PhantomData,
        }
    }

    pub(super) fn start_capture(&mut self) {
        self.inner.inspector_mut().capture = Some(new_debank_inspector());
        self.inner.set_inspector_enabled(true);
    }

    pub(super) fn finish_capture(&mut self) -> Option<TraceInspector> {
        self.inner.set_inspector_enabled(self.external_enabled);
        self.inner.inspector_mut().capture.take()
    }
}

impl<E, I> Evm for CaptureEvm<E, I>
where
    E: Evm<Inspector = CaptureInspector<I>>,
{
    type DB = E::DB;
    type Tx = E::Tx;
    type Error = E::Error;
    type HaltReason = E::HaltReason;
    type Spec = E::Spec;
    type BlockEnv = E::BlockEnv;
    type Precompiles = E::Precompiles;
    type Inspector = I;

    fn block(&self) -> &Self::BlockEnv {
        self.inner.block()
    }

    fn cfg_env(&self) -> &CfgEnv<Self::Spec> {
        self.inner.cfg_env()
    }

    fn chain_id(&self) -> u64 {
        self.inner.chain_id()
    }

    fn transact_raw(
        &mut self,
        tx: Self::Tx,
    ) -> Result<ResultAndState<Self::HaltReason>, Self::Error> {
        self.inner.transact_raw(tx)
    }

    fn transact_system_call(
        &mut self,
        caller: Address,
        contract: Address,
        data: Bytes,
    ) -> Result<ResultAndState<Self::HaltReason>, Self::Error> {
        self.inner.transact_system_call(caller, contract, data)
    }

    fn finish(self) -> (Self::DB, EvmEnv<Self::Spec, Self::BlockEnv>) {
        self.inner.finish()
    }

    fn set_inspector_enabled(&mut self, enabled: bool) {
        self.external_enabled = enabled;
        self.inner.set_inspector_enabled(enabled);
    }

    fn components(&self) -> (&Self::DB, &I, &Self::Precompiles) {
        let (db, inspector, precompiles) = self.inner.components();
        (db, &inspector.external, precompiles)
    }

    fn components_mut(&mut self) -> (&mut Self::DB, &mut I, &mut Self::Precompiles) {
        let (db, inspector, precompiles) = self.inner.components_mut();
        (db, &mut inspector.external, precompiles)
    }
}
