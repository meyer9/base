//! x402 batch-settlement payloads for load testing.
//!
//! Models the on-chain surface of the x402 `batch-settlement` scheme (a stateless unidirectional
//! payment-channel contract) as five weighted transaction types: `claimWithSignature`, `claim`,
//! `deposit` (ERC-3009), `settle`, and `refund`.
//!
//! # Role model
//!
//! Every funded pool sender `S_i` owns all signing roles of its own channels
//! (`payer = payerAuthorizer = receiverAuthorizer`) and is also the relayer, so all five tx types
//! submit with `from = S_i` and parallelize across the sender pool. `receiver` is a distinct
//! derived address (no key needed) so `receivers[receiver][token]` slots still grow.
//!
//! # Pre-signing
//!
//! Per-voucher ECDSA signing is far too slow for the single-threaded generator loop, so all
//! signatures are computed once during setup and stored in the [`ChannelBook`]. The hot path only
//! pops pre-signed artifacts and ABI-encodes calldata.
//!
//! Because `claimWithSignature` needs one `receiverAuthorizer` signature over the exact batch of
//! rows, channels are organized into fixed groups of `channels_per_claim`. Each group advances in
//! lockstep along a monotone ladder of cumulative `totalClaimed` rungs, so the batch content at
//! rung `r` is deterministic and the batch signature can be pre-signed per `(group, rung)`. The
//! payer voucher signature only covers `(channelId, maxClaimableAmount)` and is therefore constant
//! across rungs.

use std::sync::{
    Arc,
    atomic::{AtomicU64, AtomicUsize, Ordering},
};

use alloy_network::TransactionBuilder;
use alloy_primitives::{Address, B256, Bytes, U256, aliases::U40, keccak256};
use alloy_rpc_types::TransactionRequest;
use alloy_signer::SignerSync;
use alloy_signer_local::PrivateKeySigner;
use alloy_sol_types::{SolCall, SolValue, sol};

use super::Payload;
use crate::workload::SeededRng;

sol! {
    /// Immutable channel parameters; hashed to form `channelId`.
    #[derive(Debug)]
    struct ChannelConfig {
        address payer;
        address payerAuthorizer;
        address receiver;
        address receiverAuthorizer;
        address token;
        uint40 withdrawDelay;
        bytes32 salt;
    }

    /// Payer-signed authorization data.
    #[derive(Debug)]
    struct Voucher {
        ChannelConfig channel;
        uint128 maxClaimableAmount;
    }

    /// One signed voucher row plus the cumulative amount being claimed.
    #[derive(Debug)]
    struct VoucherClaim {
        Voucher voucher;
        bytes signature;
        uint128 totalClaimed;
    }

    function deposit(ChannelConfig config, uint128 amount, address collector, bytes collectorData) external;
    function claim(VoucherClaim[] voucherClaims) external;
    function claimWithSignature(VoucherClaim[] voucherClaims, bytes authorizerSignature) external;
    function settle(address receiver, address token) external;
    function refund(ChannelConfig config, uint128 amount) external;
}

/// Gas limit for a `settle` transaction.
pub const SETTLE_GAS_LIMIT: u64 = 120_000;
/// Gas limit for a `refund` transaction.
pub const REFUND_GAS_LIMIT: u64 = 120_000;
/// Gas limit for an ERC-3009 first-deposit (fresh channel) transaction.
pub const DEPOSIT_OPEN_GAS_LIMIT: u64 = 320_000;
/// Gas limit for an ERC-3009 top-up (warm channel) transaction.
pub const DEPOSIT_TOPUP_GAS_LIMIT: u64 = 260_000;

/// Gas limit for either claim path, capped at Base's EIP-7825 transaction maximum.
///
/// The linear term is based on the canonical optimized x402 contract's first-claim sweep. Near the
/// cap there is intentionally little headroom: the workload uses those sizes only to locate the
/// protocol boundary.
#[must_use]
pub const fn claim_gas_limit(channels_per_claim: usize) -> u64 {
    let estimated = 150_000 + (channels_per_claim as u64) * 47_000;
    if estimated > 16_777_216 { 16_777_216 } else { estimated }
}

