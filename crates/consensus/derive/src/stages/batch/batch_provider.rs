//! This module contains the [`BatchProvider`] stage.

use alloc::sync::Arc;
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

/// The [`BatchProvider`] stage owns the derivation rule selected by the current L1 origin.
///
/// Before Holocene it owns a [`BatchQueue`]; from Holocene onward it owns a
/// [`BatchValidator`]. The state is explicit so an origin reorganization can only move between
/// the supported derivation rules and never leave two rules active at once.
#[derive(Debug)]
pub struct BatchProvider<P, F>
where
    P: NextBatchProvider + OriginAdvancer + OriginProvider + StageReset + Debug,
    F: L2ChainProvider + Clone + Debug,
{
    /// The rollup configuration that selects the supported derivation rule.
    pub cfg: Arc<RollupConfig>,
    /// The L2 chain provider shared by each supported derivation rule.
    pub provider: F,
    /// The current derivation-rule owner.
    pub state: Option<BatchProviderState<P, F>>,
}

/// The derivation rule owned by a [`BatchProvider`].
#[derive(Debug)]
pub enum BatchProviderState<P, F>
where
    P: NextBatchProvider + OriginAdvancer + OriginProvider + StageReset + Debug,
    F: L2ChainProvider + Clone + Debug,
{
    /// The upstream stage has not yet been assigned a derivation rule.
    Pending(P),
    /// The pre-Holocene queue owns batch ordering and empty-batch derivation.
    Queue(BatchQueue<P, F>),
    /// The Holocene validator owns strict batch validation and empty-batch derivation.
    Validator(BatchValidator<P, F>),
}

impl<P, F> BatchProvider<P, F>
where
    P: NextBatchProvider + OriginAdvancer + OriginProvider + StageReset + Debug,
    F: L2ChainProvider + Clone + Debug,
{
    /// Creates a new [`BatchProvider`] with the given configuration and previous stage.
    pub const fn new(cfg: Arc<RollupConfig>, prev: P, provider: F) -> Self {
        Self { cfg, provider, state: Some(BatchProviderState::Pending(prev)) }
    }

    /// Updates the derivation-rule owner for the current L1 origin.
    pub fn attempt_update(&mut self) -> PipelineResult<()> {
        let origin = self.origin().ok_or(PipelineError::MissingOrigin.crit())?;
        let holocene_active = self.cfg.is_holocene_active(origin.timestamp);
        let state = self.state.take().expect("batch provider state is restored after every update");
        self.state = Some(match state {
            BatchProviderState::Pending(prev) if holocene_active => BatchProviderState::Validator(
                BatchValidator::new(Arc::clone(&self.cfg), prev, self.provider.clone()),
            ),
            BatchProviderState::Pending(prev) => BatchProviderState::Queue(BatchQueue::new(
                Arc::clone(&self.cfg),
                prev,
                self.provider.clone(),
            )),
            BatchProviderState::Queue(batch_queue) if holocene_active => {
                let mut validator = BatchValidator::new(
                    Arc::clone(&self.cfg),
                    batch_queue.prev,
                    self.provider.clone(),
                );
                // Retain the canonical L1 window while the ownership rule changes.
                validator.origin = batch_queue.origin;
                validator.l1_blocks = batch_queue.l1_blocks;
                BatchProviderState::Validator(validator)
            }
            BatchProviderState::Validator(batch_validator) if !holocene_active => {
                // An L1 reorganization crossed activation. Do not carry strict-validator work into
                // the legacy queue; the queue rebuilds its window from the reorged upstream origin.
                BatchProviderState::Queue(BatchQueue::new(
                    Arc::clone(&self.cfg),
                    batch_validator.prev,
                    self.provider.clone(),
                ))
            }
            state => state,
        });
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
        match self.state.as_mut().expect("batch provider state is initialized") {
            BatchProviderState::Pending(_) => Err(PipelineError::NotEnoughData.temp()),
            BatchProviderState::Queue(queue) => queue.advance_origin().await,
            BatchProviderState::Validator(validator) => validator.advance_origin().await,
        }
    }
}

