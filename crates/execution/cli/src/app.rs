use std::{fmt, sync::Arc};

use base_execution_chainspec::BaseChainSpec;
use base_execution_consensus::BaseBeaconConsensus;
use base_execution_evm::BaseExecutorProvider;
use base_node_core::BaseNode;
use eyre::{Result, eyre};
use reth_cli_commands::launcher::Launcher;
use reth_cli_runner::CliRunner;
use reth_node_core::args::{OtlpInitStatus, OtlpLogsStatus};
use reth_node_metrics::recorder::install_prometheus_recorder;
use reth_rpc_server_types::RpcModuleValidator;
use reth_tasks::{RayonConfig, RuntimeConfig};
use reth_tracing::{Layers, TracingGuards};
use tracing::{info, warn};

use crate::{Cli, Commands};

/// A wrapper around a parsed CLI that handles command execution.
#[derive(Debug)]
pub struct CliApp<Ext: clap::Args + fmt::Debug, Rpc: RpcModuleValidator> {
    cli: Cli<Ext, Rpc>,
    runner: Option<CliRunner>,
    layers: Option<Layers>,
    guard: Option<TracingGuards>,
}

impl<Ext, Rpc> CliApp<Ext, Rpc>
where
    Ext: clap::Args + fmt::Debug,
    Rpc: RpcModuleValidator,
{
    pub(crate) fn new(cli: Cli<Ext, Rpc>) -> Self {
        Self { cli, runner: None, layers: Some(Layers::new()), guard: None }
    }

    /// Sets the runner for the CLI commander.
    ///
    /// This replaces any existing runner with the provided one.
    pub fn set_runner(&mut self, runner: CliRunner) {
        self.runner = Some(runner);
    }

    /// Access to tracing layers.
    ///
    /// Returns a mutable reference to the tracing layers, or error
    /// if tracing initialized and layers have detached already.
    pub fn access_tracing_layers(&mut self) -> Result<&mut Layers> {
        self.layers.as_mut().ok_or_else(|| eyre!("Tracing already initialized"))
    }

    /// Execute the configured cli command.
    ///
    /// This accepts a closure that is used to launch the node via the
    /// [`NodeCommand`](reth_cli_commands::node::NodeCommand).
    pub fn run(
        mut self,
        launcher: impl Launcher<crate::chainspec::BaseChainSpecParser, Ext>,
    ) -> Result<()> {
        let runner = self.runner()?;

        // add network name to logs dir
        // Add network name if available to the logs dir
        if let Some(chain_spec) = self.cli.command.chain_spec() {
            self.cli.logs.log_file_directory =
                self.cli.logs.log_file_directory.join(chain_spec.chain.to_string());
        }

        self.init_tracing(&runner)?;

        // Install the prometheus recorder to be sure to record all metrics
        install_prometheus_recorder();

        let components = |spec: Arc<BaseChainSpec>| {
            (
                BaseExecutorProvider::base(Arc::clone(&spec)),
                Arc::new(BaseBeaconConsensus::new(spec)),
            )
        };

        match self.cli.command {
            Commands::Node(command) => {
                // Validate RPC modules using the configured validator
                if let Some(http_api) = &command.rpc.http_api {
                    Rpc::validate_selection(http_api, "http.api").map_err(|e| eyre!("{e}"))?;
                }
                if let Some(ws_api) = &command.rpc.ws_api {
                    Rpc::validate_selection(ws_api, "ws.api").map_err(|e| eyre!("{e}"))?;
                }

                runner.run_command_until_exit(|ctx| command.execute(ctx, launcher))
            }
            Commands::Init(command) => {
                let runtime = runner.runtime();
                runner.run_blocking_until_ctrl_c(command.execute::<BaseNode>(runtime))
            }
            Commands::InitState(command) => {
                let runtime = runner.runtime();
                runner.run_blocking_until_ctrl_c(command.execute::<BaseNode>(runtime))
            }
            Commands::DumpGenesis(command) => runner.run_blocking_until_ctrl_c(command.execute()),
            Commands::Db(command) => {
                runner.run_blocking_command_until_exit(|ctx| command.execute::<BaseNode>(ctx))
            }
            Commands::Stage(command) => {
                runner.run_command_until_exit(|ctx| command.execute::<BaseNode, _>(ctx, components))
            }
            Commands::P2P(command) => runner.run_until_ctrl_c(command.execute::<BaseNode>()),
            Commands::Config(command) => runner.run_until_ctrl_c(command.execute()),
            Commands::Prune(command) => {
                runner.run_command_until_exit(|ctx| command.execute::<BaseNode>(ctx))
            }
            #[cfg(feature = "dev")]
            Commands::TestVectors(command) => runner.run_until_ctrl_c(command.execute()),
            Commands::ReExecute(command) => {
                let runtime = runner.runtime();
                runner.run_until_ctrl_c(command.execute::<BaseNode>(components, runtime))
            }
            Commands::BaseProofs(command) => {
                let runtime = runner.runtime();
                runner.run_blocking_until_ctrl_c(command.execute::<BaseNode>(runtime))
            }
            Commands::SnapshotManifest(command) => {
                command.execute()?;
                Ok(())
            }
            Commands::Download(command) => {
                runner.run_blocking_until_ctrl_c(command.execute::<BaseNode>())
            }
        }
    }

    /// Returns the supplied runner or creates one configured from the node command.
    fn runner(&mut self) -> Result<CliRunner> {
        match self.runner.take() {
            Some(runner) => Ok(runner),
            None => {
                let runtime_config = match &self.cli.command {
                    Commands::Node(command) => RuntimeConfig::default().with_rayon(RayonConfig {
                        reserved_cpu_cores: command.engine.reserved_cpu_cores,
                        proof_storage_worker_threads: command.engine.storage_worker_count,
                        proof_account_worker_threads: command.engine.account_worker_count,
                        prewarming_threads: command.engine.prewarming_threads,
                        ..Default::default()
                    }),
                    _ => RuntimeConfig::default(),
                };
                Ok(CliRunner::try_with_runtime_config(runtime_config)?)
            }
        }
    }

    /// Initializes tracing with the configured options.
    ///
    /// If file logging is enabled, this function stores guard to the struct.
    /// For gRPC OTLP, it requires tokio runtime context.
    pub fn init_tracing(&mut self, runner: &CliRunner) -> Result<()> {
        if self.guard.is_none() {
            let mut layers = self.layers.take().unwrap_or_default();

            let otlp_status = runner.block_on(self.cli.traces.init_otlp_tracing(&mut layers))?;
            let otlp_logs_status = runner.block_on(self.cli.traces.init_otlp_logs(&mut layers))?;

            let enable_reload = self.cli.command.debug_namespace_enabled();
            self.guard = Some(self.cli.logs.init_tracing_with_layers(layers, enable_reload)?);
            info!(target: "reth::cli", log_dir = %self.cli.logs.log_file_directory, "Initialized tracing");

            match otlp_status {
                OtlpInitStatus::Started(endpoint) => {
                    info!(target: "reth::cli", protocol = ?self.cli.traces.protocol, endpoint = %endpoint, "Started OTLP tracing export");
                }
                OtlpInitStatus::NoFeature => {
                    warn!(target: "reth::cli", "Provided OTLP tracing arguments do not have effect, compile with the `otlp` feature")
                }
                OtlpInitStatus::Disabled => {}
            }

            match otlp_logs_status {
                OtlpLogsStatus::Started(endpoint) => {
                    info!(target: "reth::cli", protocol = ?self.cli.traces.protocol, endpoint = %endpoint, "Started OTLP logs export");
                }
                OtlpLogsStatus::NoFeature => {
                    warn!(target: "reth::cli", "Provided OTLP logs arguments do not have effect, compile with the `otlp-logs` feature")
                }
                OtlpLogsStatus::Disabled => {}
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use crate::{Cli, StandardNodeArgs};

    #[test]
    fn node_engine_worker_options_configure_reth_runtime() {
        let cli = Cli::<StandardNodeArgs>::parse_from([
            "base-reth",
            "node",
            "--engine.storage-worker-count",
            "3",
            "--engine.account-worker-count",
            "4",
            "--engine.prewarming-threads",
            "5",
        ]);

        let mut app = cli.configure();
        let runtime = app.runner().unwrap().runtime();

        assert_eq!(runtime.proof_storage_worker_pool().current_num_threads(), 3);
        assert_eq!(runtime.proof_account_worker_pool().current_num_threads(), 4);
        assert_eq!(runtime.prewarming_pool().current_num_threads(), 5);
    }
}
