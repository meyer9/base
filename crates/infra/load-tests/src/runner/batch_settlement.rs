//! x402 batch-settlement channel lifecycle for load tests: per-sender channel opening (funded by a
//! real ERC-3009 deposit into the settlement contract) plus pre-signing of every voucher, claim
//! batch, and top-up authorization during setup, so the single-threaded generator hot path never
//! signs.
//!
//! # Phases
//!
//! 1. **Pre-sign artifacts** (CPU, per sender): build the [`ChannelBook`] — channel configs,
//!    constant payer voucher signatures, the monotone claim-batch ladder, top-up authorizations,
//!    and pre-provisioned fresh channels.
//! 2. **Mint fixture USDC** to each sender via the funder (a configured minter on the
//!    `FiatTokenV2_2` fixture), enough to cover every setup deposit, fresh-channel open, and
//!    top-up.
//! 3. **Open channels** with a real ERC-3009 `deposit`, each sender in parallel on its own nonce
//!    stream.
//!
//! Fresh channels are intentionally left unopened here; the load-phase `deposit` sub-action opens
//! them with their pre-signed authorization to drive new-slot state-root cost.

use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicU64, AtomicUsize},
    },
    time::Duration,
};

use alloy_network::{Ethereum, EthereumWallet, TransactionBuilder};
use alloy_primitives::{Address, B256, Bytes, U256};
use alloy_provider::{PendingTransactionBuilder, Provider};
use alloy_rpc_types::TransactionRequest;
use alloy_signer_local::PrivateKeySigner;
use alloy_sol_types::{SolCall, sol};
use futures::{StreamExt, stream};
use tracing::{debug, info, warn};

use super::{
    BatchSettlementParams, LoadRunner, SubmissionPipeline, TxType,
    load_runner::FUNDING_CONCURRENCY,
};
use crate::{
    BaselineError, Result,
    rpc::{BaseFeeExt, RpcResultExt, WalletProvider, create_wallet_provider},
    workload::{
        ChannelBook, ChannelConfig, ChannelGroup, DEPOSIT_OPEN_GAS_LIMIT, DepositAuth,
        FreshChannel, Rung, SenderChannels, SettlementDomain, TokenDomain, derive_channel_salt,
        derive_receiver, encode_collector_data, encode_deposit_call, erc3009_nonce,
        make_channel_config, sign_digest,
    },
};

/// Gas limit for a fixture-token `mint` during setup.
const MINT_GAS_LIMIT: u64 = 200_000;
/// Timeout for awaiting a setup tx receipt before treating it as failed.
const RECEIPT_TIMEOUT: Duration = Duration::from_secs(120);
/// `validAfter` used for every ERC-3009 deposit authorization (immediately valid).
const VALID_AFTER: u64 = 0;
/// `validBefore` used for every ERC-3009 deposit authorization (effectively never expires).
const VALID_BEFORE: u64 = u64::MAX;

sol! {
    function mint(address to, uint256 amount) external returns (bool);
}

/// A channel-open deposit to submit: `(settlement calldata, gas limit)`.
type OpenDeposit = (Bytes, u64);
/// One sender's setup channel opens: `(sender, sender signer, deposits to submit)`.
type SenderOpenPlan = (Address, PrivateKeySigner, Vec<OpenDeposit>);

impl LoadRunner {
    /// Returns `true` if any configured transaction type is a batch-settlement type.
    pub fn needs_batch_settlement_setup(&self) -> bool {
        self.config.transactions.iter().any(|t| t.tx_type.is_batch_settlement())
    }

