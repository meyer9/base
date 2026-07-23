#![doc = include_str!("../README.md")]
#![cfg_attr(docsrs, feature(doc_cfg, doc_auto_cfg))]
#![cfg_attr(not(test), warn(unused_crate_dependencies))]

mod config;
pub use config::{
    BatchSettlementConfig, OsakaTarget, PrecompileTarget, RealTokenAcquisitionConfig,
    RealTokenPairTokenConfig, RealTokenSetupConfig, TestConfig, TxTypeConfig, WeightedTxType,
    WorkloadConfig,
};

mod executor;
pub use executor::{
    LoadTestCleanupSummary, LoadTestDisplayConfig, LoadTestExecutor, LoadTestRunHooks,
    LoadTestRunOptions, LoadTestRunOutput, LoadTestSetupAmounts, SignalHandlerGuard,
};

mod utils;
pub use utils::{BaselineError, Result};

mod rpc;
pub use rpc::{
    BaseFeeExt, BatchRpcClient, BatchSendResult, QueryProvider, RPC_TIMEOUT, RpcProviders,
    RpcResultExt, TxpoolAdminClient, WalletProvider, create_wallet_provider,
};

mod metrics;
pub use metrics::{
    BlockLoadMetrics, BlockRange, ConfigSummary, FlashblocksLatencyMetrics, GasMetrics,
    LatencyMetrics, MetricsAggregator, MetricsCollector, MetricsSummary, ReceiptCoverage,
    RollingWindow, SubmissionStats, ThroughputMetrics, ThroughputPercentiles, ThroughputSample,
    TransactionMetrics,
};

mod workload;
pub use workload::{
    AccountPool, AerodromeClPayload, B20EvmTransferPayload, B20TransferPayload,
    BatchSettlementClaimPayload, BatchSettlementClaimWithSignaturePayload,
    BatchSettlementDepositPayload, BatchSettlementRefundPayload, BatchSettlementSettlePayload,
    CalldataPayload, ChannelBook, ChannelConfig, ChannelGroup, DEPOSIT_OPEN_GAS_LIMIT,
    DEPOSIT_TOPUP_GAS_LIMIT, DepositAuth, Erc20Payload, FreshChannel, FundedAccount, KeyStream,
    OsakaPayload, Payload, PrecompileLooper, PrecompilePayload, REFUND_GAS_LIMIT, Rung,
    SETTLE_GAS_LIMIT, SETTLEMENT_DOMAIN_NAME, SETTLEMENT_DOMAIN_VERSION, SeededRng,
    SenderChannels, SettlementDomain, StoragePayload, TOKEN_DOMAIN_NAME, TOKEN_DOMAIN_VERSION,
    TokenDomain, TransferPayload, UniswapV3Payload, WorkloadGenerator, claim_gas_limit,
    derive_channel_salt, derive_receiver, encode_collector_data, encode_deposit_call, erc3009_nonce,
    make_channel_config, parse_precompile_id, sign_digest,
};

mod runner;
pub use runner::{
    AdaptiveBackoff, BatchSettlementParams, BatchTxError, BlockObservation, BlockReceipt,
    BlockWatcher,
    DEFAULT_MAX_GAS_PRICE, DisplaySnapshot, FlashblockInclusion, FlashblockWatcher, LoadConfig,
    LoadRunner, LoadTestDisplay, MAX_FEE_BASE_FEE_MULTIPLIER, MAX_SENDER_WORKER_COUNT,
    MAX_SIGNER_WORKER_COUNT, PipelineQueue, PipelineStartConfig, PreparedBatch,
    PreparedTransaction, QueuedSubmitFailures, RateLimiter, RealTokenAcquisition,
    RealTokenPairTokenSetup, RealTokenRecoverySummary, RealTokenSetup, ResultsTracker,
    SENDER_WORKERS_PER_RPC, SIGNER_WORKERS_PER_RPC, SUBMIT_BATCH_QUEUE_BUFFER, SUBMIT_MAX_ATTEMPTS,
    SenderContext, SentTransaction, SignedBatch, SignedTransaction, SignerContext,
    SubmissionPipeline, SubmitEvent, TxConfig, TxType,
};
