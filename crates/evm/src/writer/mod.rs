//! Connects the capture library to native Arc execution.

mod config;
mod evm;
mod subcall;
pub use subcall::record_subcall_trace_completion;

pub use config::WriterEvmConfig;