/// The EIP-712 name of the settlement contract's domain.
pub const SETTLEMENT_DOMAIN_NAME: &str = "x402 Batch Settlement";
/// The EIP-712 version of the settlement contract's domain.
pub const SETTLEMENT_DOMAIN_VERSION: &str = "1";
/// The EIP-712 name of the `FiatTokenV2_2` fixture token's domain (matches mainnet USDC).
pub const TOKEN_DOMAIN_NAME: &str = "USD Coin";
/// The EIP-712 version of the `FiatTokenV2_2` fixture token's domain (matches mainnet USDC).
pub const TOKEN_DOMAIN_VERSION: &str = "2";

fn word_address(a: Address) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[12..].copy_from_slice(a.as_slice());
    w
}

const fn word_u256(v: U256) -> [u8; 32] {
    v.to_be_bytes::<32>()
}

fn word_u128(v: u128) -> [u8; 32] {
    U256::from(v).to_be_bytes::<32>()
}

fn eip712_domain_separator(name: &str, version: &str, chain_id: u64, verifying: Address) -> B256 {
    let type_hash = keccak256(
        b"EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)",
    );
    let mut buf = Vec::with_capacity(32 * 5);
    buf.extend_from_slice(type_hash.as_slice());
    buf.extend_from_slice(keccak256(name.as_bytes()).as_slice());
    buf.extend_from_slice(keccak256(version.as_bytes()).as_slice());
    buf.extend_from_slice(&word_u256(U256::from(chain_id)));
    buf.extend_from_slice(&word_address(verifying));
    keccak256(buf)
}

/// EIP-712 domain and typed-data helpers for the `x402BatchSettlement` contract.
///
/// Mirrors `getChannelId` / `getVoucherDigest` / `getClaimBatchDigest` so pre-signed artifacts
/// match on-chain verification exactly.
#[derive(Debug, Clone)]
pub struct SettlementDomain {
    /// Cached EIP-712 domain separator.
    pub domain_separator: B256,
}

impl SettlementDomain {
    /// Builds the settlement domain for a chain and contract address.
    #[must_use]
    pub fn new(chain_id: u64, settlement: Address) -> Self {
        Self {
            domain_separator: eip712_domain_separator(
                SETTLEMENT_DOMAIN_NAME,
                SETTLEMENT_DOMAIN_VERSION,
                chain_id,
                settlement,
            ),
        }
    }

    fn hash_typed_data(&self, struct_hash: B256) -> B256 {
        let mut buf = Vec::with_capacity(2 + 32 + 32);
        buf.extend_from_slice(&[0x19, 0x01]);
        buf.extend_from_slice(self.domain_separator.as_slice());
        buf.extend_from_slice(struct_hash.as_slice());
        keccak256(buf)
    }

    /// Returns the canonical `channelId` for a channel configuration.
    #[must_use]
    pub fn channel_id(&self, config: &ChannelConfig) -> B256 {
        let type_hash = keccak256(
            b"ChannelConfig(address payer,address payerAuthorizer,address receiver,address receiverAuthorizer,address token,uint40 withdrawDelay,bytes32 salt)",
        );
        let mut buf = Vec::with_capacity(32 * 8);
        buf.extend_from_slice(type_hash.as_slice());
        buf.extend_from_slice(&word_address(config.payer));
        buf.extend_from_slice(&word_address(config.payerAuthorizer));
        buf.extend_from_slice(&word_address(config.receiver));
        buf.extend_from_slice(&word_address(config.receiverAuthorizer));
        buf.extend_from_slice(&word_address(config.token));
        buf.extend_from_slice(&word_u256(U256::from(config.withdrawDelay.to::<u64>())));
        buf.extend_from_slice(config.salt.as_slice());
        self.hash_typed_data(keccak256(buf))
    }

    /// EIP-712 digest for a `Voucher` (payer authorization). Independent of `totalClaimed`.
    #[must_use]
    pub fn voucher_digest(&self, channel_id: B256, max_claimable: u128) -> B256 {
        let type_hash = keccak256(b"Voucher(bytes32 channelId,uint128 maxClaimableAmount)");
        let mut buf = Vec::with_capacity(32 * 3);
        buf.extend_from_slice(type_hash.as_slice());
        buf.extend_from_slice(channel_id.as_slice());
        buf.extend_from_slice(&word_u128(max_claimable));
        self.hash_typed_data(keccak256(buf))
    }

