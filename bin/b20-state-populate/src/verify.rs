//! Read-back verification for populated B20 state.

use crate::StorageTrieVersion;
use crate::cli::VerifyArgs;
use crate::storage::{
    B20_TOTAL_SUPPLY_SLOT, address_for_index, b20_balance_slot, derive_b20_asset_address,
    derive_sender_addresses, evm_balance_slot,
};
use alloy_primitives::{Address, B256, keccak256};
use eyre::{Result, WrapErr, bail};
use reth_db::{
    ClientVersion, Database,
    mdbx::{DatabaseArguments, MaxReadTransactionDuration},
    open_db,
};
use reth_db_api::{
    cursor::{DbCursorRO, DbDupCursorRO},
    tables,
    transaction::DbTx,
};
use reth_trie::Nibbles;
use reth_trie_db::{LegacyKeyAdapter, PackedKeyAdapter, TrieTableAdapter};
use tracing::info;

/// Verifies previously written B20 state in a Reth MDBX database.
#[derive(Debug)]
pub struct Verifier;

impl Verifier {
    /// Runs the full verification suite against the given datadir.
    pub fn run(args: VerifyArgs) -> Result<()> {
        if args.contract.is_none() && (args.creator.is_none() || args.salt.is_none()) {
            bail!(
                "provide --contract for generic EVM contract verification, or provide both --creator and --salt for B20 precompile verification"
            )
        }

        let token_addr = args.contract.unwrap_or_else(|| {
            derive_b20_asset_address(
                args.creator.expect("validated above"),
                args.salt.expect("validated above"),
            )
        });
        let hashed_token = keccak256(token_addr);
        let uses_evm_mapping = args.contract.is_some();

        // Counting hundreds of millions of DupSort entries in a single read
        // transaction easily exceeds MDBX's default long-lived-read timeout, so
        // disable it for the verification pass.
        let db = open_db(
            args.datadir.join("db"),
            DatabaseArguments::new(ClientVersion::default())
                .with_max_read_transaction_duration(Some(MaxReadTransactionDuration::Unbounded)),
        )
        .wrap_err("open MDBX database")?;

        let tx = db.tx().wrap_err("begin read tx")?;
        let storage_trie_version =
            StorageTrieVersion::detect(&tx).wrap_err("detect storage settings")?;

        if args.senders_only {
            let seed =
                args.seed.ok_or_else(|| eyre::eyre!("--seed required with --senders-only"))?;
            let sender_count = args.sender_count.unwrap_or(0) as usize;
            eyre::ensure!(sender_count > 0, "--sender-count required with --senders-only");
            info!(seed, sender_count, "verifying only pre-seeded sender balances");
            Self::check_sender_balances(
                &tx,
                token_addr,
                seed,
                sender_count,
                uses_evm_mapping.then_some(args.mapping_slot),
            )
            .wrap_err("check sender balance slots")?;
            info!("sender balance verification passed");
            return Ok(());
        }

        if args.count > 0 {
            info!(token = %token_addr, count = args.count, "verifying token storage");
            Self::check_account(&tx, token_addr, hashed_token, None)
                .wrap_err("check token account")?;
            if !uses_evm_mapping {
                Self::check_total_supply(&tx, token_addr, hashed_token)
                    .wrap_err("check precompile total_supply slot")?;
            }
            Self::check_balance_samples(
                &tx,
                token_addr,
                hashed_token,
                args.count,
                uses_evm_mapping.then_some(args.mapping_slot),
            )
            .wrap_err("check balance samples")?;
            let expected_plain = if uses_evm_mapping { args.count } else { args.count + 4 };
            Self::count_storage_entries(&tx, token_addr, hashed_token, expected_plain)
                .wrap_err("count storage entries")?;
            Self::check_trie_nodes(&tx, hashed_token, storage_trie_version)
                .wrap_err("check trie nodes")?;

            if let Some(seed) = args.seed {
                let sender_count = args.sender_count.unwrap_or(0) as usize;
                if sender_count > 0 {
                    Self::check_sender_balances(
                        &tx,
                        token_addr,
                        seed,
                        sender_count,
                        uses_evm_mapping.then_some(args.mapping_slot),
                    )
                    .wrap_err("check sender balance slots")?;
                }
            }

            info!("token verification passed");
        } else {
            info!("skipping token check (count = 0)");
        }

        info!("all verification checks passed");
        Ok(())
    }

