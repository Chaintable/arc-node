//! Writer adapter for Reth's two formal engine validation entry points.

use std::{fmt, sync::Arc};

use alloy_consensus::{BlockHeader, Header};
use alloy_primitives::B256;
use alloy_rpc_types_engine::ExecutionData;
use arc_evm::writer::WriterEvmConfig;
use arc_evm::ArcEvmConfig;
use arc_execution_config::chainspec::ArcChainSpec;
use pipeline_rs_arc::{execution::LiveCapture, CaptureSink};
use reth_chain_state::ExecutedBlock;
use reth_engine_tree::tree::{
    payload_processor::multiproof::StateRootHandle,
    payload_validator::{TreeCtx, ValidationOutcome},
    CacheWaitDurations, EngineApiTreeState, EngineValidator, SavedCache, TreeConfig, WaitForCaches,
};
use reth_ethereum::{node::EthEngineTypes, Block};
use reth_ethereum_engine_primitives::EthPayloadAttributes;
use reth_ethereum_primitives::EthPrimitives;
use reth_node_api::{AddOnsContext, FullNodeComponents, NodeTypes};
use reth_node_builder::{
    components::{Components, NodeComponents},
    rpc::{BasicEngineValidatorBuilder, EngineValidatorBuilder},
    NodeAdapter,
};
use reth_payload_primitives::{InvalidPayloadAttributesError, NewPayloadError};
use reth_primitives_traits::SealedBlock;
use reth_provider::HeaderProvider;
use reth_trie_db::ChangesetCache;

use crate::node::ArcEngineValidatorBuilder;

mod delivery;
pub use delivery::start_delivery;
pub use pipeline_rs_arc::{Delivery, DeliveryConfig};

#[derive(Clone)]
pub struct WriterValidatorBuilder {
    sink: Arc<dyn CaptureSink>,
}

impl fmt::Debug for WriterValidatorBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WriterValidatorBuilder")
            .finish_non_exhaustive()
    }
}

impl WriterValidatorBuilder {
    pub fn new(sink: Arc<dyn CaptureSink>) -> Self {
        Self { sink }
    }
}

type WriterComponents<N> = Components<
    N,
    <N as FullNodeComponents>::Network,
    <N as FullNodeComponents>::Pool,
    WriterEvmConfig,
    <N as FullNodeComponents>::Consensus,
>;
type WriterNode<N> = NodeAdapter<N, WriterComponents<N>>;
type ArcValidatorBuilder = BasicEngineValidatorBuilder<ArcEngineValidatorBuilder>;

impl<N> EngineValidatorBuilder<N> for WriterValidatorBuilder
where
    N: FullNodeComponents<
        Types: NodeTypes<
            ChainSpec = ArcChainSpec,
            Payload = EthEngineTypes,
            Primitives = EthPrimitives,
        >,
        Evm = ArcEvmConfig,
    >,
    WriterComponents<N>: NodeComponents<N, Evm = WriterEvmConfig>,
{
    type EngineValidator = WriterValidator<
        <ArcValidatorBuilder as EngineValidatorBuilder<WriterNode<N>>>::EngineValidator,
        N::Provider,
    >;

    async fn build_tree_validator(
        self,
        ctx: &AddOnsContext<'_, N>,
        tree_config: TreeConfig,
        changeset_cache: ChangesetCache,
    ) -> eyre::Result<Self::EngineValidator> {
        let node = &ctx.node;
        let capture = LiveCapture::default();
        let provider = node.provider().clone();
        // Use the capture EVM only in the native tree validator builder's context.
        let writer_ctx = AddOnsContext {
            node: NodeAdapter {
                components: Components {
                    transaction_pool: node.pool().clone(),
                    evm_config: WriterEvmConfig::live(node.evm_config().clone(), capture.clone()),
                    consensus: node.consensus().clone(),
                    network: node.network().clone(),
                    payload_builder_handle: node.payload_builder_handle().clone(),
                },
                task_executor: node.task_executor().clone(),
                provider: provider.clone(),
            },
            config: ctx.config,
            beacon_engine_handle: ctx.beacon_engine_handle.clone(),
            engine_events: ctx.engine_events.clone(),
            jwt_secret: ctx.jwt_secret,
        };
        let inner = ArcValidatorBuilder::default()
            .build_tree_validator(&writer_ctx, tree_config, changeset_cache)
            .await?;
        Ok(WriterValidator {
            inner,
            provider,
            capture,
            sink: self.sink,
        })
    }
}

