//! Contains peer scoring types.

use derive_more::{Display, FromStr};
use libp2p::gossipsub::{PeerScoreParams, PeerScoreThresholds};

/// The peer scoring level is used to determine
/// how peers are scored based on their behavior.
#[derive(Debug, FromStr, Display, Default, Clone, Copy, PartialEq, Eq)]
pub enum PeerScoreLevel {
    /// No peer scoring is applied.
    #[default]
    Off,
    /// Light peer scoring is applied.
    Light,
}

impl PeerScoreLevel {
    /// Decay to zero is the decay factor for a peer's score to zero.
    pub const DECAY_TO_ZERO: f64 = 0.01;

    /// Mesh weight is the weight of the mesh delivery topic.
    pub const MESH_WEIGHT: f64 = -0.7;

    /// Max in mesh score is the maximum score for being in the mesh.
    pub const MAX_IN_MESH_SCORE: f64 = 10.0;

    /// Decay epoch is the number of epochs to decay the score over.
    pub const DECAY_EPOCH: f64 = 5.0;

    /// Helper function to calculate the decay factor for a given duration.
    /// The decay factor is calculated using the formula:
    /// `decay_factor = (1 - decay_to_zero) ^ (duration / slot)`.
    pub fn score_decay(duration: std::time::Duration, slot: std::time::Duration) -> f64 {
        let num_of_times = duration.as_secs() / slot.as_secs();
        (1.0 - Self::DECAY_TO_ZERO).powf(1.0 / num_of_times as f64)
    }

    /// Default peer score thresholds.
    pub const DEFAULT_PEER_SCORE_THRESHOLDS: PeerScoreThresholds = PeerScoreThresholds {
        gossip_threshold: -10.0,
        publish_threshold: -40.0,
        graylist_threshold: -40.0,
        accept_px_threshold: 20.0,
        opportunistic_graft_threshold: 0.05,
    };

    /// Returns a cap on the in mesh score.
    /// The cap is calculated based on the slot duration.
    /// The formula used is:
    /// `cap = (3600 * time.Second) / slot`.
    pub fn in_mesh_cap(slot: std::time::Duration) -> f64 {
        (3600 * std::time::Duration::from_secs(1)).as_secs_f64() / slot.as_secs_f64()
    }

    /// Returns the [`PeerScoreParams`] for the given peer scoring level.
    ///
    /// # Arguments
    /// * `block_time` - The block time in seconds.
    pub fn to_params(&self, block_time: u64) -> Option<PeerScoreParams> {
        let slot = std::time::Duration::from_secs(block_time);
        debug!(target: "scoring", slot = ?slot, "Slot duration");
        let epoch = slot * 6;
        let ten_epochs = epoch * 10;
        let one_hundred_epochs = epoch * 100;
        let penalty_decay = Self::score_decay(ten_epochs, slot);
        match self {
            Self::Off => None,
            Self::Light => Some(PeerScoreParams {
                topics: Default::default(),
                topic_score_cap: 34.0,
                app_specific_weight: 1.0,
                ip_colocation_factor_weight: -35.0,
                ip_colocation_factor_threshold: 10.0,
                ip_colocation_factor_whitelist: Default::default(),
                behaviour_penalty_weight: -16.0,
                behaviour_penalty_threshold: 6.0,
                behaviour_penalty_decay: penalty_decay,
                decay_interval: slot,
                decay_to_zero: Self::DECAY_TO_ZERO,
                retain_score: one_hundred_epochs,
                slow_peer_weight: -0.2,   // default
                slow_peer_threshold: 0.0, // default
                slow_peer_decay: 0.2,
            }),
        }
    }

    /// Returns the [`PeerScoreThresholds`].
    pub const fn thresholds() -> PeerScoreThresholds {
        Self::DEFAULT_PEER_SCORE_THRESHOLDS
    }
}

#[cfg(test)]
mod tests {
    use super::PeerScoreLevel;

    #[test]
    fn light_scoring_has_no_topic_scores() {
        let params = PeerScoreLevel::Light.to_params(2).expect("light scoring should have params");

        assert!(params.topics.is_empty());
    }
}