    /// Opens and pre-signs every sender's x402 batch-settlement channels.
    ///
    /// Pre-signs all artifacts into a [`ChannelBook`], mints fixture USDC to each sender through
    /// the funder, opens the setup channels with real ERC-3009 deposits, then stores the book and
    /// rebuilds the generator so the (until now un-installed) batch-settlement payloads come online.
    pub async fn setup_batch_settlement(&mut self, funding_key: PrivateKeySigner) -> Result<()> {
        let params = self.config.batch_settlement.clone().ok_or_else(|| {
            BaselineError::Config(
                "batch-settlement transactions configured but batch_settlement params missing"
                    .into(),
            )
        })?;

        if params.channels_per_claim == 0
            || params.channels_per_sender % params.channels_per_claim != 0
        {
            return Err(BaselineError::Config(format!(
                "channels_per_sender ({}) must be a non-zero multiple of channels_per_claim ({})",
                params.channels_per_sender, params.channels_per_claim
            )));
        }
        self.validate_batch_settlement_capacity(&params)?;

        let chain_id = self.config.chain_id;
        let base_fee = self.client.get_base_fee().await?;
        let max_priority_fee = (base_fee / 10).max(1);
        let max_fee = SubmissionPipeline::submission_max_fee(
            base_fee,
            max_priority_fee,
            self.config.max_gas_price,
        );

        // A fresh per-run salt keeps every run's channelIds (and receiver slots) distinct, so a
        // re-run never collides with a prior run's already-opened channels.
        let run_salt = B256::from(rand::random::<[u8; 32]>());
        let settlement_domain = SettlementDomain::new(chain_id, params.settlement);
        let token_domain = TokenDomain::new(chain_id, params.token);

        let account_data: Vec<(Address, PrivateKeySigner)> =
            self.accounts.accounts().iter().map(|a| (a.address, a.signer.clone())).collect();
        let total = account_data.len();
        let sign_claim_batches = self.config.transactions.iter().any(|tx| {
            tx.weight > 0 && matches!(tx.tx_type, TxType::BatchSettlementClaimWithSignature)
        });

        info!(
            senders = total,
            channels_per_sender = params.channels_per_sender,
            channels_per_claim = params.channels_per_claim,
            "pre-signing x402 batch-settlement channel artifacts"
        );

        // Phase 1: build every sender's pre-signed artifacts and collect the setup-open deposits.
        let pb_sign = self.progress_bar(total as u64, "Pre-signing batch-settlement channels");
        let mut senders_map: HashMap<Address, SenderChannels> = HashMap::with_capacity(total);
        // One entry per (sender, signer) with the channel opens it must submit.
        let mut open_plan: Vec<SenderOpenPlan> = Vec::with_capacity(total);
        for (sender, signer) in &account_data {
            let (sender_channels, opens) = build_sender_channels(
                &params,
                run_salt,
                *sender,
                signer,
                &settlement_domain,
                &token_domain,
                sign_claim_batches,
            );
            let deposits = opens
                .into_iter()
                .map(|(config, amount, collector_data)| {
                    (
                        encode_deposit_call(config, amount, params.collector, collector_data),
                        DEPOSIT_OPEN_GAS_LIMIT,
                    )
                })
                .collect();
            open_plan.push((*sender, signer.clone(), deposits));
            senders_map.insert(*sender, sender_channels);
            pb_sign.inc(1);
        }
        pb_sign.finish_and_clear();

        // Phase 2: mint enough fixture USDC to each sender to cover every deposit it will make.
        let per_sender_mint = per_sender_mint_amount(&params);
        let sender_addresses: Vec<Address> = account_data.iter().map(|(a, _)| *a).collect();
        self.mint_fixture_token(
            &funding_key,
            params.token,
            &sender_addresses,
            per_sender_mint,
            chain_id,
            max_fee,
            max_priority_fee,
        )
        .await?;

        // Phase 3: open each sender's setup channels via real ERC-3009 deposits, fully in parallel.
        let open_txs: usize = open_plan.iter().map(|(_, _, d)| d.len()).sum();
        let pb_open = self.progress_bar(open_txs as u64, "Opening batch-settlement channels");
        let rpc_url = self.config.primary_submission_rpc().clone();
        let open_futs = open_plan.into_iter().map(|(sender, signer, deposits)| {
            let rpc_url = rpc_url.clone();
            let pb_open = pb_open.clone();
            async move {
                let wallet = EthereumWallet::from(signer);
                let provider = create_wallet_provider(rpc_url, wallet);
                let result = Self::submit_sender_deposits(
                    &provider,
                    sender,
                    params.settlement,
                    deposits,
                    chain_id,
                    max_fee,
                    max_priority_fee,
                    &pb_open,
                )
                .await;
                (sender, result)
            }
        });
        let open_results: Vec<_> =
            stream::iter(open_futs).buffer_unordered(FUNDING_CONCURRENCY).collect().await;
        pb_open.finish_and_clear();

        let mut failed = 0usize;
        let mut first_error: Option<String> = None;
        for (sender, result) in open_results {
            if let Err(e) = result {
                warn!(sender = %sender, error = %e, "batch-settlement channel open failed");
                failed += 1;
                if first_error.is_none() {
                    first_error = Some(e.to_string());
                }
            }
        }
        if failed > 0 {
            let detail = first_error.unwrap_or_else(|| "unknown error".to_string());
            return Err(BaselineError::Transaction(format!(
                "{failed}/{total} senders failed to open batch-settlement channels (first: {detail})"
            )));
        }

        let book = ChannelBook {
            settlement: params.settlement,
            collector: params.collector,
            token: params.token,
            senders: senders_map,
            fresh_channel_ratio: params.fresh_channel_ratio,
            deposit_cursor: AtomicUsize::new(0),
            exhausted_requests: AtomicU64::new(0),
        };
        self.batch_settlement_book = Some(Arc::new(book));

        // Rebuild the generator so the batch-settlement payloads (skipped while the book was unset)
        // are now installed with the shared channel book.
        self.generator = Self::create_generator(
            self.workload_config(),
            &self.config,
            self.b20_run_salt,
            self.batch_settlement_book.as_ref(),
        )?;

        info!(senders = total, channels = open_txs, "x402 batch-settlement setup complete");
        Ok(())
    }

