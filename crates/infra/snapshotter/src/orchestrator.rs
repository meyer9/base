//! Orchestrates the full snapshot lifecycle with a restart safety guard.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use base_reth_cli::{ChunkFilename, ManifestGenerationParams, SnapshotGenerator};
use tracing::{error, info, warn};

use crate::{
    SnapshotterConfig,
    container::ContainerManager,
    tip::TipChecker,
    upload::{SnapshotUploader, StreamingS3ArchiveSink},
};

/// Orchestrates the full snapshot flow: optionally stop CL, stop EL → generate →
/// upload → restart EL, then optionally restart CL.
///
/// Containers stopped by this run are restarted even if snapshot generation or upload fails. When
/// a CL container is configured, it is stopped first and restarted last so it can reconnect to
/// the EL. This prevents leaving a stopped node behind without starting containers this run did
/// not stop.
pub struct Snapshotter<C: ContainerManager, T: TipChecker> {
    container_manager: C,
    tip_checker: T,
    uploader: SnapshotUploader,
    config: SnapshotterConfig,
}

impl<C: ContainerManager, T: TipChecker> std::fmt::Debug for Snapshotter<C, T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Snapshotter").field("config", &self.config).finish_non_exhaustive()
    }
}

impl<C: ContainerManager, T: TipChecker> Snapshotter<C, T> {
    /// Creates a new snapshotter with the given container manager, tip checker,
    /// and uploader.
    pub const fn new(
        container_manager: C,
        tip_checker: T,
        uploader: SnapshotUploader,
        config: SnapshotterConfig,
    ) -> Self {
        Self { container_manager, tip_checker, uploader, config }
    }

