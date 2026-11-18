//! Adapts Arc EVM subcall completion to the library's trace observer.

use arc_evm::{SubcallTraceCompletion, SubcallTraceCompletionPhase};
use pipeline_rs_arc::execution::{self, TraceInspector};

pub(crate) fn record_subcall_trace_completion(
    inspector: &mut TraceInspector,
    completion: SubcallTraceCompletion,
) {
    execution::record_subcall_trace_completion(
        inspector,
        execution::SubcallTraceCompletion {
            child_status: completion.child_status,
            child_output: completion.child_output,
            child_gas_used: completion.child_gas_used,
            child_gas_limit: completion.child_gas_limit,
            final_status: completion.final_status,
            rolled_back_by_completion: completion.rolled_back_by_completion,
            post_completion_log_count: completion.post_completion_log_count,
            phase: match completion.phase {
                SubcallTraceCompletionPhase::BeforeFrameEnd => {
                    execution::SubcallTraceCompletionPhase::BeforeFrameEnd
                }
                SubcallTraceCompletionPhase::AfterFrameEnd => {
                    execution::SubcallTraceCompletionPhase::AfterFrameEnd
                }
            },
        },
    );
}