    /// EIP-712 digest for a signed `ClaimBatch` (receiver authorization over all rows).
    #[must_use]
    pub fn claim_batch_digest(&self, rows: &[(B256, u128, u128)]) -> B256 {
        let entry_type_hash = keccak256(
            b"ClaimEntry(bytes32 channelId,uint128 maxClaimableAmount,uint128 totalClaimed)",
        );
        let batch_type_hash = keccak256(
            b"ClaimBatch(ClaimEntry[] claims)ClaimEntry(bytes32 channelId,uint128 maxClaimableAmount,uint128 totalClaimed)",
        );

        let entries_root = if rows.is_empty() {
            keccak256(b"")
        } else {
            let mut packed = Vec::with_capacity(rows.len() * 32);
            for (channel_id, max_claimable, total_claimed) in rows {
                let mut buf = Vec::with_capacity(32 * 4);
                buf.extend_from_slice(entry_type_hash.as_slice());
                buf.extend_from_slice(channel_id.as_slice());
                buf.extend_from_slice(&word_u128(*max_claimable));
                buf.extend_from_slice(&word_u128(*total_claimed));
                packed.extend_from_slice(keccak256(buf).as_slice());
            }
            keccak256(packed)
        };

        let mut buf = Vec::with_capacity(32 * 2);
        buf.extend_from_slice(batch_type_hash.as_slice());
        buf.extend_from_slice(entries_root.as_slice());
        self.hash_typed_data(keccak256(buf))
    }
}

/// EIP-712 helper for the ERC-3009 `receiveWithAuthorization` digest of the fixture token.
#[derive(Debug, Clone)]
pub struct TokenDomain {
    /// Cached EIP-712 domain separator for the token.
    pub domain_separator: B256,
}

impl TokenDomain {
    /// Builds the token domain (name/version match mainnet USDC) for a chain and token address.
    #[must_use]
    pub fn new(chain_id: u64, token: Address) -> Self {
        Self {
            domain_separator: eip712_domain_separator(
                TOKEN_DOMAIN_NAME,
                TOKEN_DOMAIN_VERSION,
                chain_id,
                token,
            ),
        }
    }

    /// EIP-712 digest for `receiveWithAuthorization`.
    #[must_use]
    pub fn receive_with_authorization_digest(
        &self,
        from: Address,
        to: Address,
        value: u128,
        valid_after: u64,
        valid_before: u64,
        nonce: B256,
    ) -> B256 {
        let type_hash = keccak256(
            b"ReceiveWithAuthorization(address from,address to,uint256 value,uint256 validAfter,uint256 validBefore,bytes32 nonce)",
        );
        let mut buf = Vec::with_capacity(32 * 7);
        buf.extend_from_slice(type_hash.as_slice());
        buf.extend_from_slice(&word_address(from));
        buf.extend_from_slice(&word_address(to));
        buf.extend_from_slice(&word_u128(value));
        buf.extend_from_slice(&word_u256(U256::from(valid_after)));
        buf.extend_from_slice(&word_u256(U256::from(valid_before)));
        buf.extend_from_slice(nonce.as_slice());

        let mut outer = Vec::with_capacity(2 + 32 + 32);
        outer.extend_from_slice(&[0x19, 0x01]);
        outer.extend_from_slice(self.domain_separator.as_slice());
        outer.extend_from_slice(keccak256(buf).as_slice());
        keccak256(outer)
    }
}

/// The ERC-3009 authorization nonce used by [`ERC3009DepositCollector`]: `keccak256(channelId, salt)`.
#[must_use]
pub fn erc3009_nonce(channel_id: B256, salt: U256) -> B256 {
    let mut buf = Vec::with_capacity(64);
    buf.extend_from_slice(channel_id.as_slice());
    buf.extend_from_slice(&word_u256(salt));
    keccak256(buf)
}

/// Signs a hash with the given key and returns the 65-byte `r‖s‖v` (v in {27, 28}) signature.
///
/// # Panics
///
/// Panics if signing fails, which only occurs on an internal signer error.
#[must_use]
pub fn sign_digest(signer: &PrivateKeySigner, digest: B256) -> Bytes {
    let sig = signer.sign_hash_sync(&digest).expect("hash signing is infallible for a local key");
    Bytes::from(sig.as_bytes().to_vec())
}

/// Encodes a `deposit(config, amount, collector, collectorData)` call to the settlement contract.
#[must_use]
pub fn encode_deposit_call(
    config: ChannelConfig,
    amount: u128,
    collector: Address,
    collector_data: Bytes,
) -> Bytes {
    Bytes::from(
        depositCall { config, amount, collector, collectorData: collector_data }.abi_encode(),
    )
}

