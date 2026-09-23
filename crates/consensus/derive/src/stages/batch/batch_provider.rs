//! This module contains the [`BatchProvider`] stage.

use alloc::{boxed::Box, sync::Arc};
use core::fmt::Debug;

use alloy_eips::BlockNumHash;
use async_trait::async_trait;
use base_common_genesis::{RollupConfig, SystemConfig};
use base_protocol::{BlockInfo, L2BlockInfo, SingleBatch};

use super::NextBatchProvider;
use crate::{
    AttributesProvider, BatchQueue, BatchValidator, L2ChainProvider, OriginAdvancer,
    OriginProvider, PipelineError, PipelineResult, StageReset,
};

/// The concrete owner of batch derivation for the current L1 origin.
///
/// The provider starts uninitialized so its first origin can select the matching protocol
/// rules. After that, exactly one stage owns the preceding pipeline and its L1-window state.
/// Crossing the Holocene boundary moves that ownership atomically between the two stages.
#[derive(Debug)]
pub enum BatchProviderState<P, F>
where
    P: NextBatchProvider + OriginAdvancer + OriginProvider + StageReset + Debug,
    F: L2ChainProvider + Debug,
{
    /// No origin has selected a batch derivation stage yet.
    Uninitialized(P),
    /// The pre-Holocene queue owns batch derivation state.
    BatchQueue(BatchQueue<P, F>),
    /// The Holocene validator owns batch derivation state.
    BatchValidator(BatchValidator<P, F>),
    /// A transition is moving the preceding stage to its next canonical owner.
    ///
    /// This is only installed synchronously while [`BatchProvider::attempt_update`] rebuilds the
    /// active stage and is never observable through the public API.
    Transitioning,
}

/// [`BatchProvider`] selects the one batch-derivation stage that owns the current L1 origin.
///
/// Before Holocene, [`BatchQueue`] owns ordering and span expansion. At and after Holocene,
/// [`BatchValidator`] owns validation while [`BatchStream`] owns span expansion. An L1 reorg
/// across the activation boundary transfers the shared L1-window state back to the appropriate
/// owner.
#[derive(Debug)]
pub struct BatchProvider<P, F>
where
    P: NextBatchProvider + OriginAdvancer + OriginProvider + StageReset + Debug,
    F: L2ChainProvider + Clone + Debug,
{
    /// The rollup configuration.
    pub cfg: Arc<RollupConfig>,
    /// The L2 chain provider.
    pub provider: F,
    /// The sole owner of the preceding stage and batch derivation state.
    state: BatchProviderState<P, F>,
}

impl<P, F> BatchProvider<P, F>
where
    P: NextBatchProvider + OriginAdvancer + OriginProvider + StageReset + Debug,
    F: L2ChainProvider + Clone + Debug,
{
    /// Creates a new [`BatchProvider`] with the given configuration and previous stage.
    pub const fn new(cfg: Arc<RollupConfig>, prev: P, provider: F) -> Self {
        Self { cfg, provider, state: BatchProviderState::Uninitialized(prev) }
    }

    /// Returns the stage that currently owns batch derivation state.
    pub const fn state(&self) -> &BatchProviderState<P, F> {
        &self.state
    }

    /// Selects the canonical batch-derivation owner for the current origin.
    ///
    /// The transition preserves only the L1-window state shared by both stage contracts. Pending
    /// batches remain with their former owner because they are governed by different pre- and
    /// post-Holocene validity rules.
    pub fn attempt_update(&mut self) -> PipelineResult<()> {
        let origin = self.origin().ok_or(PipelineError::MissingOrigin.crit())?;
        let holocene_active = self.cfg.is_holocene_active(origin.timestamp);
        let state = core::mem::replace(&mut self.state, BatchProviderState::Transitioning);

        self.state = match (state, holocene_active) {
            (BatchProviderState::Uninitialized(prev), false) => BatchProviderState::BatchQueue(
                BatchQueue::new(Arc::clone(&self.cfg), prev, self.provider.clone()),
            ),
            (BatchProviderState::Uninitialized(prev), true) => BatchProviderState::BatchValidator(
                BatchValidator::new(Arc::clone(&self.cfg), prev, self.provider.clone()),
            ),
            (BatchProviderState::BatchQueue(batch_queue), true) => {
                let mut batch_validator = BatchValidator::new(
                    Arc::clone(&self.cfg),
                    batch_queue.prev,
                    self.provider.clone(),
                );
                batch_validator.l1_blocks = batch_queue.l1_blocks;
                batch_validator.origin = batch_queue.origin;
                BatchProviderState::BatchValidator(batch_validator)
            }
            (BatchProviderState::BatchValidator(batch_validator), false) => {
                let mut batch_queue = BatchQueue::new(
                    Arc::clone(&self.cfg),
                    batch_validator.prev,
                    self.provider.clone(),
                );
                batch_queue.l1_blocks = batch_validator.l1_blocks;
                batch_queue.origin = batch_validator.origin;
                BatchProviderState::BatchQueue(batch_queue)
            }
            (state @ BatchProviderState::BatchQueue(_), false)
            | (state @ BatchProviderState::BatchValidator(_), true) => state,
            (BatchProviderState::Transitioning, _) => {
                unreachable!("batch provider state is only transitioning inside attempt_update")
            }
        };

        Ok(())
    }
}