pub struct WriterValidator<V, P> {
    inner: V,
    provider: P,
    capture: LiveCapture,
    sink: Arc<dyn CaptureSink>,
}

impl<V, P: HeaderProvider<Header = Header>> WriterValidator<V, P> {
    fn finish_validation(
        &self,
        outcome: ValidationOutcome<EthPrimitives>,
        parent_state_root: Option<B256>,
    ) -> ValidationOutcome<EthPrimitives> {
        match &outcome {
            Ok((executed, _)) => {
                let block = executed.sealed_block();
                let parent_state_root = parent_state_root.unwrap_or_else(|| {
                    self.provider
                        .header(block.parent_hash())
                        .unwrap_or_else(|err| {
                            panic!("cannot read Arc Writer parent of {}: {err}", block.hash())
                        })
                        .unwrap_or_else(|| panic!("missing Arc Writer parent of {}", block.hash()))
                        .state_root
                });
                let result = self.capture.accept(
                    block.header(),
                    block.hash(),
                    parent_state_root,
                    executed.execution_outcome().state.clone(),
                    self.sink.as_ref(),
                );
                if let Err(err) = result {
                    panic!(
                        "Arc Writer live capture failed at {} {}: {err}",
                        block.number(),
                        block.hash()
                    );
                }
            }
            Err(_) => self.capture.discard(),
        }
        outcome
    }
}

fn in_memory_parent_root(ctx: &TreeCtx<'_, EthPrimitives>, parent: B256) -> Option<B256> {
    ctx.state()
        .tree_state()
        .sealed_header_by_hash(&parent)
        .map(|header| header.state_root)
}

impl<V, P> EngineValidator<EthEngineTypes> for WriterValidator<V, P>
where
    V: EngineValidator<EthEngineTypes>,
    P: HeaderProvider<Header = Header> + Send + Sync + 'static,
{
    fn validate_payload_attributes_against_header(
        &self,
        attr: &EthPayloadAttributes,
        header: &Header,
    ) -> Result<(), InvalidPayloadAttributesError> {
        self.inner
            .validate_payload_attributes_against_header(attr, header)
    }

    fn convert_payload_to_block(
        &self,
        payload: ExecutionData,
    ) -> Result<SealedBlock<Block>, NewPayloadError> {
        self.inner.convert_payload_to_block(payload)
    }

    fn validate_payload(
        &mut self,
        payload: ExecutionData,
        ctx: TreeCtx<'_, EthPrimitives>,
    ) -> ValidationOutcome<EthPrimitives> {
        let parent = in_memory_parent_root(&ctx, payload.payload.parent_hash());
        let outcome = self.inner.validate_payload(payload, ctx);
        self.finish_validation(outcome, parent)
    }

    fn validate_block(
        &mut self,
        block: SealedBlock<Block>,
        ctx: TreeCtx<'_, EthPrimitives>,
    ) -> ValidationOutcome<EthPrimitives> {
        let parent = in_memory_parent_root(&ctx, block.parent_hash());
        let outcome = self.inner.validate_block(block, ctx);
        self.finish_validation(outcome, parent)
    }

    fn on_inserted_executed_block(&self, block: ExecutedBlock<EthPrimitives>) {
        self.inner.on_inserted_executed_block(block);
    }

    fn cache_for(&self, hash: B256) -> Option<SavedCache> {
        self.inner.cache_for(hash)
    }

    fn sparse_trie_handle_for(
        &self,
        parent: B256,
        root: B256,
        state: &EngineApiTreeState<EthPrimitives>,
    ) -> Option<StateRootHandle> {
        self.inner.sparse_trie_handle_for(parent, root, state)
    }
}

impl<V: WaitForCaches, P> WaitForCaches for WriterValidator<V, P> {
    fn wait_for_caches(&self) -> CacheWaitDurations {
        self.inner.wait_for_caches()
    }
}