    /// Rejects finite artifact pools that are too small for the configured target and duration.
    ///
    /// The generator cannot sign on its hot path. Every state-advancing claim and ERC-3009 deposit
    /// therefore consumes a pre-signed artifact. A 25% margin covers weighted-selection variance
    /// and moderate error in the initial gas estimate; runtime exhaustion is also treated as a
    /// workload error.
    fn validate_batch_settlement_capacity(&self, params: &BatchSettlementParams) -> Result<()> {
        let duration = self.config.duration.ok_or_else(|| {
            BaselineError::Config(
                "continuous batch-settlement runs are unsupported because pre-signed artifacts \
                 are finite; configure a duration"
                    .into(),
            )
        })?;
        let total_weight: u64 =
            self.config.transactions.iter().map(|tx| u64::from(tx.weight)).sum();
        if total_weight == 0 {
            return Ok(());
        }

        let avg_gas = self.estimate_avg_gas().max(1);
        let target_gas =
            u128::from(self.config.target_gps).saturating_mul(duration.as_millis()) / 1_000;
        let projected_txs = ceil_div(target_gas, u128::from(avg_gas));
        let required_for_weight = |weight: u64| {
            let projected = ceil_div(
                projected_txs.saturating_mul(u128::from(weight)),
                u128::from(total_weight),
            );
            ceil_div(projected.saturating_mul(5), 4)
        };

        let claim_weight: u64 = self
            .config
            .transactions
            .iter()
            .filter(|tx| {
                matches!(
                    tx.tx_type,
                    TxType::BatchSettlementClaimWithSignature | TxType::BatchSettlementClaim
                )
            })
            .map(|tx| u64::from(tx.weight))
            .sum();
        let deposit_weight: u64 = self
            .config
            .transactions
            .iter()
            .filter(|tx| matches!(tx.tx_type, TxType::BatchSettlementDeposit))
            .map(|tx| u64::from(tx.weight))
            .sum();
        let refund_weight: u64 = self
            .config
            .transactions
            .iter()
            .filter(|tx| matches!(tx.tx_type, TxType::BatchSettlementRefund))
            .map(|tx| u64::from(tx.weight))
            .sum();

        let senders = self.accounts.len() as u128;
        let groups_per_sender = (params.channels_per_sender / params.channels_per_claim) as u128;
        let claim_capacity = senders
            .saturating_mul(groups_per_sender)
            .saturating_mul(params.claim_ladder_rungs as u128);
        let required_claims = required_for_weight(claim_weight);
        if claim_capacity < required_claims {
            return Err(BaselineError::Config(format!(
                "batch_settlement claim artifact capacity ({claim_capacity}) is below the \
                 projected requirement with safety margin ({required_claims}); increase \
                 claim_ladder_rungs"
            )));
        }

        let fresh_channels = fresh_channel_count(params);
        let warm_topups = params.channels_per_sender.saturating_mul(params.topups_per_channel);
        let deposits_per_sender = if params.fresh_channel_ratio >= 1.0 {
            fresh_channels
        } else if params.fresh_channel_ratio <= 0.0 {
            warm_topups
        } else {
            fresh_channels.saturating_add(warm_topups)
        };
        let deposit_capacity = senders.saturating_mul(deposits_per_sender as u128);
        let required_deposits = required_for_weight(deposit_weight);
        if deposit_capacity < required_deposits {
            return Err(BaselineError::Config(format!(
                "batch_settlement deposit artifact capacity ({deposit_capacity}) is below the \
                 projected requirement with safety margin ({required_deposits}); increase \
                 topups_per_channel"
            )));
        }

        let step = (params.deposit_amount / (params.claim_ladder_rungs as u128 + 1)).max(1);
        let top_claim = step
            .saturating_mul(params.claim_ladder_rungs as u128)
            .min(params.deposit_amount.saturating_sub(1));
        let refund_headroom = params.deposit_amount.saturating_sub(top_claim);
        let claim_channels = senders.saturating_mul(params.channels_per_sender as u128);
        let required_refunds_per_channel =
            ceil_div(required_for_weight(refund_weight), claim_channels);
        if refund_headroom < required_refunds_per_channel {
            return Err(BaselineError::Config(format!(
                "batch_settlement per-channel refund headroom ({refund_headroom}) is below the \
                 projected requirement with safety margin ({required_refunds_per_channel}); \
                 increase deposit_amount or reduce claim_ladder_rungs"
            )));
        }

        Ok(())
    }

