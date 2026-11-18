use alloy_evm::{eth::EthEvmContext, Database, EvmEnv, EvmFactory};
use arc_evm::ArcEvmFactory;
use pipeline_rs_arc::execution::{CaptureEvm, CaptureInspector};
use revm::inspector::Inspector;

use crate::event_inspector::record_subcall_trace_completion;

pub type WriterEvm<DB, I> =
    CaptureEvm<<ArcEvmFactory as EvmFactory>::Evm<DB, CaptureInspector<I>>, I>;

pub(super) fn writer_evm<DB: Database, I: Inspector<EthEvmContext<DB>>>(
    factory: &ArcEvmFactory,
    db: DB,
    env: EvmEnv,
    external: I,
    external_enabled: bool,
) -> WriterEvm<DB, I> {
    let mut evm = factory.create_evm_with_inspector(db, env, CaptureInspector::new(external));
    evm.set_subcall_trace_completion_hook(|inspector, completion| {
        if let Some(capture) = inspector.capture_mut() {
            record_subcall_trace_completion(capture, completion);
        }
    });
    CaptureEvm::new(evm, external_enabled)
}
