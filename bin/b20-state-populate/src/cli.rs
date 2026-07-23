//! CLI argument structs for `b20-state-populate`.

use alloy_primitives::{Address, B256, U256};
use clap::{Parser, Subcommand};
use std::path::PathBuf;

/// Populate or verify B20 benchmark state in a Reth MDBX database.
#[derive(Debug, Parser)]
#[command(name = "b20-state-populate", version)]
pub struct Args {
    /// Subcommand to run.
    #[command(subcommand)]
    pub command: SubCommand,
}

/// Available subcommands.
#[derive(Debug, Subcommand)]
pub enum SubCommand {
    /// Write B20 balance slots and trie nodes into the database.
    Populate(PopulateArgs),
    /// Verify previously written balance slots and report row counts.
    Verify(VerifyArgs),
}

/// Arguments for the `populate` subcommand.
#[derive(Debug, Parser)]
pub struct PopulateArgs {
    /// Path to the Reth datadir (contains the `db/` sub-directory).
    #[arg(long)]
    pub datadir: PathBuf,

    /// Existing EVM contract address whose balance mapping should be populated.
    ///
    /// When set, `--creator`/`--salt` are ignored and the B20 precompile path is
    /// skipped automatically.
    #[arg(long)]
    pub contract: Option<Address>,

    /// Address that created (or will create) the B20 token.
    /// Required when `--contract` is not provided.
    #[arg(long)]
    pub creator: Option<Address>,

    /// 32-byte creation salt (hex, 0x-prefixed).
    /// Required when `--contract` is not provided.
    #[arg(long)]
    pub salt: Option<B256>,

    /// Number of balance slots to write (e.g. 700000000).
    #[arg(long, default_value = "700000000")]
    pub count: u64,

    /// Raw token balance credited to each account (in the token's base unit).
    #[arg(long, default_value = "1000000000000000000")]
    pub balance: U256,

    /// Number of balance slots written per MDBX transaction.
    #[arg(long, default_value = "1000000")]
    pub chunk_size: u64,

    /// Solidity mapping slot index for balance storage (`mapping(address => uint256)`).
    ///
    /// For standard ERC-20 layouts, this is typically 0.
    #[arg(long, default_value = "0")]
    pub mapping_slot: u32,

    /// Skip writing the B20 precompile token entirely; useful when only the EVM contract is needed.
    #[arg(long)]
    pub skip_precompile: bool,

    /// Seed for deriving sender addresses to pre-seed with balances in the EVM contract.
    /// Must match the `seed` field in the load-test YAML config used for benchmarking.
    #[arg(long)]
    pub seed: Option<u64>,

    /// Number of sender addresses to derive from `--seed` and pre-seed with balances.
    /// Must match `sender_count` in the load-test YAML config.
    #[arg(long)]
    pub sender_count: Option<u64>,

    /// Also write synthetic EOA accounts to `PlainAccountState` + `HashedAccounts` + `AccountsTrie`.
    /// The same `address_for_index(0..account_count)` addresses are used for token balances,
    /// making every account holder also a token holder (worst-case equal-depth trie test).
    #[arg(long)]
    pub populate_accounts: bool,

    /// Number of synthetic EOA accounts to write (default: same as `--evm-count` or `--count`).
    #[arg(long)]
    pub account_count: Option<u64>,

    /// ETH balance (in wei) credited to each synthetic EOA account.
    #[arg(long, default_value = "1000000000000000000")]
    pub account_balance: U256,

    /// Rebuild only the storage and account tries from already-written flat state.
    /// Skips all balance-slot, metadata, and bytecode writes; use this to repair
    /// a dataset whose tries are stale without re-running the multi-hour slot write.
    #[arg(long)]
    pub trie_only: bool,
}

/// Arguments for the `verify` subcommand.
#[derive(Debug, Parser)]
pub struct VerifyArgs {
    /// Path to the Reth datadir (contains the `db/` sub-directory).
    #[arg(long)]
    pub datadir: PathBuf,

    /// Existing EVM contract address whose balance mapping should be verified.
    ///
    /// When set, `--creator`/`--salt` are ignored and verification targets this
    /// contract directly.
    #[arg(long)]
    pub contract: Option<Address>,

    /// Address that created the B20 token.
    /// Required when `--contract` is not provided.
    #[arg(long)]
    pub creator: Option<Address>,

    /// 32-byte creation salt (hex, 0x-prefixed).
    /// Required when `--contract` is not provided.
    #[arg(long)]
    pub salt: Option<B256>,

    /// Expected number of balance slots in the precompile token (0 = skip precompile check).
    #[arg(long, default_value = "700000000")]
    pub count: u64,

    /// Seed used when populating sender balances; required to verify them.
    #[arg(long)]
    pub seed: Option<u64>,

    /// Solidity mapping slot index for balance storage (`mapping(address => uint256)`).
    ///
    /// For standard ERC-20 layouts, this is typically 0.
    #[arg(long, default_value = "0")]
    pub mapping_slot: u32,

    /// Number of sender addresses that were pre-seeded; required to verify them.
    #[arg(long)]
    pub sender_count: Option<u64>,

    /// Only verify the pre-seeded sender balance slots, skipping the slow full
    /// `DupSort` count; use this to quickly confirm sender balances are present.
    #[arg(long)]
    pub senders_only: bool,
}