    /// Mints `amount` of the fixture token to every sender through the funder minter.
    ///
    /// The funder signs from a single nonce stream, so mints are submitted and confirmed in bounded
    /// chunks to stay under the per-sender txpool limit.
    #[allow(clippy::too_many_arguments)]
    async fn mint_fixture_token(
        &self,
        funding_key: &PrivateKeySigner,
        token: Address,
        senders: &[Address],
        amount: U256,
        chain_id: u64,
        max_fee: u128,
        max_priority_fee: u128,
    ) -> Result<()> {
        let funder = funding_key.address();
        let wallet = EthereumWallet::from(funding_key.clone());
        let provider = create_wallet_provider(self.config.primary_submission_rpc().clone(), wallet);
        let mut nonce =
            provider.get_transaction_count(funder).pending().await.rpc("funder pending nonce")?;

        let pb = self.progress_bar(senders.len() as u64, "Minting batch-settlement fixture USDC");
        info!(senders = senders.len(), amount = %amount, "minting fixture USDC to senders");

        for chunk in senders.chunks(FUNDING_CONCURRENCY) {
            let mut pendings = Vec::with_capacity(chunk.len());
            for &to in chunk {
                let input = Bytes::from(mintCall { to, amount }.abi_encode());
                let tx = TransactionRequest::default()
                    .with_to(token)
                    .with_input(input)
                    .with_nonce(nonce)
                    .with_chain_id(chain_id)
                    .with_gas_limit(MINT_GAS_LIMIT)
                    .with_max_fee_per_gas(max_fee)
                    .with_max_priority_fee_per_gas(max_priority_fee);
                nonce += 1;
                let pending = provider
                    .send_transaction(tx)
                    .await
                    .map_err(|e| BaselineError::Transaction(format!("mint send failed: {e}")))?;
                pendings.push(pending);
            }
            for pending in pendings {
                let receipt =
                    pending.with_timeout(Some(RECEIPT_TIMEOUT)).get_receipt().await.map_err(
                        |e| BaselineError::Transaction(format!("mint receipt failed: {e}")),
                    )?;
                if !receipt.status() {
                    return Err(BaselineError::Transaction(format!(
                        "fixture-token mint reverted (tx {}); is the funder a configured minter?",
                        receipt.transaction_hash
                    )));
                }
                pb.inc(1);
            }
        }
        pb.finish_and_clear();
        Ok(())
    }