#[async_trait]
impl<P, F> OriginAdvancer for BatchProvider<P, F>
where
    P: NextBatchProvider + OriginAdvancer + OriginProvider + StageReset + Send + Debug,
    F: L2ChainProvider + Clone + Send + Debug,
{
    async fn advance_origin(&mut self) -> PipelineResult<()> {
        self.attempt_update()?;

        match &mut self.state {
            BatchProviderState::BatchQueue(stage) => stage.advance_origin().await,
            BatchProviderState::BatchValidator(stage) => stage.advance_origin().await,
            BatchProviderState::Uninitialized(_) | BatchProviderState::Transitioning => {
                unreachable!("attempt_update always selects an active batch derivation stage")
            }
        }
    }
}

impl<P, F> OriginProvider for BatchProvider<P, F>
where
    P: NextBatchProvider + OriginAdvancer + OriginProvider + StageReset + Debug,
    F: L2ChainProvider + Clone + Debug,
{
    fn origin(&self) -> Option<BlockInfo> {
        match &self.state {
            BatchProviderState::Uninitialized(stage) => stage.origin(),
            BatchProviderState::BatchQueue(stage) => stage.origin(),
            BatchProviderState::BatchValidator(stage) => stage.origin(),
            BatchProviderState::Transitioning => {
                unreachable!("batch provider state is only transitioning inside attempt_update")
            }
        }
    }
}

#[async_trait]
impl<P, F> StageReset for BatchProvider<P, F>
where
    P: NextBatchProvider + OriginAdvancer + OriginProvider + StageReset + Send + Debug,
    F: L2ChainProvider + Clone + Send + Debug,
{
    async fn reset(
        &mut self,
        l1_origin: BlockNumHash,
        system_config: SystemConfig,
    ) -> PipelineResult<()> {
        self.attempt_update()?;

        match &mut self.state {
            BatchProviderState::BatchQueue(stage) => stage.reset(l1_origin, system_config).await,
            BatchProviderState::BatchValidator(stage) => {
                stage.reset(l1_origin, system_config).await
            }
            BatchProviderState::Uninitialized(_) | BatchProviderState::Transitioning => {
                unreachable!("attempt_update always selects an active batch derivation stage")
            }
        }
    }

    async fn activate(&mut self) -> PipelineResult<()> {
        self.attempt_update()?;

        match &mut self.state {
            BatchProviderState::BatchQueue(stage) => stage.activate().await,
            BatchProviderState::BatchValidator(stage) => stage.activate().await,
            BatchProviderState::Uninitialized(_) | BatchProviderState::Transitioning => {
                unreachable!("attempt_update always selects an active batch derivation stage")
            }
        }
    }

    async fn flush_channel(&mut self) -> PipelineResult<()> {
        self.attempt_update()?;

        match &mut self.state {
            BatchProviderState::BatchQueue(stage) => stage.flush_channel().await,
            BatchProviderState::BatchValidator(stage) => stage.flush_channel().await,
            BatchProviderState::Uninitialized(_) | BatchProviderState::Transitioning => {
                unreachable!("attempt_update always selects an active batch derivation stage")
            }
        }
    }
}