    fn check_account(
        tx: &impl DbTx,
        token_addr: Address,
        hashed_token: B256,
        expected_bytecode_hash: Option<B256>,
    ) -> Result<()> {
        let entry = tx
            .cursor_read::<tables::PlainAccountState>()
            .wrap_err("open PlainAccountState")?
            .seek_exact(token_addr)
            .wrap_err("seek token in PlainAccountState")?;
        eyre::ensure!(entry.is_some(), "token account missing from PlainAccountState");

        if let Some(expected_hash) = expected_bytecode_hash {
            let actual_hash = entry.as_ref().and_then(|(_, a)| a.bytecode_hash);
            eyre::ensure!(
                actual_hash == Some(expected_hash),
                "bytecode_hash mismatch: expected {expected_hash}, got {actual_hash:?}"
            );
        }

        let hashed = tx
            .cursor_read::<tables::HashedAccounts>()
            .wrap_err("open HashedAccounts")?
            .seek_exact(hashed_token)
            .wrap_err("seek token in HashedAccounts")?;
        eyre::ensure!(hashed.is_some(), "token account missing from HashedAccounts");

        info!(token = %token_addr, "account present in PlainAccountState + HashedAccounts");
        Ok(())
    }

    fn check_total_supply(tx: &impl DbTx, token_addr: Address, hashed_token: B256) -> Result<()> {
        let mut cursor =
            tx.cursor_dup_read::<tables::PlainStorageState>().wrap_err("open PlainStorageState")?;

        let entry = cursor
            .seek_by_key_subkey(token_addr, B20_TOTAL_SUPPLY_SLOT)
            .wrap_err("seek total_supply slot")?;
        eyre::ensure!(
            entry
                .as_ref()
                .map(|e| e.key == B20_TOTAL_SUPPLY_SLOT && e.value > alloy_primitives::U256::ZERO)
                .unwrap_or(false),
            "total_supply slot missing or zero in PlainStorageState"
        );

        let hashed_total_supply = keccak256(B20_TOTAL_SUPPLY_SLOT);
        let mut hcursor =
            tx.cursor_dup_read::<tables::HashedStorages>().wrap_err("open HashedStorages")?;
        let hentry = hcursor
            .seek_by_key_subkey(hashed_token, hashed_total_supply)
            .wrap_err("seek hashed total_supply slot")?;
        eyre::ensure!(
            hentry
                .as_ref()
                .map(|e| e.key == hashed_total_supply && e.value > alloy_primitives::U256::ZERO)
                .unwrap_or(false),
            "total_supply slot missing or zero in HashedStorages"
        );

        if let Some(e) = entry {
            info!(total_supply = %e.value, token = %token_addr, "total_supply slot OK");
        }
        Ok(())
    }

    fn check_balance_samples(
        tx: &impl DbTx,
        token_addr: Address,
        hashed_token: B256,
        count: u64,
        mapping_slot: Option<u32>,
    ) -> Result<()> {
        let sample_indices = [0u64, 1, count / 4, count / 2, count - 1];
        let mut cursor =
            tx.cursor_dup_read::<tables::PlainStorageState>().wrap_err("open PlainStorageState")?;
        let mut hcursor =
            tx.cursor_dup_read::<tables::HashedStorages>().wrap_err("open HashedStorages")?;

        for idx in sample_indices {
            let addr = address_for_index(idx);
            let plain_slot = mapping_slot
                .map_or_else(|| b20_balance_slot(addr), |slot| evm_balance_slot(addr, slot));
            let hashed_slot = keccak256(plain_slot);

            let entry =
                cursor.seek_by_key_subkey(token_addr, plain_slot).wrap_err("seek plain slot")?;
            eyre::ensure!(
                entry.as_ref().map(|e| e.key == plain_slot).unwrap_or(false),
                "balance slot missing for idx={idx} addr={addr} in PlainStorageState"
            );

            let hentry = hcursor
                .seek_by_key_subkey(hashed_token, hashed_slot)
                .wrap_err("seek hashed slot")?;
            eyre::ensure!(
                hentry.as_ref().map(|e| e.key == hashed_slot).unwrap_or(false),
                "balance slot missing for idx={idx} addr={addr} in HashedStorages"
            );

            info!(idx, addr = %addr, "balance slot OK");
        }
        Ok(())
    }

