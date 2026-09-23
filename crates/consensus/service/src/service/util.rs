//! Utilities for the rollup node service, internal to the crate.

use tracing::info;

/// Spawns a set of parallel actors in a [`JoinSet`], and cancels all actors if any of them fail. The
/// type of the error in the [`NodeActor`]s is erased to avoid having to specify a common error type
/// between actors.
///
/// Actors are passed in as optional arguments, in case a given actor is not needed.
///
/// This macro also handles OS shutdown signals (SIGTERM, SIGINT) and triggers graceful shutdown
/// when received.
///
/// [JoinSet]: tokio::task::JoinSet
/// [NodeActor]: crate::NodeActor
macro_rules! spawn_and_wait {
    ($cancellation:expr, actors = [$($actor:expr$(,)?)*]) => {
        use tracing::{error, info};
        let mut task_handles = tokio::task::JoinSet::new();

        // Check if the actor is present, and spawn it if it is.
        $(
            if let Some((actor, context)) = $actor {
                let cancellation = $cancellation.clone();
                task_handles.spawn(async move {
                    // This guard ensures that the cancellation token is cancelled when the actor is
                    // dropped. This ensures that the actor is properly shut down.
                    // Note the underscore prefix: this is to signal that we don't use the guard anywhere, but
                    // *the compiler shouldn't optimize it away*.
                    // Note that using a simple `_` would not work here because it gets optimized away in
                    // release mode.
                    let _guard = cancellation.drop_guard();

                    if let Err(e) = actor.start(context).await {
                        return Err(format!("{e:?}"));
                    }
                    Ok(())
                });
            }
        )*

        // Create the shutdown signal future. Keep a clone for the external cancellation
        // branch so callers can request the same graceful drain programmatically.
        let shutdown = $crate::ShutdownSignal::wait();
        tokio::pin!(shutdown);
        let shutdown_cancellation = $cancellation.clone();

        loop {
            tokio::select! {
                _ = &mut shutdown => {
                    info!(target: "rollup_node", "Received shutdown signal, initiating graceful shutdown...");
                    $cancellation.cancel();
                    break;
                }
                _ = shutdown_cancellation.cancelled() => {
                    info!(target: "rollup_node", "Cancellation requested, waiting for actors to stop...");
                    break;
                }
                result = task_handles.join_next() => {
                    match result {
                        Some(Ok(Ok(()))) => { /* Actor completed successfully */ }
                        Some(Ok(Err(e))) => {
                            error!(target: "rollup_node", error = %e, "Critical error in sub-routine");
                            // Cancel all tasks and gracefully shutdown.
                            $cancellation.cancel();
                            return Err(e);
                        }
                        Some(Err(e)) => {
                            let error_msg = format!("Task join error: {e}");
                            // Log the error and cancel all tasks.
                            error!(target: "rollup_node", error = %e, "Task join error");
                            // Cancel all tasks and gracefully shutdown.
                            $cancellation.cancel();
                            return Err(error_msg);
                        }
                        None => break, // All tasks completed
                    }
                }
            }
        }

        // Do not drop the JoinSet after a graceful shutdown request: dropping it aborts
        // remaining actors before they can flush their operator-owned state.
        while let Some(result) = task_handles.join_next().await {
            match result {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    error!(target: "rollup_node", error = %error, "Actor failed while shutting down");
                }
                Err(error) if error.is_cancelled() => {}
                Err(error) => {
                    error!(target: "rollup_node", error = %error, "Actor join failed while shutting down");
                }
            }
        }
    };
}

// Export the `spawn_and_wait` macro for use in other modules.
pub(crate) use spawn_and_wait;

/// Listens for OS shutdown signals (SIGTERM, SIGINT)
#[derive(Debug)]
pub struct ShutdownSignal;

impl ShutdownSignal {
    /// Waits for OS shutdown signals (SIGTERM, SIGINT).
    pub async fn wait() {
        let ctrl_c = async {
            tokio::signal::ctrl_c().await.expect("failed to install Ctrl+C handler");
        };

        #[cfg(unix)]
        let terminate = async {
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("failed to install SIGTERM handler")
                .recv()
                .await;
        };

        #[cfg(not(unix))]
        let terminate = std::future::pending::<()>();

        tokio::select! {
            _ = ctrl_c => {
                info!(target: "rollup_node", "Received SIGINT (Ctrl+C)");
            },
            _ = terminate => {
                info!(target: "rollup_node", "Received SIGTERM");
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use tokio::{
        sync::oneshot,
        time::{Duration, timeout},
    };
    use tokio_util::sync::CancellationToken;

    use crate::NodeActor;

    struct ShutdownAwareActor {
        stopped: oneshot::Sender<()>,
    }

    #[async_trait]
    impl NodeActor for ShutdownAwareActor {
        type Error = ();
        type StartData = CancellationToken;

        async fn start(self, cancellation: Self::StartData) -> Result<(), Self::Error> {
            cancellation.cancelled().await;
            let _ = self.stopped.send(());
            Ok(())
        }
    }

    async fn run_until_cancelled(
        cancellation: CancellationToken,
        actor: ShutdownAwareActor,
    ) -> Result<(), String> {
        crate::service::spawn_and_wait!(
            cancellation,
            actors = [Some((actor, cancellation.clone()))]
        );
        Ok(())
    }

    #[tokio::test]
    async fn external_cancellation_waits_for_actor_shutdown() {
        let cancellation = CancellationToken::new();
        let (stopped_tx, stopped_rx) = oneshot::channel();
        let task = tokio::spawn(run_until_cancelled(
            cancellation.clone(),
            ShutdownAwareActor { stopped: stopped_tx },
        ));

        tokio::task::yield_now().await;
        cancellation.cancel();

        timeout(Duration::from_secs(1), stopped_rx)
            .await
            .expect("actor should receive cancellation before service exits")
            .expect("actor should report graceful shutdown");
        timeout(Duration::from_secs(1), task)
            .await
            .expect("service should finish after draining actors")
            .expect("service task should not panic")
            .expect("service should stop cleanly");
    }
}