/// Encodes the ERC-3009 `collectorData` blob `abi.encode(validAfter, validBefore, salt, signature)`.
#[must_use]
pub fn encode_collector_data(
    valid_after: u64,
    valid_before: u64,
    salt: U256,
    signature: Bytes,
) -> Bytes {
    // `abi_encode()` treats this Rust tuple as one dynamic tuple and prepends an outer `0x20`
    // offset. The collector decodes four parameters, so use the layout produced by Solidity's
    // `abi.encode(validAfter, validBefore, salt, signature)`.
    let encoded =
        (U256::from(valid_after), U256::from(valid_before), salt, signature).abi_encode_params();
    Bytes::from(encoded)
}

/// A pre-signed deposit authorization (ERC-3009 `collectorData`) plus its amount.
#[derive(Debug, Clone)]
pub struct DepositAuth {
    /// The amount authorized for this deposit.
    pub amount: u128,
    /// The ABI-encoded `collectorData` forwarded to the ERC-3009 collector.
    pub collector_data: Bytes,
}

/// A pre-signed cumulative-claim rung shared by every channel in a group.
#[derive(Debug, Clone)]
pub struct Rung {
    /// Cumulative `totalClaimed` applied to each channel at this rung.
    pub total_claimed: u128,
    /// `receiverAuthorizer` signature over the batch, or empty for plain-claim-only runs.
    pub batch_signature: Bytes,
}

/// A fixed group of `channels_per_claim` channels that advance a shared claim ladder in lockstep.
#[derive(Debug)]
pub struct ChannelGroup {
    /// The channel configurations in this group.
    pub configs: Vec<ChannelConfig>,
    /// Per-channel constant payer voucher signatures.
    pub voucher_signatures: Vec<Bytes>,
    /// Per-channel top-up deposit authorizations (distinct ERC-3009 nonces).
    pub topups: Vec<Vec<DepositAuth>>,
    /// Ceiling encoded in every voucher (equals the initial deposit amount).
    pub max_claimable: u128,
    /// The monotone ladder of pre-signed claim rungs.
    pub rungs: Vec<Rung>,
    /// Next rung index to submit.
    pub cursor: AtomicUsize,
}

/// A pre-provisioned channel that is opened lazily during the load phase by a `deposit`.
#[derive(Debug)]
pub struct FreshChannel {
    /// The channel configuration.
    pub config: ChannelConfig,
    /// The full first-deposit authorization that opens the channel.
    pub open: DepositAuth,
}

/// All pre-signed channel artifacts owned by a single sender.
#[derive(Debug)]
pub struct SenderChannels {
    /// Claim-eligible groups opened during setup.
    pub groups: Vec<ChannelGroup>,
    /// Pre-provisioned channels opened during the load phase.
    pub fresh: Vec<FreshChannel>,
    /// Round-robin cursor over [`Self::groups`].
    pub group_cursor: AtomicUsize,
    /// Cursor into [`Self::fresh`].
    pub fresh_cursor: AtomicUsize,
    /// Cursor across every pre-signed warm-channel top-up authorization.
    pub topup_cursor: AtomicUsize,
    /// Round-robin cursor used by `settle` to spread sweeps across distinct receivers.
    pub settle_cursor: AtomicUsize,
    /// Round-robin cursor used by `refund` to spread balance reductions across channels.
    pub refund_cursor: AtomicUsize,
}

/// Shared, interior-mutable book of every sender's pre-signed channel artifacts.
///
/// Built once during setup and shared (`Arc`) with all five payloads. Cursors use atomics so the
/// `&self` [`Payload::generate`] hot path can advance ladders without locking.
#[derive(Debug)]
pub struct ChannelBook {
    /// Settlement contract address (the `to` of every generated transaction).
    pub settlement: Address,
    /// ERC-3009 deposit collector address.
    pub collector: Address,
    /// Fixture token address.
    pub token: Address,
    /// Per-sender pre-signed artifacts.
    pub senders: std::collections::HashMap<Address, SenderChannels>,
    /// Target fraction of load-phase deposits that open a fresh channel.
    pub fresh_channel_ratio: f64,
    /// Cursor across all load-phase deposit operations.
    pub deposit_cursor: AtomicUsize,
    /// Number of generated requests that exhausted their finite pre-signed artifact supply.
    pub exhausted_requests: AtomicU64,
}

impl ChannelBook {
    fn sender(&self, from: Address) -> Option<&SenderChannels> {
        self.senders.get(&from)
    }