    /// Submits one sender's channel-open deposits in bounded chunks, confirming each chunk.
    #[allow(clippy::too_many_arguments)]
    async fn submit_sender_deposits(
        provider: &WalletProvider,
        sender: Address,
        settlement: Address,
        deposits: Vec<OpenDeposit>,
        chain_id: u64,
        max_fee: u128,
        max_priority_fee: u128,
        pb: &indicatif::ProgressBar,
    ) -> Result<()> {
        let mut nonce =
            provider.get_transaction_count(sender).pending().await.rpc("sender pending nonce")?;

        for chunk in deposits.chunks(FUNDING_CONCURRENCY) {
            let mut pendings: Vec<PendingTransactionBuilder<Ethereum>> =
                Vec::with_capacity(chunk.len());
            for (input, gas_limit) in chunk {
                let tx = TransactionRequest::default()
                    .with_to(settlement)
                    .with_input(input.clone())
                    .with_nonce(nonce)
                    .with_chain_id(chain_id)
                    .with_gas_limit(*gas_limit)
                    .with_max_fee_per_gas(max_fee)
                    .with_max_priority_fee_per_gas(max_priority_fee);
                nonce += 1;
                let pending = provider
                    .send_transaction(tx)
                    .await
                    .map_err(|e| BaselineError::Transaction(format!("deposit send failed: {e}")))?;
                pendings.push(pending);
            }
            for pending in pendings {
                let receipt =
                    pending.with_timeout(Some(RECEIPT_TIMEOUT)).get_receipt().await.map_err(
                        |e| BaselineError::Transaction(format!("deposit receipt failed: {e}")),
                    )?;
                if !receipt.status() {
                    return Err(BaselineError::Transaction(format!(
                        "channel-open deposit reverted (tx {})",
                        receipt.transaction_hash
                    )));
                }
                pb.inc(1);
            }
        }
        debug!(sender = %sender, "sender channel opens confirmed");
        Ok(())
    }

    /// Teardown hook for batch-settlement channels.
    ///
    /// Channel escrow is denominated in the worthless devnet fixture token, so stranded escrow has
    /// no value and there is nothing to reclaim; the native-ETH drain in [`Self::drain_accounts`]
    /// recovers the only asset worth recovering. Kept for symmetry with the B-20 teardown and so a
    /// future refund/withdraw sweep has a home.
    pub fn teardown_batch_settlement(&self) -> Result<()> {
        if self.batch_settlement_book.is_some() {
            debug!("batch-settlement teardown: leaving devnet-token escrow in place");
        }
        Ok(())
    }
}

const fn ceil_div(numerator: u128, denominator: u128) -> u128 {
    numerator.saturating_add(denominator.saturating_sub(1)) / denominator
}

/// Total fixture-token amount a single sender must hold to cover all of its setup deposits, fresh
/// channel opens, and pre-signed top-ups.
fn per_sender_mint_amount(params: &BatchSettlementParams) -> U256 {
    let fresh_count = fresh_channel_count(params);
    let opens = params.channels_per_sender + fresh_count;
    let topup_amount = topup_amount(params.deposit_amount);
    let topups = params.channels_per_sender * params.topups_per_channel;
    U256::from(params.deposit_amount)
        .saturating_mul(U256::from(opens as u64))
        .saturating_add(U256::from(topup_amount).saturating_mul(U256::from(topups as u64)))
}

