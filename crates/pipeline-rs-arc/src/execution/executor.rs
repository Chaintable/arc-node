use std::cell::RefCell;

use alloy_consensus::{transaction::TxHashRef, Transaction, TxReceipt};
use alloy_evm::{
    block::{
        BlockExecutionError, BlockExecutionResult, BlockExecutor, ExecutableTx, GasOutput,
        OnStateHook, TxResult,
    },
    Evm, FromRecoveredTx, FromTxWithEncoded, RecoveredTx,
};
use alloy_primitives::{Log, B256};
use revm::context::Block as _;

use super::{
    append_traces,
    debank::{build_debank_traces, BlockFile, DebankTransaction},
    debank_transaction_target, transaction_metadata, CaptureEvm, CaptureInspector,
};

type FinishCapture<'a> = Box<dyn FnOnce(Result<BlockFile, String>) + 'a>;

pub struct WriterExecutor<'a, B: BlockExecutor> {
    inner: B,
    capture: Option<FinishCapture<'a>>,
    projection: BlockFile,
    capture_error: Option<String>,
    pending: Option<(B256, DebankTransaction)>,
    log_index: RefCell<usize>,
}

impl<'a, B: BlockExecutor> WriterExecutor<'a, B> {
    pub fn new(inner: B) -> Self {
        Self {
            inner,
            capture: None,
            projection: BlockFile::default(),
            capture_error: None,
            pending: None,
            log_index: RefCell::new(0),
        }
    }

    pub fn set_capture(&mut self, capture: impl FnOnce(Result<BlockFile, String>) + 'a) {
        self.capture = Some(Box::new(capture));
    }
}

impl<E, I, B> BlockExecutor for WriterExecutor<'_, B>
where
    E: Evm<Inspector = CaptureInspector<I>>,
    E::Tx: FromRecoveredTx<B::Transaction> + FromTxWithEncoded<B::Transaction>,
    B: BlockExecutor<
        Evm = CaptureEvm<E, I>,
        Transaction: Transaction + TxHashRef,
        Receipt: TxReceipt<Log = Log>,
    >,
{
    type Transaction = B::Transaction;
    type Receipt = B::Receipt;
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
            let hash = *tx.tx_hash();
            let mut artifact = transaction_metadata(
                tx,
                hash,
                *recovered.signer(),
                tx.effective_gas_price(Some(self.inner.evm().block().basefee())),
                self.projection.transactions.len() as u64,
            );
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
                    let traced =
                        build_debank_traces(hash, tracing.into_traces(), events, &self.log_index)?;
                    append_traces(&mut self.projection, tx.status, traced)
                        .map_err(str::to_owned)?;
                    self.projection.transactions.push(tx);
                    Ok(())
                })();
                if let Err(err) = projection {
                    self.capture_error = Some(format!("transaction {hash}: {err}"));
                }
            }
        }
        gas
    }

    fn finish(
        mut self,
    ) -> Result<(Self::Evm, BlockExecutionResult<Self::Receipt>), BlockExecutionError> {
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
            capture(projection);
        }
        Ok((evm, result))
    }

    fn receipts(&self) -> &[Self::Receipt] {
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
