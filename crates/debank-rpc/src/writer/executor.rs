use std::cell::RefCell;

use alloy_consensus::{Transaction, TxReceipt};
use alloy_evm::{
    block::{BlockExecutionResult, BlockExecutor, ExecutableTx, GasOutput, OnStateHook, TxResult},
    eth::EthEvmContext,
    Database, Evm, RecoveredTx,
};
use alloy_primitives::B256;
use reth_ethereum_primitives::{Receipt, TransactionSigned};
use reth_evm::execute::BlockExecutionError;
use revm::inspector::Inspector;

use super::evm::WriterEvm;
use crate::{
    debank_trace::{build_debank_traces, BlockFile, DebankTransaction},
    trace_block::{debank_transaction_target, validate_transaction_classification},
};

pub(super) type FinishCapture<'a, E> = Box<dyn FnOnce(&E, Result<BlockFile, String>) + 'a>;

pub struct WriterExecutor<'a, B: BlockExecutor> {
    pub(super) inner: B,
    pub(super) capture: Option<FinishCapture<'a, B::Evm>>,
    pub(super) projection: BlockFile,
    pub(super) capture_error: Option<String>,
    pub(super) pending: Option<(B256, DebankTransaction)>,
    pub(super) log_index: RefCell<usize>,
}

impl<DB, I, B> BlockExecutor for WriterExecutor<'_, B>
where
    DB: Database,
    I: Inspector<EthEvmContext<DB>>,
    B: BlockExecutor<Evm = WriterEvm<DB, I>, Transaction = TransactionSigned, Receipt = Receipt>,
{
    type Transaction = TransactionSigned;
    type Receipt = Receipt;
    type Evm = B::Evm;
    type Result = B::Result;

    fn apply_pre_execution_changes(&mut self) -> Result<(), BlockExecutionError> {
        self.inner.apply_pre_execution_changes()
    }

    fn execute_transaction_without_commit(
        &mut self,
        tx: impl ExecutableTx<Self>,
    ) -> Result<Self::Result, BlockExecutionError> {
        let (env, recovered) = tx.into_parts();
        self.pending = None;
        if self.capture.is_some() {
            let tx = recovered.tx();
            let mut artifact = DebankTransaction {
                id: tx.tx_hash().to_string(),
                from: *recovered.signer(),
                gas_limit: tx.gas_limit(),
                gas_price: tx.effective_gas_price(Some(self.inner.evm().block().basefee)),
                gas_fee_cap: tx.max_fee_per_gas(),
                gas_tip_cap: tx.max_priority_fee_per_gas().unwrap_or_default(),
                input: tx.input().clone(),
                nonce: tx.nonce(),
                transaction_index: self.projection.transactions.len() as u64,
                value: tx.value(),
                ..Default::default()
            };
            let hash = *tx.tx_hash();
            let recipient = tx.to();
            self.inner.evm_mut().start_capture();
            match self
                .inner
                .execute_transaction_without_commit((env, recovered))
            {
                Ok(output) => {
                    let result = &output.result().result;
                    artifact.to = debank_transaction_target(recipient, result.created_address());
                    self.pending = Some((hash, artifact));
                    Ok(output)
                }
                Err(error) => {
                    self.inner.evm_mut().finish_capture();
                    Err(error)
                }
            }
        } else {
            self.inner
                .execute_transaction_without_commit((env, recovered))
        }
    }

    fn commit_transaction(&mut self, output: Self::Result) -> GasOutput {
        let gas = self.inner.commit_transaction(output);
        if let Some((hash, mut tx)) = self.pending.take() {
            let (tracing, events) = self
                .inner
                .evm_mut()
                .finish_capture()
                .expect("executed Writer transaction must have an active inspector");
            let receipt = self
                .inner
                .receipts()
                .last()
                .expect("Arc commit must append a receipt");
            tx.status = receipt.status();
            tx.gas_used = gas.tx_gas_used();
            let events = events.into_captured();
            if self.capture_error.is_none() {
                let projection = (|| {
                    if events.successful_logs() != receipt.logs() {
                        return Err("inspector logs differ from the receipt".to_owned());
                    }
                    let (traces, error_traces, events, error_events) =
                        build_debank_traces(hash, tracing.into_traces(), events, &self.log_index)?;
                    validate_transaction_classification(
                        tx.status,
                        &traces,
                        &error_traces,
                        &events,
                        &error_events,
                    )
                    .map_err(str::to_owned)?;
                    self.projection.transactions.push(tx);
                    self.projection.traces.extend(traces);
                    self.projection.error_traces.extend(error_traces);
                    self.projection.events.extend(events);
                    self.projection.error_events.extend(error_events);
                    Ok(())
                })();
                if let Err(err) = projection {
                    self.capture_error = Some(format!("transaction {hash}: {err}"));
                }
            }
        }
        gas
    }

    fn finish(mut self) -> Result<(Self::Evm, BlockExecutionResult<Receipt>), BlockExecutionError> {
        self.inner.evm_mut().finish_capture();
        let (evm, result) = self.inner.finish()?;
        if let Some(capture) = self.capture {
            let projection = match self.capture_error {
                Some(err) => Err(err),
                None if self.projection.transactions.len() != result.receipts.len() => {
                    Err("captured transaction count differs from receipt count".into())
                }
                None => Ok(self.projection),
            };
            capture(&evm, projection);
        }
        Ok((evm, result))
    }

    fn receipts(&self) -> &[Receipt] {
        self.inner.receipts()
    }

    fn set_state_hook(&mut self, hook: Option<Box<dyn OnStateHook>>) {
        self.inner.set_state_hook(hook);
    }

    fn evm(&self) -> &Self::Evm {
        self.inner.evm()
    }

    fn evm_mut(&mut self) -> &mut Self::Evm {
        self.inner.evm_mut()
    }
}
