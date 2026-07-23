//! Workload generation, account management, and transaction payloads.

mod accounts;
pub use accounts::{AccountPool, FundedAccount};

mod seeded;
pub use seeded::SeededRng;

mod key_stream;
pub use key_stream::KeyStream;

mod payloads;
pub use payloads::{
    AerodromeClPayload, B20EvmTransferPayload, B20TransferPayload, BatchSettlementClaimPayload,
    BatchSettlementClaimWithSignaturePayload, BatchSettlementDepositPayload,
    BatchSettlementRefundPayload, BatchSettlementSettlePayload, CalldataPayload, ChannelBook,
    ChannelConfig, ChannelGroup, DEPOSIT_OPEN_GAS_LIMIT, DEPOSIT_TOPUP_GAS_LIMIT, DepositAuth,
    Erc20Payload, FreshChannel, OsakaPayload, Payload, PrecompileLooper, PrecompilePayload,
    REFUND_GAS_LIMIT, Rung, SETTLE_GAS_LIMIT, SETTLEMENT_DOMAIN_NAME, SETTLEMENT_DOMAIN_VERSION,
    SenderChannels, SettlementDomain, StoragePayload, TOKEN_DOMAIN_NAME, TOKEN_DOMAIN_VERSION,
    TokenDomain, TransferPayload, UniswapV3Payload, claim_gas_limit, derive_channel_salt,
    derive_receiver, encode_collector_data, encode_deposit_call, erc3009_nonce, make_channel_config,
    parse_precompile_id, sign_digest,
};
pub(crate) use payloads::{b20_salt_for, b20_token_for};

mod generator;
pub use generator::WorkloadGenerator;
