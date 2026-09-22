//! Known revert classification for proof contract transactions.

use base_proof_contracts::{
    already_proven_selector, game_already_exists_selector, invalid_parent_game_selector,
    invalid_signer_selector, l1_origin_too_old_selector,
};
use base_tx_manager::TxManagerError;
use thiserror::Error;

use crate::ProofSubmissionError;

const GAME_ALREADY_EXISTS: &str = "GameAlreadyExists";
const ALREADY_PROVEN: &str = "AlreadyProven";
const L1_ORIGIN_TOO_OLD: &str = "L1OriginTooOld";
const INVALID_PARENT_GAME: &str = "InvalidParentGame";
const INVALID_SIGNER: &str = "InvalidSigner";

/// Known non-retryable contract reverts shared by proof-related onchain transactions.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Error)]
pub enum KnownRevert {
    /// The target dispute game already exists for the submitted parameters.
    #[error("game already exists")]
    GameAlreadyExists,

    /// A proof of this type has already been attached to the dispute game.
    #[error("proof already verified")]
    ProofAlreadyVerified,

    /// The proof's L1 origin is older than the EIP-2935 history window.
    #[error("l1 origin too old")]
    L1OriginTooOld,

    /// The parent game is no longer valid on-chain.
    #[error("invalid parent game")]
    InvalidParentGame,

    /// The proof signer is not valid on-chain.
    #[error("invalid signer")]
    InvalidSigner,
}

impl KnownRevert {
    /// Classifies a transaction manager error as a known contract revert.
    ///
    /// The classifier checks structured execution reverts first, matching both
    /// decoded custom-error names and raw selector data. It also preserves the
    /// previous fallback behavior of scanning non-revert transaction manager error
    /// display strings for known custom-error names and selectors.
    pub fn from_tx_manager_error(err: &TxManagerError) -> Option<Self> {
        let known_reverts = [
            (game_already_exists_selector(), GAME_ALREADY_EXISTS, Self::GameAlreadyExists),
            (already_proven_selector(), ALREADY_PROVEN, Self::ProofAlreadyVerified),
            (l1_origin_too_old_selector(), L1_ORIGIN_TOO_OLD, Self::L1OriginTooOld),
            (invalid_parent_game_selector(), INVALID_PARENT_GAME, Self::InvalidParentGame),
            (invalid_signer_selector(), INVALID_SIGNER, Self::InvalidSigner),
        ];

        if let TxManagerError::ExecutionReverted { reason, data } = err {
            for (selector, name, revert) in known_reverts {
                if reason.as_deref().is_some_and(|reason| reason.contains(name))
                    || data.as_ref().is_some_and(|data| data.starts_with(&selector))
                {
                    return Some(revert);
                }
            }
            return None;
        }

        let msg = err.to_string();
        known_reverts.into_iter().find_map(|(selector, name, revert)| {
            (msg.contains(&alloy_primitives::hex::encode(selector)) || msg.contains(name))
                .then_some(revert)
        })
    }
}

impl From<KnownRevert> for ProofSubmissionError {
    fn from(revert: KnownRevert) -> Self {
        match revert {
            KnownRevert::GameAlreadyExists => Self::GameAlreadyExists,
            KnownRevert::ProofAlreadyVerified => Self::ProofAlreadyVerified,
            KnownRevert::L1OriginTooOld => Self::L1OriginTooOld,
            KnownRevert::InvalidParentGame => Self::InvalidParentGame,
            KnownRevert::InvalidSigner => Self::InvalidSigner,
        }
    }
}

impl From<TxManagerError> for ProofSubmissionError {
    fn from(err: TxManagerError) -> Self {
        if let Some(revert) = KnownRevert::from_tx_manager_error(&err) {
            return Self::from(revert);
        }

        Self::TxManager(err)
    }
}

#[cfg(test)]
mod tests {
    use alloy_primitives::Bytes;
    use base_tx_manager::TxManagerError;

    use super::*;

    #[derive(Debug)]
    struct KnownRevertCase {
        name: &'static str,
        selector: [u8; 4],
        expected: KnownRevert,
    }

    fn known_revert_cases() -> [KnownRevertCase; 5] {
        [
            KnownRevertCase {
                name: GAME_ALREADY_EXISTS,
                selector: game_already_exists_selector(),
                expected: KnownRevert::GameAlreadyExists,
            },
            KnownRevertCase {
                name: ALREADY_PROVEN,
                selector: already_proven_selector(),
                expected: KnownRevert::ProofAlreadyVerified,
            },
            KnownRevertCase {
                name: L1_ORIGIN_TOO_OLD,
                selector: l1_origin_too_old_selector(),
                expected: KnownRevert::L1OriginTooOld,
            },
            KnownRevertCase {
                name: INVALID_PARENT_GAME,
                selector: invalid_parent_game_selector(),
                expected: KnownRevert::InvalidParentGame,
            },
            KnownRevertCase {
                name: INVALID_SIGNER,
                selector: invalid_signer_selector(),
                expected: KnownRevert::InvalidSigner,
            },
        ]
    }

    fn assert_classifies(err: TxManagerError, expected: Option<KnownRevert>, scenario: &str) {
        let result = KnownRevert::from_tx_manager_error(&err);
        assert_eq!(result, expected, "{scenario}: got {result:?}");
    }

    #[test]
    fn classify_tx_manager_error_maps_known_reverts() {
        for case in known_revert_cases() {
            assert_classifies(
                TxManagerError::Rpc(format!(
                    "execution reverted: 0x{}",
                    alloy_primitives::hex::encode(case.selector)
                )),
                Some(case.expected),
                "selector hex in Rpc message",
            );
            assert_classifies(
                TxManagerError::Rpc(format!("{}()", case.name)),
                Some(case.expected),
                "error name in Rpc message",
            );
            assert_classifies(
                TxManagerError::ExecutionReverted {
                    reason: Some(format!("{}()", case.name)),
                    data: None,
                },
                Some(case.expected),
                "reason string contains name",
            );
            assert_classifies(
                TxManagerError::ExecutionReverted {
                    reason: None,
                    data: Some(Bytes::from(case.selector.to_vec())),
                },
                Some(case.expected),
                "raw data contains selector",
            );
        }
    }

    #[test]
    fn classify_tx_manager_error_leaves_unrelated_reverts_as_tx_manager_errors() {
        let result = KnownRevert::from_tx_manager_error(&TxManagerError::ExecutionReverted {
            reason: Some("SomeOtherError()".to_string()),
            data: Some(Bytes::from(vec![0xde, 0xad, 0xbe, 0xef])),
        });

        assert_eq!(result, None);
    }

    #[test]
    fn classify_tx_manager_error_leaves_non_reverts_as_tx_manager_errors() {
        let result = KnownRevert::from_tx_manager_error(&TxManagerError::NonceTooLow);

        assert_eq!(result, None);
    }
}
