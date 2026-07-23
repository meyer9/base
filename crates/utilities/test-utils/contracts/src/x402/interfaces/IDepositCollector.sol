// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

// VENDORED from x402 (github.com/x402/x402, contracts/evm) @ 0a604079a for the base batch-settlement
// load-test devnet fixture. Do not edit here; update by re-copying from x402 when the source changes.

/// @title IDepositCollector
/// @notice Interface for pluggable deposit collectors used by x402BatchSettlement.
/// @dev Collectors handle the token transfer mechanics (ERC-3009, Permit2, etc.)
///      while `x402BatchSettlement` verifies actual token receipt via balance checks.
///      Collectors MUST transfer tokens to `msg.sender` (the calling `x402BatchSettlement`).
interface IDepositCollector {
    /// @notice Pull tokens from payer to the calling `x402BatchSettlement` contract.
    /// @param payer The address that owns the tokens being deposited
    /// @param token The ERC-20 token address
    /// @param amount The exact amount of tokens to transfer
    /// @param channelId The channel identifier (used by Permit2 collectors for witness binding)
    /// @param collectorData Opaque bytes containing collector-specific parameters (signatures, nonces, etc.)
    function collect(
        address payer,
        address token,
        uint256 amount,
        bytes32 channelId,
        bytes calldata collectorData
    ) external;
}
