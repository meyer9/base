//! Transaction payload types for different workload scenarios.

use alloy_primitives::Address;
use alloy_rpc_types::TransactionRequest;

use crate::workload::SeededRng;

mod transfer;
pub use transfer::TransferPayload;

mod calldata;
pub use calldata::CalldataPayload;

mod erc20;
pub use erc20::Erc20Payload;

mod storage;
pub use storage::StoragePayload;

mod precompile;
pub use precompile::{PrecompilePayload, parse_precompile_id};

mod looper;
pub use looper::PrecompileLooper;

mod uniswap;
pub use uniswap::UniswapV3Payload;

mod aerodrome;
pub use aerodrome::AerodromeClPayload;

mod b20;
pub use b20::B20TransferPayload;
pub(crate) use b20::{b20_salt_for, b20_token_for};

mod b20_evm;
pub use b20_evm::B20EvmTransferPayload;

mod batch_settlement;
pub use batch_settlement::{
    BatchSettlementClaimPayload, BatchSettlementClaimWithSignaturePayload,
    BatchSettlementDepositPayload, BatchSettlementRefundPayload, BatchSettlementSettlePayload,
    ChannelBook, ChannelConfig, ChannelGroup, DEPOSIT_OPEN_GAS_LIMIT, DEPOSIT_TOPUP_GAS_LIMIT,
    DepositAuth, FreshChannel, REFUND_GAS_LIMIT, Rung, SETTLE_GAS_LIMIT, SETTLEMENT_DOMAIN_NAME,
    SETTLEMENT_DOMAIN_VERSION, SenderChannels, SettlementDomain, TOKEN_DOMAIN_NAME,
    TOKEN_DOMAIN_VERSION, TokenDomain, claim_gas_limit, derive_channel_salt, derive_receiver,
    encode_collector_data, encode_deposit_call, erc3009_nonce, make_channel_config, sign_digest,
};

mod osaka;
pub use osaka::OsakaPayload;

/// A transaction payload generator.
pub trait Payload: Send + Sync + std::fmt::Debug {
    /// Returns the name of this payload type.
    fn name(&self) -> &'static str;

    /// Returns true when this payload uses the runner-supplied recipient address.
    fn uses_runner_recipient(&self) -> bool;

    /// Generates a transaction request.
    fn generate(&self, rng: &mut SeededRng, from: Address, to: Address) -> TransactionRequest;
}