    /// Returns how many requests could not be represented by a real x402 operation.
    #[must_use]
    pub fn exhausted_requests(&self) -> u64 {
        self.exhausted_requests.load(Ordering::Relaxed)
    }

    fn exhausted_fallback(&self, from: Address) -> TransactionRequest {
        self.exhausted_requests.fetch_add(1, Ordering::Relaxed);
        fallback_request(from)
    }

    /// Builds a claim (`claim` or `claimWithSignature`) transaction for one of `from`'s groups.
    ///
    /// Advances the selected group's ladder cursor. Returns `None` when `from` owns no groups or
    /// all its groups are exhausted, so the caller can emit a harmless fallback instead of a
    /// silent on-chain no-op.
    fn claim_request(&self, from: Address, with_signature: bool) -> Option<TransactionRequest> {
        let sender = self.sender(from)?;
        if sender.groups.is_empty() {
            return None;
        }

        // Round-robin from a rotating start so senders spread load across their groups, skipping
        // any group whose ladder is exhausted.
        let start = sender.group_cursor.fetch_add(1, Ordering::Relaxed);
        for offset in 0..sender.groups.len() {
            let group = &sender.groups[(start + offset) % sender.groups.len()];
            let rung_idx = group.cursor.fetch_add(1, Ordering::Relaxed);
            if rung_idx >= group.rungs.len() {
                continue;
            }
            let rung = &group.rungs[rung_idx];

            let claims: Vec<VoucherClaim> = group
                .configs
                .iter()
                .enumerate()
                .map(|(i, cfg)| VoucherClaim {
                    voucher: Voucher {
                        channel: cfg.clone(),
                        maxClaimableAmount: group.max_claimable,
                    },
                    signature: group.voucher_signatures[i].clone(),
                    totalClaimed: rung.total_claimed,
                })
                .collect();

            let input = if with_signature {
                claimWithSignatureCall {
                    voucherClaims: claims,
                    authorizerSignature: rung.batch_signature.clone(),
                }
                .abi_encode()
            } else {
                claimCall { voucherClaims: claims }.abi_encode()
            };

            return Some(
                TransactionRequest::default()
                    .with_to(self.settlement)
                    .with_input(Bytes::from(input))
                    .with_gas_limit(claim_gas_limit(group.configs.len())),
            );
        }
        None
    }

    /// Builds a `deposit` transaction: opens a fresh channel or tops up a warm one.
    fn deposit_request(&self, from: Address) -> Option<TransactionRequest> {
        let sender = self.sender(from)?;
        let operation = self.deposit_cursor.fetch_add(1, Ordering::Relaxed);
        let open_fresh = should_open_fresh(operation, self.fresh_channel_ratio);

        if open_fresh {
            self.fresh_deposit_request(sender).or_else(|| self.topup_deposit_request(sender))
        } else {
            self.topup_deposit_request(sender).or_else(|| self.fresh_deposit_request(sender))
        }
    }

    fn fresh_deposit_request(&self, sender: &SenderChannels) -> Option<TransactionRequest> {
        if !sender.fresh.is_empty() {
            let idx = sender.fresh_cursor.fetch_add(1, Ordering::Relaxed);
            if idx < sender.fresh.len() {
                let fresh = &sender.fresh[idx];
                let input = depositCall {
                    config: fresh.config.clone(),
                    amount: fresh.open.amount,
                    collector: self.collector,
                    collectorData: fresh.open.collector_data.clone(),
                }
                .abi_encode();
                return Some(
                    TransactionRequest::default()
                        .with_to(self.settlement)
                        .with_input(Bytes::from(input))
                        .with_gas_limit(DEPOSIT_OPEN_GAS_LIMIT),
                );
            }
        }
        None
    }

    fn topup_deposit_request(&self, sender: &SenderChannels) -> Option<TransactionRequest> {
        // Consume warm-channel top-ups in deterministic rounds. Randomly selecting a channel can
        // hit an exhausted channel while other authorizations remain, silently shortening the
        // usable benchmark.
        if sender.groups.is_empty() {
            return None;
        }
        let channels_per_group = sender.groups.first()?.configs.len();
        if channels_per_group == 0 {
            return None;
        }
        let channel_count = sender.groups.len().saturating_mul(channels_per_group);
        let topups_per_channel = sender.groups.first()?.topups.first()?.len();
        let topup_idx = sender.topup_cursor.fetch_add(1, Ordering::Relaxed);
        if topup_idx >= channel_count.saturating_mul(topups_per_channel) {
            return None;
        }
        let flat_channel_idx = topup_idx % channel_count;
        let auth_idx = topup_idx / channel_count;
        let group = &sender.groups[flat_channel_idx / channels_per_group];
        let channel_idx = flat_channel_idx % channels_per_group;
        let auths = &group.topups[channel_idx];
        let auth = &auths[auth_idx];
        let input = depositCall {
            config: group.configs[channel_idx].clone(),
            amount: auth.amount,
            collector: self.collector,
            collectorData: auth.collector_data.clone(),
        }
        .abi_encode();
        Some(
            TransactionRequest::default()
                .with_to(self.settlement)
                .with_input(Bytes::from(input))
                .with_gas_limit(DEPOSIT_TOPUP_GAS_LIMIT),
        )
    }