/// Number of pre-provisioned (unopened) fresh channels reserved per sender.
fn fresh_channel_count(params: &BatchSettlementParams) -> usize {
    if params.fresh_channel_ratio <= 0.0 {
        return 0;
    }
    let warm_capacity = params.channels_per_sender.saturating_mul(params.topups_per_channel.max(1));
    if params.fresh_channel_ratio >= 1.0 {
        return warm_capacity;
    }
    (warm_capacity as f64 * params.fresh_channel_ratio / (1.0 - params.fresh_channel_ratio)).ceil()
        as usize
}

/// Per-top-up amount: a small fraction of the initial deposit (at least one base unit).
const fn topup_amount(deposit_amount: u128) -> u128 {
    let candidate = deposit_amount / 100;
    if candidate == 0 { 1 } else { candidate }
}

/// Builds all pre-signed artifacts for one sender and the list of setup channel-open deposits.
///
/// Returns the [`SenderChannels`] to store in the book plus `(config, amount, collectorData)`
/// tuples for each channel that must be opened during setup (fresh channels are excluded — they are
/// opened lazily during the load phase).
fn build_sender_channels(
    params: &BatchSettlementParams,
    run_salt: B256,
    sender: Address,
    signer: &PrivateKeySigner,
    settlement_domain: &SettlementDomain,
    token_domain: &TokenDomain,
    sign_claim_batches: bool,
) -> (SenderChannels, Vec<(ChannelConfig, u128, Bytes)>) {
    let deposit_amount = params.deposit_amount;
    let topup_amount = topup_amount(deposit_amount);
    let withdraw_delay = params.withdraw_delay_secs;
    let groups_count = params.channels_per_sender / params.channels_per_claim;
    let fresh_count = fresh_channel_count(params);

    let mut groups = Vec::with_capacity(groups_count);
    let mut opens = Vec::with_capacity(params.channels_per_sender);
    let mut channel_index: u64 = 0;

    for _ in 0..groups_count {
        let mut configs = Vec::with_capacity(params.channels_per_claim);
        let mut channel_ids = Vec::with_capacity(params.channels_per_claim);
        let mut voucher_signatures = Vec::with_capacity(params.channels_per_claim);
        let mut topups: Vec<Vec<DepositAuth>> = Vec::with_capacity(params.channels_per_claim);

        for _ in 0..params.channels_per_claim {
            let receiver = derive_receiver(sender, run_salt, channel_index);
            let salt = derive_channel_salt(sender, run_salt, channel_index);
            let config = make_channel_config(sender, receiver, params.token, withdraw_delay, salt);
            let channel_id = settlement_domain.channel_id(&config);

            // The payer voucher only commits to (channelId, maxClaimableAmount), so it is constant
            // across every rung of the claim ladder.
            let voucher_digest = settlement_domain.voucher_digest(channel_id, deposit_amount);
            voucher_signatures.push(sign_digest(signer, voucher_digest));

            // Setup-open deposit (ERC-3009 salt 0).
            let open_auth = build_deposit_auth(
                token_domain,
                signer,
                sender,
                params.collector,
                channel_id,
                deposit_amount,
                U256::ZERO,
            );
            opens.push((config.clone(), deposit_amount, open_auth.collector_data));

            // Warm top-up authorizations (ERC-3009 salts 1..=topups_per_channel, distinct nonces).
            let mut channel_topups = Vec::with_capacity(params.topups_per_channel);
            for t in 1..=params.topups_per_channel {
                channel_topups.push(build_deposit_auth(
                    token_domain,
                    signer,
                    sender,
                    params.collector,
                    channel_id,
                    topup_amount,
                    U256::from(t as u64),
                ));
            }

            configs.push(config);
            channel_ids.push(channel_id);
            topups.push(channel_topups);
            channel_index += 1;
        }

        let rungs = build_claim_ladder(
            settlement_domain,
            signer,
            &channel_ids,
            deposit_amount,
            params.claim_ladder_rungs,
            sign_claim_batches,
        );

        groups.push(ChannelGroup {
            configs,
            voucher_signatures,
            topups,
            max_claimable: deposit_amount,
            rungs,
            cursor: AtomicUsize::new(0),
        });
    }

    let mut fresh = Vec::with_capacity(fresh_count);
    for _ in 0..fresh_count {
        let receiver = derive_receiver(sender, run_salt, channel_index);
        let salt = derive_channel_salt(sender, run_salt, channel_index);
        let config = make_channel_config(sender, receiver, params.token, withdraw_delay, salt);
        let channel_id = settlement_domain.channel_id(&config);
        let open = build_deposit_auth(
            token_domain,
            signer,
            sender,
            params.collector,
            channel_id,
            deposit_amount,
            U256::ZERO,
        );
        fresh.push(FreshChannel { config, open });
        channel_index += 1;
    }

    let sender_channels = SenderChannels {
        groups,
        fresh,
        group_cursor: AtomicUsize::new(0),
        fresh_cursor: AtomicUsize::new(0),
        topup_cursor: AtomicUsize::new(0),
        settle_cursor: AtomicUsize::new(0),
        refund_cursor: AtomicUsize::new(0),
    };
    (sender_channels, opens)
}

