//! Captures full Arc logs and their execution-frame ordering.
//!
//! `revm-inspectors` 0.34 stores only `LogData` in `CallLog`, which loses the
//! emitter address. Arc also emits EIP-7708 logs directly through the journal
//! during frame initialization and SELFDESTRUCT handling. This inspector runs
//! alongside `TracingInspector` and records the complete logs without changing
//! execution.

use alloy_primitives::Log;
use revm::{
    context_interface::{ContextTr, JournalTr},
    inspector::Inspector,
    interpreter::{CallInputs, CallOutcome, CreateInputs, CreateOutcome, Interpreter},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CapturedMember {
    Call(usize),
    Event(usize),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct CapturedFrame {
    pub parent: Option<usize>,
    pub members: Vec<CapturedMember>,
    pub success: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CapturedEvent {
    pub frame: usize,
    pub log: Log,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct CapturedEvents {
    pub frames: Vec<CapturedFrame>,
    pub events: Vec<CapturedEvent>,
    valid: bool,
}

impl CapturedEvents {
    pub fn validate(&self, expected_frames: usize) -> Result<(), String> {
        if !self.valid {
            return Err("event inspector observed an invalid frame stack".to_string());
        }
        if self.frames.len() != expected_frames {
            return Err(format!(
                "event inspector captured {} frames, tracing inspector captured {expected_frames}",
                self.frames.len()
            ));
        }
        Ok(())
    }

    pub fn successful_logs(&self) -> Vec<Log> {
        let mut frame_success = Vec::with_capacity(self.frames.len());
        for frame in &self.frames {
            let parent_success = frame
                .parent
                .and_then(|parent| frame_success.get(parent).copied())
                .unwrap_or(true);
            frame_success.push(parent_success && frame.success);
        }

        self.events
            .iter()
            .filter(|event| frame_success.get(event.frame).copied().unwrap_or(false))
            .map(|event| event.log.clone())
            .collect()
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ArcEventInspector {
    captured: CapturedEvents,
    frame_stack: Vec<usize>,
    journal_log_count: usize,
}

impl Default for ArcEventInspector {
    fn default() -> Self {
        Self {
            captured: CapturedEvents {
                valid: true,
                ..Default::default()
            },
            frame_stack: Vec::new(),
            journal_log_count: 0,
        }
    }
}

impl ArcEventInspector {
    pub fn into_captured(mut self) -> CapturedEvents {
        if !self.frame_stack.is_empty() {
            self.captured.valid = false;
        }
        self.captured
    }

    fn push_frame(&mut self) {
        let parent = self.frame_stack.last().copied();
        let frame_id = self.captured.frames.len();
        if let Some(parent_id) = parent {
            if let Some(parent_frame) = self.captured.frames.get_mut(parent_id) {
                parent_frame.members.push(CapturedMember::Call(frame_id));
            } else {
                self.captured.valid = false;
            }
        }
        self.captured.frames.push(CapturedFrame {
            parent,
            ..Default::default()
        });
        self.frame_stack.push(frame_id);
    }

    fn finish_frame(&mut self, success: bool) {
        let Some(frame_id) = self.frame_stack.pop() else {
            self.captured.valid = false;
            return;
        };
        if let Some(frame) = self.captured.frames.get_mut(frame_id) {
            frame.success = success;
        } else {
            self.captured.valid = false;
        }
    }

    fn push_event(&mut self, log: Log) {
        let Some(frame_id) = self.frame_stack.last().copied() else {
            self.captured.valid = false;
            return;
        };
        let event_id = self.captured.events.len();
        self.captured.events.push(CapturedEvent {
            frame: frame_id,
            log,
        });
        if let Some(frame) = self.captured.frames.get_mut(frame_id) {
            frame.members.push(CapturedMember::Event(event_id));
        } else {
            self.captured.valid = false;
        }
    }

    fn sync_journal<CTX: ContextTr>(&mut self, context: &CTX) {
        let logs = context.journal_ref().logs();
        if logs.len() < self.journal_log_count {
            self.journal_log_count = logs.len();
            return;
        }
        let new_logs = logs[self.journal_log_count..].to_vec();
        self.journal_log_count = logs.len();
        for log in new_logs {
            self.push_event(log);
        }
    }

    fn capture_callback_log<CTX: ContextTr>(&mut self, context: &CTX, log: Log) {
        self.push_event(log);
        // Precompile execution reports all of its logs after the frame returns,
        // so the journal may already contain more than the current callback.
        // Those entries are delivered by subsequent callbacks and must not be
        // copied again by `sync_journal`.
        self.journal_log_count = context.journal_ref().logs().len();
    }
}

impl<CTX> Inspector<CTX> for ArcEventInspector
where
    CTX: ContextTr,
{
    fn initialize_interp(&mut self, _interp: &mut Interpreter, context: &mut CTX) {
        self.sync_journal(context);
    }

    fn step_end(&mut self, _interp: &mut Interpreter, context: &mut CTX) {
        self.sync_journal(context);
    }

    fn log(&mut self, context: &mut CTX, log: Log) {
        self.capture_callback_log(context, log);
    }

    fn call(&mut self, context: &mut CTX, _inputs: &mut CallInputs) -> Option<CallOutcome> {
        self.sync_journal(context);
        self.push_frame();
        None
    }

    fn call_end(&mut self, context: &mut CTX, _inputs: &CallInputs, outcome: &mut CallOutcome) {
        self.sync_journal(context);
        self.finish_frame(outcome.instruction_result().is_ok());
    }

    fn create(&mut self, context: &mut CTX, _inputs: &mut CreateInputs) -> Option<CreateOutcome> {
        self.sync_journal(context);
        self.push_frame();
        None
    }

    fn create_end(
        &mut self,
        context: &mut CTX,
        _inputs: &CreateInputs,
        outcome: &mut CreateOutcome,
    ) {
        self.sync_journal(context);
        self.finish_frame(outcome.instruction_result().is_ok());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{Address, LogData, B256};
    use revm::interpreter::InstructionResult;
    use revm::{context::Context, MainContext};
    use revm_inspectors::tracing::{
        types::{CallKind, CallTrace, CallTraceNode, CallTraceStep},
        CallTraceArena,
    };

    use crate::debank_trace::build_debank_traces;

    fn log(byte: u8) -> Log {
        Log {
            address: Address::repeat_byte(byte),
            data: LogData::empty(),
        }
    }

    #[test]
    fn successful_logs_follow_emission_order_and_filter_failed_frames() {
        let mut inspector = ArcEventInspector::default();
        inspector.push_frame();
        inspector.push_event(log(1));
        inspector.push_frame();
        inspector.push_event(log(2));
        inspector.finish_frame(false);
        inspector.push_event(log(3));
        inspector.finish_frame(true);

        let captured = inspector.into_captured();
        assert!(captured.validate(2).is_ok());
        assert_eq!(captured.successful_logs(), vec![log(1), log(3)]);
    }

    #[test]
    fn incomplete_frame_stack_is_rejected() {
        let mut inspector = ArcEventInspector::default();
        inspector.push_frame();
        assert!(inspector.into_captured().validate(1).is_err());
    }

    #[test]
    fn journal_and_callback_logs_are_captured_once_and_keep_duplicates() {
        let mut context = Context::mainnet();
        let mut inspector = ArcEventInspector::default();
        let duplicate = log(1);
        let direct = log(2);
        inspector.push_frame();

        // Precompile callbacks arrive after all returned logs have already been
        // appended to the journal.
        context.journal_mut().log(duplicate.clone());
        context.journal_mut().log(duplicate.clone());
        inspector.capture_callback_log(&context, duplicate.clone());
        inspector.capture_callback_log(&context, duplicate.clone());
        inspector.sync_journal(&context);

        // Arc frame-init and SELFDESTRUCT logs have no inspector callback.
        context.journal_mut().log(direct.clone());
        inspector.sync_journal(&context);
        inspector.finish_frame(true);

        let captured = inspector.into_captured();
        assert_eq!(
            captured.successful_logs(),
            vec![duplicate.clone(), duplicate, direct]
        );
        assert_eq!(
            captured.frames[0].members,
            vec![
                CapturedMember::Event(0),
                CapturedMember::Event(1),
                CapturedMember::Event(2),
            ]
        );
    }

    #[test]
    fn builder_flattens_hidden_precompile_events_without_losing_order_or_emitter() {
        let mut arena = CallTraceArena::default();
        arena.nodes_mut()[0] = CallTraceNode {
            children: vec![2],
            trace: CallTrace {
                success: true,
                status: Some(InstructionResult::Return),
                kind: CallKind::Call,
                address: Address::repeat_byte(0x10),
                ..Default::default()
            },
            ..Default::default()
        };
        arena.nodes_mut().push(CallTraceNode {
            parent: Some(0),
            idx: 1,
            trace: CallTrace {
                success: true,
                status: Some(InstructionResult::Return),
                kind: CallKind::Call,
                address: Address::repeat_byte(0x20),
                maybe_precompile: Some(true),
                ..Default::default()
            },
            ..Default::default()
        });
        arena.nodes_mut().push(CallTraceNode {
            parent: Some(0),
            idx: 2,
            trace: CallTrace {
                success: false,
                status: Some(InstructionResult::Revert),
                kind: CallKind::Call,
                address: Address::repeat_byte(0x30),
                ..Default::default()
            },
            ..Default::default()
        });

        let logs = [log(1), log(2), log(3), log(4)];
        let captured = CapturedEvents {
            frames: vec![
                CapturedFrame {
                    parent: None,
                    members: vec![
                        CapturedMember::Event(0),
                        CapturedMember::Call(1),
                        CapturedMember::Call(2),
                        CapturedMember::Event(3),
                    ],
                    success: true,
                },
                CapturedFrame {
                    parent: Some(0),
                    members: vec![CapturedMember::Event(1)],
                    success: true,
                },
                CapturedFrame {
                    parent: Some(0),
                    members: vec![CapturedMember::Event(2)],
                    success: false,
                },
            ],
            events: logs
                .iter()
                .cloned()
                .enumerate()
                .map(|(frame, log)| CapturedEvent {
                    frame: [0, 1, 2, 0][frame],
                    log,
                })
                .collect(),
            valid: true,
        };

        let (traces, error_traces, events, error_events) = build_debank_traces(
            B256::repeat_byte(0xaa),
            arena,
            captured,
            &std::cell::RefCell::new(0),
        )
        .unwrap();

        assert_eq!(traces.len(), 1);
        assert_eq!(error_traces.len(), 1);
        assert_eq!(error_traces[0].trace_address, vec![0]);
        assert_eq!(error_traces[0].pos_in_parent_trace, 2);
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].contract_id, logs[0].address);
        assert_eq!(events[0].pos_in_parent_trace, 0);
        assert_eq!(events[1].contract_id, logs[1].address);
        assert_eq!(events[1].pos_in_parent_trace, 1);
        assert_eq!(events[2].contract_id, logs[3].address);
        assert_eq!(events[2].pos_in_parent_trace, 3);
        assert_eq!(
            events.iter().map(|event| event.idx).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        assert_eq!(error_events.len(), 1);
        assert_eq!(error_events[0].contract_id, logs[2].address);
        assert_eq!(error_events[0].idx, 0);
    }

    #[test]
    fn builder_puts_hidden_failure_before_event_and_selfdestruct() {
        let selfdestruct_address = Address::repeat_byte(0x11);
        let refund_target = Address::repeat_byte(0x22);
        let transferred_value = alloy_primitives::U256::from(3);
        let mut arena = CallTraceArena::default();
        arena.nodes_mut()[0] = CallTraceNode {
            trace: CallTrace {
                success: true,
                status: Some(InstructionResult::SelfDestruct),
                kind: CallKind::Call,
                address: Address::repeat_byte(0x10),
                selfdestruct_address: Some(selfdestruct_address),
                selfdestruct_refund_target: Some(refund_target),
                selfdestruct_transferred_value: Some(transferred_value),
                ..Default::default()
            },
            ..Default::default()
        };
        arena.nodes_mut().push(CallTraceNode {
            parent: Some(0),
            idx: 1,
            trace: CallTrace {
                success: false,
                status: Some(InstructionResult::Revert),
                kind: CallKind::Call,
                address: Address::repeat_byte(0x20),
                maybe_precompile: Some(true),
                ..Default::default()
            },
            ..Default::default()
        });

        let failed_log = log(1);
        let root_log = log(2);
        let captured = CapturedEvents {
            frames: vec![
                CapturedFrame {
                    parent: None,
                    members: vec![CapturedMember::Call(1), CapturedMember::Event(1)],
                    success: true,
                },
                CapturedFrame {
                    parent: Some(0),
                    members: vec![CapturedMember::Event(0)],
                    success: false,
                },
            ],
            events: vec![
                CapturedEvent {
                    frame: 1,
                    log: failed_log,
                },
                CapturedEvent {
                    frame: 0,
                    log: root_log,
                },
            ],
            valid: true,
        };

        let (traces, error_traces, events, error_events) = build_debank_traces(
            B256::repeat_byte(0xaa),
            arena,
            captured,
            &std::cell::RefCell::new(0),
        )
        .unwrap();

        assert!(error_traces.is_empty());
        assert_eq!(traces.len(), 2);
        assert_eq!(traces[0].subtraces, 1);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].pos_in_parent_trace, 1);
        assert_eq!(error_events.len(), 1);
        assert_eq!(error_events[0].pos_in_parent_trace, 0);
        assert_eq!(error_events[0].idx, 0);

        let suicide = &traces[1];
        assert_eq!(suicide.call_create_type, "suicide");
        assert_eq!(suicide.from_addr, selfdestruct_address);
        assert_eq!(suicide.to_addr, refund_target);
        assert_eq!(suicide.value, transferred_value);
        assert_eq!(suicide.pos_in_parent_trace, 2);
        assert_eq!(suicide.trace_address, vec![0]);
    }

    #[test]
    fn successful_child_storage_change_propagates_through_failed_parent() {
        let mut arena = CallTraceArena::default();
        arena.nodes_mut()[0] = CallTraceNode {
            children: vec![1],
            trace: CallTrace {
                success: false,
                status: Some(InstructionResult::Revert),
                kind: CallKind::Call,
                ..Default::default()
            },
            ..Default::default()
        };
        arena.nodes_mut().push(CallTraceNode {
            parent: Some(0),
            children: vec![2],
            idx: 1,
            trace: CallTrace {
                success: true,
                status: Some(InstructionResult::Return),
                kind: CallKind::Call,
                steps: vec![CallTraceStep {
                    pc: 0,
                    op: revm::bytecode::opcode::OpCode::SSTORE,
                    stack: None,
                    push_stack: None,
                    memory: None,
                    returndata: Default::default(),
                    gas_remaining: 0,
                    gas_refund_counter: 0,
                    gas_used: 0,
                    gas_cost: 0,
                    storage_change: None,
                    status: None,
                    immediate_bytes: None,
                    decoded: None,
                }],
                ..Default::default()
            },
            ..Default::default()
        });
        arena.nodes_mut().push(CallTraceNode {
            parent: Some(1),
            idx: 2,
            trace: CallTrace {
                success: false,
                status: Some(InstructionResult::OutOfGas),
                kind: CallKind::Call,
                ..Default::default()
            },
            ..Default::default()
        });
        let captured = CapturedEvents {
            frames: vec![
                CapturedFrame {
                    parent: None,
                    members: vec![CapturedMember::Call(1)],
                    success: false,
                },
                CapturedFrame {
                    parent: Some(0),
                    members: vec![CapturedMember::Call(2)],
                    success: true,
                },
                CapturedFrame {
                    parent: Some(1),
                    members: vec![],
                    success: false,
                },
            ],
            events: vec![],
            valid: true,
        };

        let (traces, error_traces, events, error_events) = build_debank_traces(
            B256::repeat_byte(0xaa),
            arena,
            captured,
            &std::cell::RefCell::new(0),
        )
        .unwrap();

        assert!(traces.is_empty());
        assert!(events.is_empty());
        assert!(error_events.is_empty());
        assert_eq!(error_traces.len(), 3);
        assert_eq!(error_traces[0].error, "Reverted");
        assert_eq!(error_traces[1].error, "parent call failed");
        assert_eq!(error_traces[2].error, "Out of gas");
        assert!(error_traces[0].storage_change);
        assert!(error_traces[1].self_storage_change);
    }

    #[test]
    fn selfdestruct_under_failed_parent_has_parent_error() {
        let refund_target = Address::repeat_byte(0x22);
        let mut arena = CallTraceArena::default();
        arena.nodes_mut()[0] = CallTraceNode {
            children: vec![1],
            trace: CallTrace {
                success: false,
                status: Some(InstructionResult::Revert),
                kind: CallKind::Call,
                ..Default::default()
            },
            ..Default::default()
        };
        arena.nodes_mut().push(CallTraceNode {
            parent: Some(0),
            idx: 1,
            trace: CallTrace {
                success: true,
                status: Some(InstructionResult::SelfDestruct),
                kind: CallKind::Call,
                selfdestruct_address: Some(Address::repeat_byte(0x11)),
                selfdestruct_refund_target: Some(refund_target),
                ..Default::default()
            },
            ..Default::default()
        });
        let captured = CapturedEvents {
            frames: vec![
                CapturedFrame {
                    parent: None,
                    members: vec![CapturedMember::Call(1)],
                    success: false,
                },
                CapturedFrame {
                    parent: Some(0),
                    members: vec![],
                    success: true,
                },
            ],
            events: vec![],
            valid: true,
        };

        let (traces, error_traces, events, error_events) = build_debank_traces(
            B256::repeat_byte(0xaa),
            arena,
            captured,
            &std::cell::RefCell::new(0),
        )
        .unwrap();

        assert!(traces.is_empty());
        assert!(events.is_empty());
        assert!(error_events.is_empty());
        assert_eq!(error_traces.len(), 3);
        let suicide = error_traces
            .iter()
            .find(|trace| trace.call_create_type == "suicide")
            .unwrap();
        assert_eq!(suicide.to_addr, refund_target);
        assert_eq!(suicide.error, "parent call failed");
    }
}