#[async_trait]
impl<P, F> AttributesProvider for BatchProvider<P, F>
where
    P: NextBatchProvider + OriginAdvancer + OriginProvider + StageReset + Debug + Send,
    F: L2ChainProvider + Clone + Send + Debug,
{
    fn is_last_in_span(&self) -> bool {
        match &self.state {
            BatchProviderState::Uninitialized(_) => true,
            BatchProviderState::BatchQueue(stage) => stage.is_last_in_span(),
            BatchProviderState::BatchValidator(stage) => stage.is_last_in_span(),
            BatchProviderState::Transitioning => {
                unreachable!("batch provider state is only transitioning inside attempt_update")
            }
        }
    }

    async fn next_batch(&mut self, parent: L2BlockInfo) -> PipelineResult<SingleBatch> {
        self.attempt_update()?;

        match &mut self.state {
            BatchProviderState::BatchQueue(stage) => stage.next_batch(parent).await,
            BatchProviderState::BatchValidator(stage) => stage.next_batch(parent).await,
            BatchProviderState::Uninitialized(_) | BatchProviderState::Transitioning => {
                unreachable!("attempt_update always selects an active batch derivation stage")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::{sync::Arc, vec};

    use alloy_eips::BlockNumHash;
    use alloy_primitives::B256;
    use base_common_genesis::{BaseUpgradeConfig, RollupConfig, SystemConfig, UpgradeConfig};
    use base_protocol::{Batch, BlockInfo, L2BlockInfo, SingleBatch};

    use super::{BatchProvider, BatchProviderState};
    use crate::{
        AttributesProvider, PipelineError, StageReset,
        test_utils::{TestL2ChainProvider, TestNextBatchProvider},
        traits::OriginProvider,
    };

    #[test]
    fn test_batch_provider_validator_active() {
        let provider = TestNextBatchProvider::new(vec![]);
        let l2_provider = TestL2ChainProvider::default();
        let cfg = Arc::new(RollupConfig {
            upgrades: UpgradeConfig { holocene_time: Some(0), ..Default::default() },
            ..Default::default()
        });
        let mut batch_provider = BatchProvider::new(cfg, provider, l2_provider);

        assert!(batch_provider.attempt_update().is_ok());
        assert!(matches!(batch_provider.state(), BatchProviderState::BatchValidator(_)));
    }

    #[test]
    fn test_batch_provider_batch_queue_active() {
        let provider = TestNextBatchProvider::new(vec![]);
        let l2_provider = TestL2ChainProvider::default();
        let cfg = Arc::new(RollupConfig::default());
        let mut batch_provider = BatchProvider::new(cfg, provider, l2_provider);

        assert!(batch_provider.attempt_update().is_ok());
        assert!(matches!(batch_provider.state(), BatchProviderState::BatchQueue(_)));
    }

    #[test]
    fn test_batch_provider_transition_stage() {
        let provider = TestNextBatchProvider::new(vec![]);
        let l2_provider = TestL2ChainProvider::default();
        let cfg = Arc::new(RollupConfig {
            upgrades: UpgradeConfig { holocene_time: Some(2), ..Default::default() },
            ..Default::default()
        });
        let mut batch_provider = BatchProvider::new(cfg, provider, l2_provider);

        batch_provider.attempt_update().unwrap();

        // Update the L1 origin to Holocene activation.
        let BatchProviderState::BatchQueue(stage) = &mut batch_provider.state else {
            panic!("Expected BatchQueue");
        };
        stage.prev.origin = Some(BlockInfo { number: 1, timestamp: 2, ..Default::default() });

        // Transition to the BatchValidator stage.
        batch_provider.attempt_update().unwrap();
        assert!(matches!(batch_provider.state(), BatchProviderState::BatchValidator(_)));

        assert_eq!(batch_provider.origin().unwrap().number, 1);
    }

    #[test]
    fn test_holocene_transition_preserves_shared_l1_window() {
        let provider = TestNextBatchProvider::new(vec![]);
        let l2_provider = TestL2ChainProvider::default();
        let cfg = Arc::new(RollupConfig {
            upgrades: UpgradeConfig { holocene_time: Some(10), ..Default::default() },
            ..Default::default()
        });
        let mut batch_provider = BatchProvider::new(cfg, provider, l2_provider);

        batch_provider.attempt_update().unwrap();

        // Set origin and l1_blocks on the BatchQueue before the transition.
        let BatchProviderState::BatchQueue(stage) = &mut batch_provider.state else {
            panic!("Expected BatchQueue");
        };
        stage.origin = Some(BlockInfo { number: 5, timestamp: 3, ..Default::default() });
        stage.l1_blocks = vec![BlockInfo { number: 5, timestamp: 3, ..Default::default() }];

        // Update the L1 origin to Holocene activation.
        stage.prev.origin = Some(BlockInfo { number: 1, timestamp: 10, ..Default::default() });

        // Transition to the BatchValidator stage.
        batch_provider.attempt_update().unwrap();
        assert!(matches!(batch_provider.state(), BatchProviderState::BatchValidator(_)));

        // Assert that origin was transferred.
        let BatchProviderState::BatchValidator(bv) = batch_provider.state() else {
            panic!("Expected BatchValidator");
        };
        assert_eq!(bv.origin, Some(BlockInfo { number: 5, timestamp: 3, ..Default::default() }));
        assert_eq!(bv.l1_blocks, vec![BlockInfo { number: 5, timestamp: 3, ..Default::default() }]);
    }

    #[test]
    fn test_holocene_reorg_transfers_shared_l1_window_back_to_queue() {
        let provider = TestNextBatchProvider::new(vec![]);
        let l2_provider = TestL2ChainProvider::default();
        let cfg = Arc::new(RollupConfig {
            upgrades: UpgradeConfig { holocene_time: Some(2), ..Default::default() },
            ..Default::default()
        });
        let mut batch_provider = BatchProvider::new(cfg, provider, l2_provider);

        batch_provider.attempt_update().unwrap();

        let shared_origin = BlockInfo { number: 1, timestamp: 1, ..Default::default() };
        let shared_l1_blocks = vec![shared_origin];

        // Update the L1 origin to Holocene activation while the legacy queue owns a window.
        let BatchProviderState::BatchQueue(stage) = &mut batch_provider.state else {
            panic!("Expected BatchQueue");
        };
        stage.origin = Some(shared_origin);
        stage.l1_blocks = shared_l1_blocks.clone();
        stage.prev.origin = Some(BlockInfo { number: 2, timestamp: 2, ..Default::default() });

        batch_provider.attempt_update().unwrap();
        let BatchProviderState::BatchValidator(stage) = batch_provider.state() else {
            panic!("Expected BatchValidator");
        };
        assert_eq!(stage.origin, Some(shared_origin));
        assert_eq!(stage.l1_blocks, shared_l1_blocks);

        // Reorg to before activation. The legacy queue must become the sole owner again without
        // losing the shared window used to validate the next batch.
        let BatchProviderState::BatchValidator(stage) = &mut batch_provider.state else {
            panic!("Expected BatchValidator");
        };
        stage.prev.origin = Some(BlockInfo::default());

        batch_provider.attempt_update().unwrap();
        let BatchProviderState::BatchQueue(stage) = batch_provider.state() else {
            panic!("Expected BatchQueue");
        };
        assert_eq!(stage.origin, Some(shared_origin));
        assert_eq!(stage.l1_blocks, shared_l1_blocks);
    }

    #[tokio::test]
    async fn test_batch_provider_reset_bq() {
        let provider = TestNextBatchProvider::new(vec![]);
        let l2_provider = TestL2ChainProvider::default();
        let cfg = Arc::new(RollupConfig::default());
        let mut batch_provider = BatchProvider::new(cfg, provider, l2_provider);

        // Reset the batch provider.
        batch_provider.reset(BlockNumHash::default(), SystemConfig::default()).await.unwrap();

        let BatchProviderState::BatchQueue(bq) = batch_provider.state() else {
            panic!("Expected BatchQueue");
        };
        assert!(bq.l1_blocks.len() == 1);
    }

    #[tokio::test]
    async fn test_batch_provider_reset_validator() {
        let provider = TestNextBatchProvider::new(vec![]);
        let l2_provider = TestL2ChainProvider::default();
        let cfg = Arc::new(RollupConfig {
            upgrades: UpgradeConfig { holocene_time: Some(0), ..Default::default() },
            ..Default::default()
        });
        let mut batch_provider = BatchProvider::new(cfg, provider, l2_provider);

        // Reset the batch provider.
        batch_provider.reset(BlockNumHash::default(), SystemConfig::default()).await.unwrap();

        let BatchProviderState::BatchValidator(bv) = batch_provider.state() else {
            panic!("Expected BatchValidator");
        };
        assert!(bv.l1_blocks.len() == 1);
    }

    #[tokio::test]
    async fn test_denim_validator_skips_same_second_stale_batch() {
        let origin = BlockInfo { number: 1, hash: B256::repeat_byte(0x11), ..Default::default() };
        let cfg = Arc::new(RollupConfig {
            block_time: 2,
            upgrades: UpgradeConfig {
                holocene_time: Some(0),
                base: BaseUpgradeConfig { denim: Some(46), ..Default::default() },
                ..Default::default()
            },
            ..Default::default()
        });
        let parent = L2BlockInfo {
            block_info: BlockInfo {
                number: 300,
                hash: B256::repeat_byte(0x33),
                timestamp: cfg.l2_block_timestamp(300),
                ..Default::default()
            },
            l1_origin: BlockNumHash { number: 0, ..Default::default() },
            ..Default::default()
        };
        assert_eq!(cfg.denim_activation_block_number(), Some(23));
        assert_eq!(cfg.l2_block_timestamp(298), cfg.l2_block_timestamp(301));
        let valid = SingleBatch {
            parent_hash: parent.block_info.hash,
            epoch_num: origin.number,
            epoch_hash: origin.hash,
            timestamp: cfg.l2_block_timestamp(parent.block_info.number + 1),
            ..Default::default()
        };
        let stale_298 =
            SingleBatch { parent_hash: B256::with_last_byte(297_u64 as u8), ..valid.clone() };
        let stale_299 =
            SingleBatch { parent_hash: B256::with_last_byte(298_u64 as u8), ..valid.clone() };
        let mut prev = TestNextBatchProvider::new(vec![
            Ok(Batch::Single(valid.clone())),
            Ok(Batch::Single(stale_299)),
            Ok(Batch::Single(stale_298)),
        ]);
        prev.origin = Some(origin);
        let l2_provider = TestL2ChainProvider {
            blocks: (295..parent.block_info.number)
                .map(|number| L2BlockInfo {
                    block_info: BlockInfo {
                        number,
                        hash: B256::with_last_byte(number as u8),
                        ..Default::default()
                    },
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        };
        let mut batch_provider = BatchProvider::new(cfg, prev, l2_provider);
        batch_provider.attempt_update().unwrap();
        let BatchProviderState::BatchValidator(validator) = &mut batch_provider.state else {
            panic!("Expected BatchValidator");
        };
        validator.origin = Some(origin);
        validator.l1_blocks = vec![origin, origin];

        assert_eq!(
            batch_provider.next_batch(parent).await.unwrap_err(),
            PipelineError::NotEnoughData.temp()
        );
        assert_eq!(
            batch_provider.next_batch(parent).await.unwrap_err(),
            PipelineError::NotEnoughData.temp()
        );
        let BatchProviderState::BatchValidator(validator) = batch_provider.state() else {
            panic!("Expected BatchValidator");
        };
        assert!(!validator.prev.flushed);
        assert_eq!(batch_provider.next_batch(parent).await.unwrap(), valid);
    }
}
