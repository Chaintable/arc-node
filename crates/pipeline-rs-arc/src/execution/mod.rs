//! Capture during execution and shared DeBank output projection.

pub mod debank;
mod event_inspector;
mod evm;
mod executor;
mod lifecycle;
mod projection;

use revm::bytecode::opcode::OpCode;
use revm_inspectors::tracing::{OpcodeFilter, TracingInspector, TracingInspectorConfig};

pub use event_inspector::{
    record_subcall_trace_completion, ArcEventInspector, SubcallTraceCompletion,
    SubcallTraceCompletionPhase,
};
pub use evm::{CaptureEvm, CaptureInspector};
pub use executor::WriterExecutor;
pub use lifecycle::LiveCapture;
pub use projection::{
    append_traces, block_metadata, capture_block, capture_genesis, capture_replayed_block,
    debank_transaction_target, genesis_output, project_output, transaction_metadata,
    validate_transaction_classification,
};

pub type TraceInspector = (TracingInspector, ArcEventInspector);

pub fn new_debank_inspector() -> TraceInspector {
    let mut config = TracingInspectorConfig::default_parity()
        .set_steps(true)
        .set_record_logs(true)
        .set_exclude_precompile_calls(false);
    config.record_opcodes_filter = Some(OpcodeFilter::new().enabled(OpCode::SSTORE));
    (TracingInspector::new(config), ArcEventInspector::default())
}
