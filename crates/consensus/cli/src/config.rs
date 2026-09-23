//! Configuration file path wrappers for L1 and L2 configs.
//!
//! These types wrap `Option<PathBuf>` and provide methods to load
//! the configuration from a file or fall back to built-in mappings.

use std::{fs::File, path::PathBuf};

use alloy_chains::Chain;
use alloy_genesis::ChainConfig;
use base_common_genesis::RollupConfig;
use serde_json::from_reader;
use tracing::debug;

/// Error type for configuration loading.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// Failed to open configuration file.
    #[error("failed to open config file: {0}")]
    OpenFile(std::io::Error),
    /// Failed to parse configuration file.
    #[error("failed to parse config: {0}")]
    Parse(serde_json::Error),
    /// Failed to find configuration in a built-in mapping.
    #[error("failed to find config for chain ID {0}")]
    NotFound(u64),
    /// The custom L2 configuration does not match the selected chain.
    #[error("custom L2 config chain ID {configured} does not match selected chain ID {requested}")]
    ChainIdMismatch {
        /// Chain ID selected by `--chain`.
        requested: u64,
        /// Chain ID declared by the custom L2 config.
        configured: u64,
    },
}

/// L1 configuration file path wrapper.
///
/// Wraps an optional path to a custom L1 chain configuration file.
/// If no path is provided, the configuration is loaded from the built-in Ethereum L1 mapping.
#[derive(Clone, Debug, Default, clap::Args)]
pub struct L1ConfigFile {
    /// Path to a custom L1 chain configuration file.
    /// (overrides the default configuration from the built-in Ethereum L1 mapping)
    #[arg(long, visible_alias = "rollup-l1-cfg", env = "BASE_NODE_L1_CHAIN_CONFIG")]
    pub l1_config_file: Option<PathBuf>,
}

impl L1ConfigFile {
    /// Creates a new [`L1ConfigFile`] with the given path.
    pub const fn new(path: Option<PathBuf>) -> Self {
        Self { l1_config_file: path }
    }

    /// Returns the path to the configuration file, if set.
    pub const fn path(&self) -> Option<&PathBuf> {
        self.l1_config_file.as_ref()
    }

    /// Loads the L1 chain configuration.
    ///
    /// If a file path is set, loads the configuration from the JSON file.
    /// Otherwise, falls back to the built-in Ethereum L1 mapping using the provided chain ID.
    pub fn load(&self, l1_chain_id: u64) -> Result<ChainConfig, ConfigError> {
        match &self.l1_config_file {
            Some(path) => {
                debug!(path = ?path, "Loading l1 config from file");
                let file = File::open(path).map_err(ConfigError::OpenFile)?;
                from_reader(file).map_err(ConfigError::Parse)
            }
            None => {
                debug!("loading l1 config from built-in mapping");
                base_common_chains::L1_CONFIGS
                    .get(&l1_chain_id)
                    .cloned()
                    .ok_or(ConfigError::NotFound(l1_chain_id))
            }
        }
    }
}

/// L2 rollup configuration file path wrapper.
///
/// Wraps an optional path to a custom L2 rollup configuration file.
/// If no path is provided, the configuration is loaded from the built-in Base chain config.
#[derive(Clone, Debug, Default, clap::Args)]
pub struct L2ConfigFile {
    /// Path to a custom L2 rollup configuration file.
    /// (overrides the default rollup configuration from the built-in Base chain config)
    #[arg(long, visible_alias = "rollup-cfg", env = "BASE_NODE_ROLLUP_CONFIG")]
    pub l2_config_file: Option<PathBuf>,
}

impl L2ConfigFile {
    /// Creates a new [`L2ConfigFile`] with the given path.
    pub const fn new(path: Option<PathBuf>) -> Self {
        Self { l2_config_file: path }
    }

    /// Returns the path to the configuration file, if set.
    pub const fn path(&self) -> Option<&PathBuf> {
        self.l2_config_file.as_ref()
    }

    /// Loads the L2 rollup configuration.
    ///
    /// If a file path is set, loads the configuration from the JSON file and verifies that its
    /// chain ID matches the selected chain. Otherwise, falls back to the built-in Base chain
    /// config using the provided chain.
    pub fn load(&self, l2_chain: &Chain) -> Result<RollupConfig, ConfigError> {
        match &self.l2_config_file {
            Some(path) => {
                debug!(path = ?path, "loading L2 config from file");
                let file = File::open(path).map_err(ConfigError::OpenFile)?;
                let config: RollupConfig = from_reader(file).map_err(ConfigError::Parse)?;
                if config.l2_chain_id != *l2_chain {
                    return Err(ConfigError::ChainIdMismatch {
                        requested: l2_chain.id(),
                        configured: config.l2_chain_id.id(),
                    });
                }
                Ok(config)
            }
            None => {
                debug!("loading L2 config from built-in chain config");
                base_common_chains::rollup_config!(l2_chain)
                    .ok_or_else(|| ConfigError::NotFound(l2_chain.id()))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use alloy_chains::Chain;
    use base_common_genesis::RollupConfig;
    use tempfile::NamedTempFile;

    use super::*;

    fn l2_config_file(chain_id: u64) -> NamedTempFile {
        let file = NamedTempFile::new().unwrap();
        let config = RollupConfig { l2_chain_id: Chain::from(chain_id), ..Default::default() };
        serde_json::to_writer(&file, &config).unwrap();
        file
    }

    #[test]
    fn test_l1_config_file_default() {
        let config = L1ConfigFile::default();
        assert!(config.path().is_none());
    }

    #[test]
    fn test_l2_config_file_default() {
        let config = L2ConfigFile::default();
        assert!(config.path().is_none());
    }

    #[test]
    fn test_l1_config_file_with_path() {
        let path = PathBuf::from("/tmp/l1_config.json");
        let config = L1ConfigFile::new(Some(path.clone()));
        assert_eq!(config.path(), Some(&path));
    }

    #[test]
    fn test_l2_config_file_with_path() {
        let path = PathBuf::from("/tmp/l2_config.json");
        let config = L2ConfigFile::new(Some(path.clone()));
        assert_eq!(config.path(), Some(&path));
    }

    #[test]
    fn loads_custom_l2_config_for_selected_chain() {
        let file = l2_config_file(8453);
        let config = L2ConfigFile::new(Some(file.path().to_path_buf()));

        let loaded = config.load(&Chain::from(8453_u64)).unwrap();

        assert_eq!(loaded.l2_chain_id, Chain::from(8453_u64));
    }

    #[test]
    fn rejects_custom_l2_config_for_another_chain() {
        let file = l2_config_file(84532);
        let config = L2ConfigFile::new(Some(file.path().to_path_buf()));

        let error = config.load(&Chain::from(8453_u64)).unwrap_err();

        assert!(matches!(
            error,
            ConfigError::ChainIdMismatch { requested: 8453, configured: 84532 }
        ));
    }
}