    /// Executes the full snapshot lifecycle.
    ///
    /// 0. Captures the EL's latest block and verifies it is at chain tip; skips the run if it is not
    /// 1. Stops the CL (when configured) then the EL
    /// 2. Verifies stopped containers are no longer running
    /// 3. Generates snapshot archives
    /// 4. Uploads to S3/R2
    /// 5. Clears reth's persisted peer list (best effort)
    /// 6. Restarts the EL and then the CL when configured, if this run stopped them
    pub async fn run(&self) -> Result<()> {
        // Only snapshot when the EL is caught up to tip. Snapshotting a lagging
        // node would publish stale data and pause a node that is still syncing.
        //
        // This is a best-effort PRE-check, not a guarantee of freshness at
        // snapshot time. There is an inherent TOCTOU gap: after this check
        // passes, time elapses while we stop the container, generate archives,
        // and upload — so a node that was "barely at tip" (e.g. 9s old with a
        // 10s threshold) may be stale by the time data is actually captured.
        // This is acceptable for the default 10s threshold on a 2s block-time
        // chain, but callers tightening the threshold should keep this in mind.
        let threshold = Duration::from_secs(self.config.tip_threshold_secs);
        let tip =
            self.tip_checker.check_tip(threshold).await.context("failed to check EL tip status")?;
        if !tip.at_tip {
            warn!(
                threshold_secs = self.config.tip_threshold_secs,
                "EL is not at tip; skipping snapshot run and leaving containers running"
            );
            return Ok(());
        }

        // Stop the dependent CL first when configured, then the EL. Restarting
        // in the reverse order below ensures the EL is available when the CL
        // reconnects.
        let (cl_stop_result, cl_stopped) =
            if let Some(ref cl_name) = self.config.consensus_container_name {
                match self.container_manager.stop(cl_name).await {
                    Ok(()) => (Ok(()), true),
                    Err(error) => (Err(error).context("failed to stop CL container"), false),
                }
            } else {
                (Ok(()), false)
            };
        let (result, el_stopped) = match cl_stop_result {
            Ok(()) => match self.container_manager.stop(&self.config.container_name).await {
                Ok(()) => (self.generate_and_upload(tip.block_number).await, true),
                Err(error) => (Err(error).context("failed to stop EL container"), false),
            },
            Err(error) => (Err(error), false),
        };

        // Clear reth's persisted peer list before the EL restarts so the node
        // rediscovers peers from bootnodes — an early-warning canary for peering
        // health. Best effort: a missing file or removal error is logged and
        // never aborts the run or blocks the restart.
        if el_stopped {
            self.clear_known_peers();
        }

        let el_restart_result = if el_stopped {
            self.container_manager.start(&self.config.container_name).await
        } else {
            Ok(())
        };

        if let Err(ref restart_err) = el_restart_result {
            error!(
                error = %restart_err,
                container = %self.config.container_name,
                "CRITICAL: failed to restart EL container after snapshot"
            );
        }

        let cl_restart_result = if cl_stopped {
            let Some(cl_name) = self.config.consensus_container_name.as_deref() else {
                unreachable!("only a configured CL can be stopped")
            };
            let cl_restart_result = if el_restart_result.is_ok() {
                self.container_manager.start(cl_name).await
            } else {
                warn!(container = %cl_name, "leaving CL stopped because EL restart failed");
                Ok(())
            };
            if let Err(ref restart_err) = cl_restart_result {
                error!(
                    error = %restart_err,
                    container = %cl_name,
                    "CRITICAL: failed to restart CL container after snapshot"
                );
            }
            cl_restart_result
        } else {
            Ok(())
        };

        let restart_result = match (el_restart_result, cl_restart_result) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(el_err), Ok(())) => Err(el_err).context("failed to restart EL container"),
            (Ok(()), Err(cl_err)) => Err(cl_err).context("failed to restart CL container"),
            (Err(el_err), Err(cl_err)) => {
                bail!("failed to restart EL container ({el_err}) and CL container ({cl_err})")
            }
        };

        match (result, restart_result) {
            (Ok(()), Ok(())) => {
                info!("snapshot lifecycle complete");
                Ok(())
            }
            (Err(snapshot_err), Ok(())) => {
                let restarted = if self.config.consensus_container_name.is_some() {
                    "snapshot failed but EL and CL containers were restarted"
                } else {
                    "snapshot failed but EL container was restarted"
                };
                Err(snapshot_err).context(restarted)
            }
            (Ok(()), Err(restart_err)) => {
                bail!(
                    "snapshot succeeded but container restart failed: {restart_err}. \
                     MANUAL INTERVENTION REQUIRED."
                )
            }
            (Err(snapshot_err), Err(restart_err)) => {
                bail!(
                    "snapshot failed ({snapshot_err}) AND container restart failed \
                     ({restart_err}). MANUAL INTERVENTION REQUIRED."
                )
            }
        }
    }

    /// Generates snapshot archives and uploads them. Separated from `run` so
    /// the restart guard logic stays clean.
    async fn generate_and_upload(&self, latest_block: u64) -> Result<()> {
        let run_timestamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();

        let remote_static_files = self.uploader.list_remote_static_files().await?;

        info!(remote_files = remote_static_files.len(), "fetched remote static file listing");

        let remote_manifest = self.uploader.fetch_previous_manifest().await?;
        info!(
            has_remote_manifest = remote_manifest.is_some(),
            "fetched previous manifest for blake3 diff"
        );

        let source_datadir = self.config.source_datadir.clone();
        let chain_id = self.config.chain_id;
        let block = self.config.block.unwrap_or(latest_block);
        let blocks_per_file = self.config.blocks_per_file;
        let remote_for_gen = remote_static_files.clone();
        let previous_manifest_for_gen = remote_manifest.clone();
        let upload_proofs = self.config.upload_proofs;
        let effective_block = block;
        let effective_blocks_per_file = blocks_per_file.unwrap_or(500_000);
        let latest_chunk_start = effective_block
            .saturating_sub(1)
            .checked_div(effective_blocks_per_file)
            .and_then(|index| index.checked_mul(effective_blocks_per_file))
            .context("latest static-file chunk range overflow")?;
        let key_uploader = self.uploader.clone();
        let sink = StreamingS3ArchiveSink::new(
            self.uploader.clone(),
            tokio::runtime::Handle::current(),
            self.config.max_streaming_archives.get(),
            move |archive_name| {
                let key = match ChunkFilename::parse(archive_name) {
                    Some((_component, start, _end)) if start != latest_chunk_start => {
                        key_uploader.static_file_object_key(archive_name)
                    }
                    _ => key_uploader.run_object_key(run_timestamp, archive_name),
                };
                Ok(key)
            },
        )?;

        let manifest = tokio::task::spawn_blocking(move || {
            let params = ManifestGenerationParams {
                source_datadir: &source_datadir,
                output_dir: None,
                chain_id,
                base_url: None,
                block: Some(block),
                blocks_per_file,
                remote_static_files: &remote_for_gen,
                previous_manifest: previous_manifest_for_gen.as_ref(),
                upload_proofs,
            };
            SnapshotGenerator::generate_manifest_with_sink(&params, &sink)
        })
        .await
        .context("snapshot generation task panicked")?
        .context("snapshot generation failed")?;

        self.uploader
            .publish_streamed_manifest(&manifest, run_timestamp, self.config.retain_runs.get())
            .await
            .with_context(|| {
                format!("failed to publish streamed snapshot manifest for {run_timestamp}")
            })?;

        Ok(())
    }

    /// Removes reth's persisted peer list (`known-peers.json`) from the datadir.
    ///
    /// Best effort: a missing file or removal error is logged and swallowed so
    /// it never aborts the snapshot run or blocks the EL restart.
    fn clear_known_peers(&self) {
        let known_peers = self.config.source_datadir.join("known-peers.json");
        match std::fs::remove_file(&known_peers) {
            Ok(()) => info!(path = %known_peers.display(), "cleared persisted peer list"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                error!(path = %known_peers.display(), "persisted peer list not found; nothing to clear")
            }
            Err(e) => {
                error!(error = %e, path = %known_peers.display(), "failed to clear persisted peer list")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    //! Orchestrator transition tests use a hand-rolled container manager because the observable
    //! contract is the ordered log across its `stop` and `start` methods.

    use std::sync::Mutex;

    use anyhow::{Result, bail};
    use async_trait::async_trait;
    use aws_config::BehaviorVersion;
    use aws_sdk_s3::config::Credentials;
    use clap::Parser;

    use super::Snapshotter;
    use crate::{ContainerManager, SnapshotUploader, SnapshotterConfig, TipChecker, TipStatus};

    struct ElStopFailureManager {
        calls: Mutex<Vec<String>>,
    }

    impl ElStopFailureManager {
        const fn new() -> Self {
            Self { calls: Mutex::new(Vec::new()) }
        }

        fn calls(&self) -> Vec<String> {
            self.calls.lock().expect("call log lock should not be poisoned").clone()
        }
    }

    #[async_trait]
    impl ContainerManager for ElStopFailureManager {
        async fn stop(&self, container_name: &str) -> Result<()> {
            self.calls
                .lock()
                .expect("call log lock should not be poisoned")
                .push(format!("stop:{container_name}"));
            if container_name == "el" {
                bail!("EL stop failed")
            }
            Ok(())
        }

        async fn start(&self, container_name: &str) -> Result<()> {
            self.calls
                .lock()
                .expect("call log lock should not be poisoned")
                .push(format!("start:{container_name}"));
            Ok(())
        }

        async fn is_running(&self, _container_name: &str) -> Result<bool> {
            Ok(true)
        }
    }

    struct AtTipChecker;

    #[async_trait]
    impl TipChecker for AtTipChecker {
        async fn check_tip(&self, _threshold: std::time::Duration) -> Result<TipStatus> {
            Ok(TipStatus { block_number: 1, at_tip: true })
        }
    }

    #[derive(Parser)]
    struct TestArgs {
        #[command(flatten)]
        config: SnapshotterConfig,
    }

    fn test_config() -> SnapshotterConfig {
        TestArgs::parse_from([
            "snapshotter-test",
            "--container-name",
            "el",
            "--consensus-container-name",
            "cl",
            "--el-rpc-url",
            "http://127.0.0.1:8545",
            "--source-datadir",
            "/unused",
            "--bucket",
            "unused",
        ])
        .config
    }

    async fn test_uploader() -> SnapshotUploader {
        let config = aws_config::defaults(BehaviorVersion::latest())
            .region("us-east-1")
            .credentials_provider(Credentials::new("test", "test", None, None, "test"))
            .load()
            .await;
        SnapshotUploader::new(
            aws_sdk_s3::Client::from_conf(aws_sdk_s3::config::Builder::from(&config).build()),
            "unused".to_string(),
            "unused".to_string(),
            None,
        )
    }

    #[tokio::test]
    async fn el_stop_failure_restores_only_the_stopped_cl() {
        let snapshotter = Snapshotter::new(
            ElStopFailureManager::new(),
            AtTipChecker,
            test_uploader().await,
            test_config(),
        );

        let error = snapshotter.run().await.expect_err("EL stop should fail");

        assert!(format!("{error:#}").contains("failed to stop EL container"));
        assert_eq!(
            snapshotter.container_manager.calls(),
            ["stop:cl", "stop:el", "start:cl"],
            "the recovery transition must not start the EL because this run did not stop it"
        );
    }
}