    fn check_sender_balances(
        tx: &impl DbTx,
        token_addr: Address,
        seed: u64,
        sender_count: usize,
        mapping_slot: Option<u32>,
    ) -> Result<()> {
        let senders = derive_sender_addresses(seed, sender_count);
        let mut cursor =
            tx.cursor_dup_read::<tables::PlainStorageState>().wrap_err("open PlainStorageState")?;

        for (i, addr) in senders.iter().enumerate() {
            let plain_slot = mapping_slot
                .map_or_else(|| b20_balance_slot(*addr), |slot| evm_balance_slot(*addr, slot));
            let entry = cursor
                .seek_by_key_subkey(token_addr, plain_slot)
                .wrap_err("seek sender plain slot")?;
            eyre::ensure!(
                entry
                    .as_ref()
                    .map(|e| e.key == plain_slot && e.value > alloy_primitives::U256::ZERO)
                    .unwrap_or(false),
                "sender balance slot missing or zero for sender={i} addr={addr}"
            );
        }

        info!(sender_count, "all sender balance slots verified");
        Ok(())
    }

    fn count_storage_entries(
        tx: &impl DbTx,
        token_addr: Address,
        hashed_token: B256,
        expected_minimum: u64,
    ) -> Result<()> {
        let mut cursor = tx
            .cursor_dup_read::<tables::PlainStorageState>()
            .wrap_err("open PlainStorageState for count")?;

        let mut plain_count = 0u64;
        if cursor.seek_exact(token_addr).wrap_err("seek token in PSS")?.is_some() {
            plain_count += 1;
            while cursor.next_dup().wrap_err("next_dup PSS")?.is_some() {
                plain_count += 1;
            }
        }

        let mut hcursor = tx
            .cursor_dup_read::<tables::HashedStorages>()
            .wrap_err("open HashedStorages for count")?;

        let mut hashed_count = 0u64;
        if hcursor.seek_exact(hashed_token).wrap_err("seek token in HS")?.is_some() {
            hashed_count += 1;
            while hcursor.next_dup().wrap_err("next_dup HS")?.is_some() {
                hashed_count += 1;
            }
        }

        info!(
            token = %token_addr,
            plain_count,
            hashed_count,
            expected_minimum,
            "storage entry counts"
        );

        eyre::ensure!(
            plain_count >= expected_minimum,
            "PlainStorageState count {plain_count} < expected minimum {expected_minimum}"
        );
        eyre::ensure!(
            hashed_count >= expected_minimum,
            "HashedStorages count {hashed_count} < expected minimum {expected_minimum}"
        );
        eyre::ensure!(
            plain_count == hashed_count,
            "count mismatch: plain={plain_count} hashed={hashed_count}"
        );

        Ok(())
    }

    fn check_trie_nodes(
        tx: &impl DbTx,
        hashed_token: B256,
        storage_trie_version: StorageTrieVersion,
    ) -> Result<()> {
        match storage_trie_version {
            StorageTrieVersion::V1 => {
                Self::check_trie_nodes_with_adapter::<LegacyKeyAdapter>(tx, hashed_token)
            }
            StorageTrieVersion::V2 => {
                Self::check_trie_nodes_with_adapter::<PackedKeyAdapter>(tx, hashed_token)
            }
        }
    }

    fn check_trie_nodes_with_adapter<A: TrieTableAdapter>(
        tx: &impl DbTx,
        hashed_token: B256,
    ) -> Result<()> {
        let mut storage_cursor =
            tx.cursor_dup_read::<A::StorageTrieTable>().wrap_err("open StoragesTrie")?;
        let has_storage_trie = storage_cursor
            .seek_exact(hashed_token)
            .wrap_err("seek StoragesTrie for token")?
            .is_some();
        eyre::ensure!(
            has_storage_trie,
            "no StoragesTrie entries for token (hashed={hashed_token})"
        );

        let mut node_count = 1u64;
        while storage_cursor.next_dup().wrap_err("next_dup StoragesTrie")?.is_some() {
            node_count += 1;
        }
        eyre::ensure!(node_count > 1, "only 1 StoragesTrie node — trie may be incomplete");

        let has_acct_trie = tx
            .cursor_read::<A::AccountTrieTable>()
            .wrap_err("open AccountsTrie")?
            .seek_exact(A::AccountKey::from(Nibbles::default()))
            .wrap_err("seek AccountsTrie root")?
            .is_some();

        info!(storage_trie_nodes = node_count, "StoragesTrie nodes present");
        info!(account_trie_has_root = has_acct_trie, "AccountsTrie check");
        Ok(())
    }
}
