//! An abstraction for the driver's block executor.
//!
//! This module provides the [`Executor`] trait which abstracts block execution for the driver.
//! The executor is responsible for building and executing blocks from payload attributes,
//! maintaining safe head state, and computing output roots for the execution results.

use alloc::boxed::Box;
use core::error::Error;

use alloy_consensus::{Header, Sealed};
use alloy_primitives::B256;
use async_trait::async_trait;
use base_common_rpc_types_engine::BasePayloadAttributes;
use base_proof_executor::BlockBuildingOutcome;

/// The action the driver takes after a payload execution failure.
///
/// This is the canonical transition decision for legacy pre-Holocene payload
/// failures and Holocene deposit-only recovery.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PayloadExecutionFailureAction {
    /// Discard a failed pre-Holocene payload and continue derivation.
    DiscardPreHolocene,
    /// Flush the channel and retry the payload with deposits only.
    RetryDepositOnly,
    /// Stop derivation because the execution failure is not recoverable.
    Abort,
}

impl PayloadExecutionFailureAction {
    /// Selects the transition for a failed payload execution.
    pub const fn from_execution_failure(
        holocene_active: bool,
        deposit_only_retryable: bool,
    ) -> Self {
        if !holocene_active {
            Self::DiscardPreHolocene
        } else if deposit_only_retryable {
            Self::RetryDepositOnly
        } else {
            Self::Abort
        }
    }
}

/// Executor trait for block execution in the driver pipeline.
///
/// This trait abstracts the block execution functionality needed by the driver.
/// Implementations are responsible for:
/// - Building blocks from payload attributes
/// - Maintaining execution state and safe head tracking
/// - Computing output roots after block execution
/// - Handling execution errors and recovery scenarios
#[async_trait]
pub trait Executor {
    /// The error type for the Executor.
    type Error: Error;

    /// Returns whether the provided error should trigger Holocene deposit-only recovery.
    fn is_deposit_only_retryable(_error: &Self::Error) -> bool {
        false
    }

    /// Waits for the executor to be ready for block execution.
    async fn wait_until_ready(&mut self);

    /// Updates the safe head to the specified header.
    fn update_safe_head(&mut self, header: Sealed<Header>);

    /// Execute the given payload attributes to build and execute a block.
    async fn execute_payload(
        &mut self,
        attributes: BasePayloadAttributes,
    ) -> Result<BlockBuildingOutcome, Self::Error>;

    /// Computes the output root for the most recently executed block.
    fn compute_output_root(&mut self) -> Result<B256, Self::Error>;
}

#[cfg(test)]
mod tests {
    use super::PayloadExecutionFailureAction;

    #[test]
    fn selects_the_supported_execution_failure_transition() {
        assert_eq!(
            PayloadExecutionFailureAction::from_execution_failure(false, true),
            PayloadExecutionFailureAction::DiscardPreHolocene
        );
        assert_eq!(
            PayloadExecutionFailureAction::from_execution_failure(true, true),
            PayloadExecutionFailureAction::RetryDepositOnly
        );
        assert_eq!(
            PayloadExecutionFailureAction::from_execution_failure(true, false),
            PayloadExecutionFailureAction::Abort
        );
    }
}