    /// Builds a `settle` transaction for a receiver that has recorded claims.
    fn settle_request(&self, from: Address) -> Option<TransactionRequest> {
        let sender = self.sender(from)?;
        if sender.groups.is_empty() {
            return None;
        }
        // Every channel has a distinct receiver and claims advance all channels in a group in
        // lockstep, so a receiver first settled sweeps a non-empty amount but a re-settle before the
        // next claim is an on-chain no-op. Rotate across groups (and channels within a group) so
        // consecutive settles target different receivers with freshly claimed-but-unswept balances.
        let start = sender.settle_cursor.fetch_add(1, Ordering::Relaxed);
        let receiver = sender
            .groups
            .iter()
            .cycle()
            .skip(start % sender.groups.len())
            .take(sender.groups.len())
            .find(|g| g.cursor.load(Ordering::Relaxed) > 0)
            .and_then(|g| g.configs.get(start % g.configs.len().max(1)).map(|c| c.receiver))
            .or_else(|| {
                sender.groups.first().and_then(|g| g.configs.first()).map(|c| c.receiver)
            })?;
        let input = settleCall { receiver, token: self.token }.abi_encode();
        Some(
            TransactionRequest::default()
                .with_to(self.settlement)
                .with_input(Bytes::from(input))
                .with_gas_limit(SETTLE_GAS_LIMIT),
        )
    }

    /// Builds a `refund` transaction for a small amount (never reverts: capped to available escrow).
    fn refund_request(&self, from: Address) -> Option<TransactionRequest> {
        let sender = self.sender(from)?;
        if sender.groups.is_empty() {
            return None;
        }
        let channels_per_group = sender.groups.first()?.configs.len();
        let channel_count = sender.groups.len().saturating_mul(channels_per_group);
        if channel_count == 0 {
            return None;
        }
        let index = sender.refund_cursor.fetch_add(1, Ordering::Relaxed) % channel_count;
        let group = &sender.groups[index / channels_per_group];
        let config = &group.configs[index % channels_per_group];
        let input = refundCall { config: config.clone(), amount: 1u128 }.abi_encode();
        Some(
            TransactionRequest::default()
                .with_to(self.settlement)
                .with_input(Bytes::from(input))
                .with_gas_limit(REFUND_GAS_LIMIT),
        )
    }
}

/// Emits a harmless zero-value self-transfer used when no channel artifact is available.
fn fallback_request(from: Address) -> TransactionRequest {
    TransactionRequest::default().with_to(from).with_value(U256::ZERO).with_gas_limit(21_000)
}

fn should_open_fresh(operation: usize, ratio: f64) -> bool {
    let before = operation as f64 * ratio;
    let after = operation.saturating_add(1) as f64 * ratio;
    after.floor() > before.floor()
}

/// Generates x402 batch-settlement `batch_settlement_claim_with_signature` transactions.
#[derive(Debug, Clone)]
pub struct BatchSettlementClaimWithSignaturePayload {
    /// Shared pre-signed channel artifacts.
    pub book: Arc<ChannelBook>,
}

impl BatchSettlementClaimWithSignaturePayload {
    /// Creates the payload bound to a channel book.
    #[must_use]
    pub const fn new(book: Arc<ChannelBook>) -> Self {
        Self { book }
    }
}

impl Payload for BatchSettlementClaimWithSignaturePayload {
    fn name(&self) -> &'static str {
        "batch_settlement_claim_with_signature"
    }

    fn uses_runner_recipient(&self) -> bool {
        false
    }

    fn generate(&self, _rng: &mut SeededRng, from: Address, _to: Address) -> TransactionRequest {
        self.book.claim_request(from, true).unwrap_or_else(|| self.book.exhausted_fallback(from))
    }
}

