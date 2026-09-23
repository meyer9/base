//! Implements the rollup client rpc endpoints. These endpoints serve data about the rollup state.
//!
//! The method names remain compatible with the legacy rollup RPC namespace.

use std::{
    fmt::Debug,
    future::Future,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use alloy_eips::BlockNumberOrTag;
use async_trait::async_trait;
use base_common_genesis::RollupConfig;
use base_consensus_engine::EngineState;
use base_consensus_gossip::Metrics;
use base_consensus_safedb::{SafeDBError, SafeDBReader, SafeHeadResponse};
use base_protocol::SyncStatus;
use jsonrpsee::{
    core::RpcResult,
    types::{ErrorCode, ErrorObject},
};
use tracing::Instrument;

use crate::{
    EngineRpcClient, L1State, L1WatcherQueries, OutputResponse, RollupNodeApiServer,
    l1_watcher::L1WatcherQuerySender,
};

static RPC_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

/// `RollupRpc`
///
/// This is a server implementation of [`crate::RollupNodeApiServer`].
#[derive(Debug)]
pub struct RollupRpc<EngineRpcClient_> {
    /// The channel to send [`base_consensus_engine::EngineQueries`]s.
    pub engine_client: EngineRpcClient_,
    /// The channel to send [`crate::L1WatcherQueries`]s.
    pub l1_watcher_sender: L1WatcherQuerySender,
    /// Reader for safe head lookups by L1 block number.
    pub safe_db_reader: Arc<dyn SafeDBReader>,
    /// Maximum time an RPC call can wait for an internal actor response.
    pub request_timeout: Duration,
}

impl<EngineRpcClient_: EngineRpcClient> RollupRpc<EngineRpcClient_> {
    /// Constructs a new [`RollupRpc`] given a sender channel.
    pub fn new(
        engine_client: EngineRpcClient_,
        l1_watcher_sender: L1WatcherQuerySender,
        safe_db_reader: Arc<dyn SafeDBReader>,
        request_timeout: Duration,
    ) -> Self {
        Self { engine_client, l1_watcher_sender, safe_db_reader, request_timeout }
    }

    pub async fn await_internal<T, Request>(
        &self,
        operation: &'static str,
        request: Request,
    ) -> RpcResult<T>
    where
        Request: Future<Output = RpcResult<T>>,
    {
        tokio::time::timeout(self.request_timeout, request).await.map_err(|_| {
            warn!(
                target: "rpc",
                operation,
                timeout_ms = self.request_timeout.as_millis() as u64,
                "Timed out waiting for an internal RPC dependency"
            );
            ErrorObject::owned(
                ErrorCode::InternalError.code(),
                "Timed out waiting for an internal node component",
                None::<()>,
            )
        })?
    }

    pub async fn l1_state(&self) -> RpcResult<L1State> {
        let (sender, receiver) = tokio::sync::oneshot::channel();

        self.await_internal("L1 watcher state", async {
            self.l1_watcher_sender
                .send(L1WatcherQueries::L1State(sender))
                .await
                .map_err(|_| ErrorObject::from(ErrorCode::InternalError))?;
            receiver.await.map_err(|_| ErrorObject::from(ErrorCode::InternalError))
        })
        .await
    }

    // Important note: we zero-out the fields that can't be derived yet to follow the reference node's
    // behaviour.
    fn sync_status_from_actor_queries(
        l1_sync_status: L1State,
        l2_sync_status: EngineState,
    ) -> SyncStatus {
        SyncStatus {
            current_l1: l1_sync_status.current_l1.unwrap_or_default(),
            current_l1_finalized: l1_sync_status.current_l1_finalized.unwrap_or_default(),
            head_l1: l1_sync_status.head_l1.unwrap_or_default(),
            safe_l1: l1_sync_status.safe_l1.unwrap_or_default(),
            finalized_l1: l1_sync_status.finalized_l1.unwrap_or_default(),
            unsafe_l2: l2_sync_status.sync_state.unsafe_head(),
            local_safe_l2: l2_sync_status.sync_state.local_safe_head(),
            safe_l2: l2_sync_status.sync_state.safe_head(),
            finalized_l2: l2_sync_status.sync_state.finalized_head(),
        }
    }
}

#[async_trait]
impl<EngineRpcClient_: EngineRpcClient + 'static> RollupNodeApiServer
    for RollupRpc<EngineRpcClient_>
{
    async fn output_at_block(&self, block_num: BlockNumberOrTag) -> RpcResult<OutputResponse> {
        const RPC_METHOD: &str = "optimism_outputAtBlock";

        Metrics::rpc_calls("base_outputAtBlock").increment(1.0);

        let request_id = RPC_REQUEST_ID.fetch_add(1, Ordering::Relaxed);
        let request_started_at = Instant::now();
        let span = info_span!(
            target: "rpc",
            "rpc_request",
            request_id,
            rpc_method = RPC_METHOD,
            block = ?block_num,
        );

        info!(target: "rpc", request_id, rpc_method = RPC_METHOD, block = ?block_num, "Started rollup RPC request");

        let ((l2_block_info, output_root, l2_sync_status), l1_sync_status) = tokio::try_join!(
            self.await_internal("engine output", self.engine_client.output_at_block(block_num))
                .instrument(span.clone()),
            self.l1_state().instrument(span.clone())
        )
        .map_err(|error| {
            warn!(
                target: "rpc",
                request_id,
                rpc_method = RPC_METHOD,
                block = ?block_num,
                elapsed_ms = request_started_at.elapsed().as_millis() as u64,
                error = ?error,
                "Rollup RPC request failed"
            );
            error
        })?;

        let sync_status = Self::sync_status_from_actor_queries(l1_sync_status, l2_sync_status);

        info!(
            target: "rpc",
            request_id,
            rpc_method = RPC_METHOD,
            block = ?block_num,
            elapsed_ms = request_started_at.elapsed().as_millis() as u64,
            "Completed rollup RPC request"
        );

        Ok(OutputResponse::from_v0(output_root, sync_status, l2_block_info))
    }

    async fn safe_head_at_l1_block(
        &self,
        block_num: BlockNumberOrTag,
    ) -> RpcResult<SafeHeadResponse> {
        Metrics::rpc_calls("base_safeHeadAtL1Block").increment(1.0);

        let number = match block_num {
            BlockNumberOrTag::Number(n) => n,
            _ => {
                return Err(ErrorObject::owned(
                    -32602,
                    "optimism_safeHeadAtL1Block requires an explicit block number, not latest/earliest/pending",
                    None::<()>,
                ));
            }
        };

        self.safe_db_reader.safe_head_at_l1(number).await.map_err(|e| match e {
            SafeDBError::NotFound => ErrorObject::owned(-32000, "safe head not found", None::<()>),
            SafeDBError::Disabled => ErrorObject::owned(
                -32000,
                "safe head tracking is disabled on this node",
                None::<()>,
            ),
            SafeDBError::Database(_) => {
                error!(target: "rpc", error = %e, "safedb query failed");
                ErrorObject::from(ErrorCode::InternalError)
            }
        })
    }

    async fn sync_status(&self) -> RpcResult<SyncStatus> {
        const RPC_METHOD: &str = "optimism_syncStatus";

        Metrics::rpc_calls("base_syncStatus").increment(1.0);

        let request_id = RPC_REQUEST_ID.fetch_add(1, Ordering::Relaxed);
        let request_started_at = Instant::now();
        let span = info_span!(
            target: "rpc",
            "rpc_request",
            request_id,
            rpc_method = RPC_METHOD,
        );

        debug!(target: "rpc", request_id, rpc_method = RPC_METHOD, "Started rollup RPC request");

        let (l1_sync_status, l2_sync_status) = tokio::try_join!(
            self.l1_state().instrument(span.clone()),
            self.await_internal("engine state", self.engine_client.get_state())
                .instrument(span.clone())
        )
        .map_err(|error| {
            warn!(
                target: "rpc",
                request_id,
                rpc_method = RPC_METHOD,
                elapsed_ms = request_started_at.elapsed().as_millis() as u64,
                error = ?error,
                "Rollup RPC request failed"
            );
            ErrorObject::from(ErrorCode::InternalError)
        })?;

        debug!(
            target: "rpc",
            request_id,
            rpc_method = RPC_METHOD,
            elapsed_ms = request_started_at.elapsed().as_millis() as u64,
            "Completed rollup RPC request"
        );

        Ok(Self::sync_status_from_actor_queries(l1_sync_status, l2_sync_status))
    }

    async fn rollup_config(&self) -> RpcResult<RollupConfig> {
        Metrics::rpc_calls("base_rollupConfig").increment(1.0);

        self.engine_client.get_config().await
    }

    async fn version(&self) -> RpcResult<String> {
        Metrics::rpc_calls("base_version").increment(1.0);

        const RPC_VERSION: &str = env!("CARGO_PKG_VERSION");

        return Ok(RPC_VERSION.to_string());
    }
}

#[cfg(test)]
mod tests {
    //! The `EngineRpcClient` double is hand-rolled because `automock` cannot
    //! implement its required `Clone` supertrait.

    use std::{future::pending, sync::Arc, time::Duration};

    use alloy_eips::BlockNumberOrTag;
    use alloy_primitives::B256;
    use async_trait::async_trait;
    use base_common_genesis::RollupConfig;
    use base_consensus_engine::EngineState;
    use base_consensus_safedb::DisabledSafeDB;
    use base_protocol::{L2BlockInfo, OutputRoot};
    use jsonrpsee::{core::RpcResult, types::ErrorCode};
    use tokio::sync::{mpsc, watch};

    use super::RollupRpc;
    use crate::{EngineRpcClient, RollupNodeApiServer};

    #[derive(Clone, Debug)]
    struct ResponsiveEngine;

    #[async_trait]
    impl EngineRpcClient for ResponsiveEngine {
        async fn get_config(&self) -> RpcResult<RollupConfig> {
            Ok(RollupConfig::default())
        }

        async fn get_state(&self) -> RpcResult<EngineState> {
            Ok(EngineState::default())
        }

        async fn output_at_block(
            &self,
            _: BlockNumberOrTag,
        ) -> RpcResult<(L2BlockInfo, OutputRoot, EngineState)> {
            Ok((
                L2BlockInfo::default(),
                OutputRoot::from_parts(B256::ZERO, B256::ZERO, B256::ZERO),
                EngineState::default(),
            ))
        }

        async fn dev_get_task_queue_length(&self) -> RpcResult<usize> {
            Ok(0)
        }

        async fn dev_subscribe_to_engine_queue_length(&self) -> RpcResult<watch::Receiver<usize>> {
            let (_, receiver) = watch::channel(0);
            Ok(receiver)
        }

        async fn dev_subscribe_to_engine_state(&self) -> RpcResult<watch::Receiver<EngineState>> {
            let (_, receiver) = watch::channel(EngineState::default());
            Ok(receiver)
        }
    }

    fn rpc_with_unresponsive_l1_watcher() -> RollupRpc<ResponsiveEngine> {
        let (sender, receiver) = mpsc::channel(1);
        tokio::spawn(async move {
            let _receiver = receiver;
            pending::<()>().await;
        });
        RollupRpc::new(ResponsiveEngine, sender, Arc::new(DisabledSafeDB), Duration::from_millis(1))
    }

    #[tokio::test]
    async fn sync_status_times_out_when_l1_watcher_does_not_reply() {
        let error = rpc_with_unresponsive_l1_watcher().sync_status().await.unwrap_err();

        assert_eq!(error.code(), ErrorCode::InternalError.code());
    }

    #[tokio::test]
    async fn output_at_block_times_out_when_l1_watcher_does_not_reply() {
        let error = rpc_with_unresponsive_l1_watcher()
            .output_at_block(BlockNumberOrTag::Number(1))
            .await
            .unwrap_err();

        assert_eq!(error.code(), ErrorCode::InternalError.code());
        assert_eq!(error.message(), "Timed out waiting for an internal node component");
    }
}