/// Builds a monotone, strictly-increasing ladder of pre-signed claim rungs shared by a group.
///
/// Every channel in the group carries the same cumulative `totalClaimed` at a given rung, so a
/// single receiver-authorizer signature covers the whole batch. Rungs never exceed the per-channel
/// voucher ceiling (`deposit_amount`), so the contract never reverts with `ClaimExceedsBalance`.
fn build_claim_ladder(
    settlement_domain: &SettlementDomain,
    signer: &PrivateKeySigner,
    channel_ids: &[B256],
    deposit_amount: u128,
    ladder_rungs: usize,
    sign_batches: bool,
) -> Vec<Rung> {
    let rungs_n = ladder_rungs.max(1);
    // Divide by `rungs_n + 1` so the top rung tops out strictly below `deposit_amount`, reserving one
    // step of permanent headroom between `totalClaimed` and the channel `balance`. `refund` decrements
    // `ch.balance` (capped to `balance - totalClaimed`), so without this reserve a single `amount = 1`
    // refund on a channel would make its top-rung claim revert with `ClaimExceedsBalance`. The voucher
    // ceiling (`maxClaimableAmount`) stays at `deposit_amount`, which remains >= every rung total.
    let step = (deposit_amount / (rungs_n as u128 + 1)).max(1);
    let mut rungs = Vec::with_capacity(rungs_n);
    let mut prev = 0u128;
    for r in 1..=rungs_n {
        let total = step.saturating_mul(r as u128).min(deposit_amount.saturating_sub(1));
        if total <= prev {
            break;
        }
        prev = total;
        let rows: Vec<(B256, u128, u128)> =
            channel_ids.iter().map(|cid| (*cid, deposit_amount, total)).collect();
        let batch_signature = if sign_batches {
            sign_digest(signer, settlement_domain.claim_batch_digest(&rows))
        } else {
            Bytes::new()
        };
        rungs.push(Rung { total_claimed: total, batch_signature });
    }
    rungs
}

/// Signs one ERC-3009 `receiveWithAuthorization` deposit authorization and encodes its
/// `collectorData` blob for the [`crate::workload::ChannelBook`].
fn build_deposit_auth(
    token_domain: &TokenDomain,
    signer: &PrivateKeySigner,
    payer: Address,
    collector: Address,
    channel_id: B256,
    amount: u128,
    salt: U256,
) -> DepositAuth {
    let nonce = erc3009_nonce(channel_id, salt);
    let digest = token_domain.receive_with_authorization_digest(
        payer,
        collector,
        amount,
        VALID_AFTER,
        VALID_BEFORE,
        nonce,
    );
    let signature = sign_digest(signer, digest);
    let collector_data = encode_collector_data(VALID_AFTER, VALID_BEFORE, salt, signature);
    DepositAuth { amount, collector_data }
}