/// Generates x402 batch-settlement `batch_settlement_claim` transactions (no batch signature).
#[derive(Debug, Clone)]
pub struct BatchSettlementClaimPayload {
    /// Shared pre-signed channel artifacts.
    pub book: Arc<ChannelBook>,
}

impl BatchSettlementClaimPayload {
    /// Creates the payload bound to a channel book.
    #[must_use]
    pub const fn new(book: Arc<ChannelBook>) -> Self {
        Self { book }
    }
}

impl Payload for BatchSettlementClaimPayload {
    fn name(&self) -> &'static str {
        "batch_settlement_claim"
    }

    fn uses_runner_recipient(&self) -> bool {
        false
    }

    fn generate(&self, _rng: &mut SeededRng, from: Address, _to: Address) -> TransactionRequest {
        self.book.claim_request(from, false).unwrap_or_else(|| self.book.exhausted_fallback(from))
    }
}

/// Generates x402 batch-settlement `batch_settlement_deposit` (ERC-3009) transactions.
#[derive(Debug, Clone)]
pub struct BatchSettlementDepositPayload {
    /// Shared pre-signed channel artifacts.
    pub book: Arc<ChannelBook>,
}

impl BatchSettlementDepositPayload {
    /// Creates the payload bound to a channel book.
    #[must_use]
    pub const fn new(book: Arc<ChannelBook>) -> Self {
        Self { book }
    }
}

impl Payload for BatchSettlementDepositPayload {
    fn name(&self) -> &'static str {
        "batch_settlement_deposit"
    }

    fn uses_runner_recipient(&self) -> bool {
        false
    }

    fn generate(&self, _rng: &mut SeededRng, from: Address, _to: Address) -> TransactionRequest {
        self.book.deposit_request(from).unwrap_or_else(|| self.book.exhausted_fallback(from))
    }
}

/// Generates x402 batch-settlement `batch_settlement_settle` transactions.
#[derive(Debug, Clone)]
pub struct BatchSettlementSettlePayload {
    /// Shared pre-signed channel artifacts.
    pub book: Arc<ChannelBook>,
}

impl BatchSettlementSettlePayload {
    /// Creates the payload bound to a channel book.
    #[must_use]
    pub const fn new(book: Arc<ChannelBook>) -> Self {
        Self { book }
    }
}

impl Payload for BatchSettlementSettlePayload {
    fn name(&self) -> &'static str {
        "batch_settlement_settle"
    }

    fn uses_runner_recipient(&self) -> bool {
        false
    }

    fn generate(&self, _rng: &mut SeededRng, from: Address, _to: Address) -> TransactionRequest {
        self.book.settle_request(from).unwrap_or_else(|| self.book.exhausted_fallback(from))
    }
}

/// Generates x402 batch-settlement `batch_settlement_refund` transactions.
#[derive(Debug, Clone)]
pub struct BatchSettlementRefundPayload {
    /// Shared pre-signed channel artifacts.
    pub book: Arc<ChannelBook>,
}

impl BatchSettlementRefundPayload {
    /// Creates the payload bound to a channel book.
    #[must_use]
    pub const fn new(book: Arc<ChannelBook>) -> Self {
        Self { book }
    }
}

impl Payload for BatchSettlementRefundPayload {
    fn name(&self) -> &'static str {
        "batch_settlement_refund"
    }

    fn uses_runner_recipient(&self) -> bool {
        false
    }

    fn generate(&self, _rng: &mut SeededRng, from: Address, _to: Address) -> TransactionRequest {
        self.book.refund_request(from).unwrap_or_else(|| self.book.exhausted_fallback(from))
    }
}

/// Builds an immutable [`ChannelConfig`] with the load-tester role model
/// (`payer = payerAuthorizer = receiverAuthorizer = sender`).
#[must_use]
pub fn make_channel_config(
    sender: Address,
    receiver: Address,
    token: Address,
    withdraw_delay: u64,
    salt: B256,
) -> ChannelConfig {
    ChannelConfig {
        payer: sender,
        payerAuthorizer: sender,
        receiver,
        receiverAuthorizer: sender,
        token,
        withdrawDelay: U40::from(withdraw_delay),
        salt,
    }
}