impl<P, F> OriginProvider for BatchProvider<P, F>
where
    P: NextBatchProvider + OriginAdvancer + OriginProvider + StageReset + Debug,
    F: L2ChainProvider + Clone + Debug,
{
    fn origin(&self) -> Option<BlockInfo> {
        match self.state.as_ref()? {
            BatchProviderState::Pending(prev) => prev.origin(),
            BatchProviderState::Queue(queue) => queue.origin(),
            BatchProviderState::Validator(validator) => validator.origin(),
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
        match self.state.as_mut().expect("batch provider state is initialized") {
            BatchProviderState::Pending(_) => Err(PipelineError::NotEnoughData.temp()),
            BatchProviderState::Queue(queue) => queue.reset(l1_origin, system_config).await,
            BatchProviderState::Validator(validator) => {
                validator.reset(l1_origin, system_config).await
            }
        }
    }

    async fn activate(&mut self) -> PipelineResult<()> {
        self.attempt_update()?;
        match self.state.as_mut().expect("batch provider state is initialized") {
            BatchProviderState::Pending(_) => Err(PipelineError::NotEnoughData.temp()),
            BatchProviderState::Queue(queue) => queue.activate().await,
            BatchProviderState::Validator(validator) => validator.activate().await,
        }
    }

    async fn flush_channel(&mut self) -> PipelineResult<()> {
        self.attempt_update()?;
        match self.state.as_mut().expect("batch provider state is initialized") {
            BatchProviderState::Pending(_) => Err(PipelineError::NotEnoughData.temp()),
            BatchProviderState::Queue(queue) => queue.flush_channel().await,
            BatchProviderState::Validator(validator) => validator.flush_channel().await,
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
        match self.state.as_ref() {
            Some(BatchProviderState::Queue(queue)) => queue.is_last_in_span(),
            Some(BatchProviderState::Validator(validator)) => validator.is_last_in_span(),
            Some(BatchProviderState::Pending(_)) | None => false,
        }
    }

    async fn next_batch(&mut self, parent: L2BlockInfo) -> PipelineResult<SingleBatch> {
        self.attempt_update()?;
        match self.state.as_mut().expect("batch provider state is initialized") {
            BatchProviderState::Pending(_) => Err(PipelineError::NotEnoughData.temp()),
            BatchProviderState::Queue(queue) => queue.next_batch(parent).await,
            BatchProviderState::Validator(validator) => validator.next_batch(parent).await,
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
        assert!(matches!(batch_provider.state, Some(BatchProviderState::Validator(_))));
    }

    #[test]
    fn test_batch_provider_batch_queue_active() {
        let provider = TestNextBatchProvider::new(vec![]);
        let l2_provider = TestL2ChainProvider::default();
        let cfg = Arc::new(RollupConfig::default());
        let mut batch_provider = BatchProvider::new(cfg, provider, l2_provider);

        assert!(batch_provider.attempt_update().is_ok());
        assert!(matches!(batch_provider.state, Some(BatchProviderState::Queue(_))));
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
        let Some(BatchProviderState::Queue(stage)) = batch_provider.state.as_mut() else {
            panic!("Expected BatchQueue");
        };
        stage.prev.origin = Some(BlockInfo { number: 1, timestamp: 2, ..Default::default() });

        // Transition to the BatchValidator stage.
        batch_provider.attempt_update().unwrap();
        assert!(matches!(batch_provider.state, Some(BatchProviderState::Validator(_))));

        assert_eq!(batch_provider.origin().unwrap().number, 1);
    }

    #[test]
    fn test_batch_provider_holocene_transition_retains_l1_window() {
        let provider = TestNextBatchProvider::new(vec![]);
        let l2_provider = TestL2ChainProvider::default();
        let cfg = Arc::new(RollupConfig {
            upgrades: UpgradeConfig { holocene_time: Some(10), ..Default::default() },
            ..Default::default()
        });
        let mut batch_provider = BatchProvider::new(cfg, provider, l2_provider);

        batch_provider.attempt_update().unwrap();

        // Set origin and l1_blocks on the BatchQueue before the transition.
        let Some(BatchProviderState::Queue(stage)) = batch_provider.state.as_mut() else {
            panic!("Expected BatchQueue");
        };
        stage.origin = Some(BlockInfo { number: 5, timestamp: 3, ..Default::default() });
        stage.l1_blocks = vec![BlockInfo { number: 5, timestamp: 3, ..Default::default() }];

        // Update the L1 origin to Holocene activation.
        stage.prev.origin = Some(BlockInfo { number: 1, timestamp: 10, ..Default::default() });

        // Transition to the BatchValidator stage.
        batch_provider.attempt_update().unwrap();
        assert!(matches!(batch_provider.state, Some(BatchProviderState::Validator(_))));

        // Assert that origin was transferred.
        let Some(BatchProviderState::Validator(bv)) = batch_provider.state.as_ref() else {
            panic!("Expected BatchValidator");
        };
        assert_eq!(bv.origin, Some(BlockInfo { number: 5, timestamp: 3, ..Default::default() }));
        assert_eq!(bv.l1_blocks, vec![BlockInfo { number: 5, timestamp: 3, ..Default::default() }]);
    }

    #[test]
    fn test_batch_provider_transition_stage_backwards() {
        let provider = TestNextBatchProvider::new(vec![]);
        let l2_provider = TestL2ChainProvider::default();
        let cfg = Arc::new(RollupConfig {
            upgrades: UpgradeConfig { holocene_time: Some(2), ..Default::default() },
            ..Default::default()
        });
        let mut batch_provider = BatchProvider::new(cfg, provider, l2_provider);

        batch_provider.attempt_update().unwrap();

        // Update the L1 origin to Holocene activation.
        let Some(BatchProviderState::Queue(stage)) = batch_provider.state.as_mut() else {
            panic!("Expected BatchQueue");
        };
        stage.prev.origin = Some(BlockInfo { number: 1, timestamp: 2, ..Default::default() });

        // Transition to the BatchValidator stage.
        batch_provider.attempt_update().unwrap();
        assert!(matches!(batch_provider.state, Some(BatchProviderState::Validator(_))));

        // Update the L1 origin to before Holocene activation, to simulate a re-org.
        let Some(BatchProviderState::Validator(stage)) = batch_provider.state.as_mut() else {
            panic!("Expected BatchValidator");
        };
        stage.prev.origin = Some(BlockInfo::default());

        batch_provider.attempt_update().unwrap();
        assert!(matches!(batch_provider.state, Some(BatchProviderState::Queue(_))));
    }

    #[tokio::test]
    async fn test_batch_provider_reset_bq() {
        let provider = TestNextBatchProvider::new(vec![]);
        let l2_provider = TestL2ChainProvider::default();
        let cfg = Arc::new(RollupConfig::default());
        let mut batch_provider = BatchProvider::new(cfg, provider, l2_provider);

        // Reset the batch provider.
        batch_provider.reset(BlockNumHash::default(), SystemConfig::default()).await.unwrap();

        let Some(BatchProviderState::Queue(bq)) = batch_provider.state else {
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

        let Some(BatchProviderState::Validator(bv)) = batch_provider.state else {
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
        let Some(BatchProviderState::Validator(validator)) = batch_provider.state.as_mut() else {
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
        let Some(BatchProviderState::Validator(validator)) = batch_provider.state.as_ref() else {
            panic!("Expected BatchValidator");
        };
        assert!(!validator.prev.flushed);
        assert_eq!(batch_provider.next_batch(parent).await.unwrap(), valid);
    }
}