/// Derives the deterministic receiver address for one of a sender's channels.
///
/// The receiver needs no signing key (it never submits a transaction), so it is a pure hash of
/// `(sender, run_salt, channel_index)`, guaranteeing distinct `receivers[receiver][token]` slots.
#[must_use]
pub fn derive_receiver(sender: Address, run_salt: B256, channel_index: u64) -> Address {
    let mut buf = Vec::with_capacity(20 + 32 + 8);
    buf.extend_from_slice(sender.as_slice());
    buf.extend_from_slice(run_salt.as_slice());
    buf.extend_from_slice(&channel_index.to_be_bytes());
    Address::from_slice(&keccak256(buf)[12..])
}

/// Derives the deterministic channel salt for one of a sender's channels.
#[must_use]
pub fn derive_channel_salt(sender: Address, run_salt: B256, channel_index: u64) -> B256 {
    let mut buf = Vec::with_capacity(20 + 32 + 8 + 1);
    buf.push(0x01);
    buf.extend_from_slice(sender.as_slice());
    buf.extend_from_slice(run_salt.as_slice());
    buf.extend_from_slice(&channel_index.to_be_bytes());
    keccak256(buf)
}

#[cfg(test)]
mod tests {
    use alloy_primitives::address;
    use alloy_signer::SignerSync;

    use super::*;

    #[test]
    fn voucher_signature_recovers_to_signer() {
        let signer = PrivateKeySigner::random();
        let domain =
            SettlementDomain::new(8453, address!("00000000000000000000000000000000000000ff"));
        let config = make_channel_config(
            signer.address(),
            address!("00000000000000000000000000000000000000a1"),
            address!("00000000000000000000000000000000000000b2"),
            3600,
            B256::repeat_byte(0x11),
        );
        let channel_id = domain.channel_id(&config);
        let digest = domain.voucher_digest(channel_id, 1_000_000);

        let sig = signer.sign_hash_sync(&digest).expect("sign");
        let recovered = sig.recover_address_from_prehash(&digest).expect("recover");
        assert_eq!(recovered, signer.address(), "voucher digest must recover to the payer key");
    }

    #[test]
    fn channel_id_is_deterministic() {
        let domain =
            SettlementDomain::new(1337, address!("00000000000000000000000000000000000000ff"));
        let config = make_channel_config(
            address!("00000000000000000000000000000000000000a1"),
            address!("00000000000000000000000000000000000000a2"),
            address!("00000000000000000000000000000000000000a3"),
            900,
            B256::repeat_byte(0x22),
        );
        assert_eq!(domain.channel_id(&config), domain.channel_id(&config));
    }

    #[test]
    fn claim_gas_limit_is_capped_at_base_transaction_maximum() {
        assert_eq!(claim_gas_limit(1), 197_000);
        assert_eq!(claim_gas_limit(100), 4_850_000);
        assert_eq!(claim_gas_limit(400), 16_777_216);
        assert_eq!(claim_gas_limit(1_000), 16_777_216);
    }

    #[test]
    fn erc3009_nonce_matches_collector_scheme() {
        let channel_id = B256::repeat_byte(0xcd);
        let salt = U256::from(7u64);
        // keccak256(abi.encode(channelId, salt)) == keccak256(channelId ++ salt-as-word)
        let expected = {
            let mut buf = Vec::new();
            buf.extend_from_slice(channel_id.as_slice());
            buf.extend_from_slice(&salt.to_be_bytes::<32>());
            keccak256(buf)
        };
        assert_eq!(erc3009_nonce(channel_id, salt), expected);
    }

    #[test]
    fn collector_data_matches_solidity_abi_encode() {
        let signature = Bytes::from(vec![0xabu8; 65]);
        let encoded = encode_collector_data(7, 11, U256::from(13), signature);

        assert_eq!(U256::from_be_slice(&encoded[0..32]), U256::from(7));
        assert_eq!(U256::from_be_slice(&encoded[32..64]), U256::from(11));
        assert_eq!(U256::from_be_slice(&encoded[64..96]), U256::from(13));
        assert_eq!(U256::from_be_slice(&encoded[96..128]), U256::from(128));
        assert_eq!(U256::from_be_slice(&encoded[128..160]), U256::from(65));
        assert_eq!(&encoded[160..225], &[0xabu8; 65]);
    }

    #[test]
    fn fresh_deposits_are_interleaved_at_configured_ratio() {
        let fresh = (0..100).filter(|operation| should_open_fresh(*operation, 0.25)).count();
        assert_eq!(fresh, 25);
        assert!(!should_open_fresh(0, 0.0));
        assert!(should_open_fresh(0, 1.0));
    }
}
