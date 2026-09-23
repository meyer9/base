//! Hybrid L1 head source that races a subscription stream against interval-based polling.

use std::{marker::PhantomData, time::Duration};

use async_trait::async_trait;
use base_runtime::Clock;
use futures::{StreamExt, stream::BoxStream};

use crate::{L1HeadEvent, L1HeadPolling, L1HeadSource, L1HeadSubscription, SourceError};

/// An L1 head source that races a subscription stream against an interval-based poller.
///
/// If the subscription closes or fails, it is disabled and the source continues
/// advancing from polling. This prevents a transient WebSocket failure from
/// disabling L1-head-driven channel timeout handling in the batcher.
///
/// Deduplicates head numbers so that the same block number is only reported once.
/// Stale reads (same or lower block number than last reported) are also silently dropped.
#[derive(derive_more::Debug)]
pub struct HybridL1HeadSource<S, P, C> {
    /// The head number stream returned by `S::take_stream`.
    ///
    /// Declared before `_subscription` so it is dropped first, ensuring the
    /// stream's underlying transport is released before the provider is torn down.
    #[debug(skip)]
    sub: Option<BoxStream<'static, Result<u64, SourceError>>>,
    /// The original subscription, kept alive so its resources remain open.
    #[debug(skip)]
    _subscription: S,
    /// Polling source for fetching the latest L1 head block number.
    #[debug(skip)]
    poller: P,
    /// Polling interval timer.
    #[debug(skip)]
    interval: BoxStream<'static, ()>,
    /// Runtime clock type marker.
    #[debug(skip)]
    _clock: PhantomData<C>,
    /// Last reported head number for deduplication.
    last_head: Option<u64>,
}

impl<S, P, C> HybridL1HeadSource<S, P, C>
where
    S: L1HeadSubscription,
    P: L1HeadPolling,
    C: Clock,
{
    /// Create a new hybrid L1 head source.
    ///
    /// Calls [`L1HeadSubscription::take_stream`] once to obtain the live head
    /// number stream, then retains the subscription to keep any underlying
    /// resources (e.g. a WebSocket provider) alive. Combines the stream with a
    /// poller that fires at `poll_interval`.
    pub fn new(clock: C, mut subscription: S, poller: P, poll_interval: Duration) -> Self {
        let sub = subscription.take_stream();
        let interval = clock.interval(poll_interval);
        Self {
            sub: Some(sub),
            _subscription: subscription,
            poller,
            interval,
            _clock: PhantomData,
            last_head: None,
        }
    }

    /// Process a received head number, returning an event if it is strictly newer.
    ///
    /// Drops duplicate or stale values (same or lower head number than last emitted).
    fn process(&mut self, head: u64) -> Option<L1HeadEvent> {
        if self.last_head.is_some_and(|last| last >= head) {
            tracing::debug!(head, "stale or duplicate L1 head, skipping");
            return None;
        }
        self.last_head = Some(head);
        Some(L1HeadEvent::NewHead(head))
    }
}

#[async_trait]
impl<S, P, C> L1HeadSource for HybridL1HeadSource<S, P, C>
where
    S: L1HeadSubscription,
    P: L1HeadPolling,
    C: Clock,
{
    async fn next(&mut self) -> Result<L1HeadEvent, SourceError> {
        loop {
            tokio::select! {
                head = async {
                    self.sub
                        .as_mut()
                        .expect("subscription branch requires an active stream")
                        .next()
                        .await
                }, if self.sub.is_some() => {
                    match head {
                        Some(Ok(n)) => {
                            if let Some(event) = self.process(n) {
                                return Ok(event);
                            }
                            // Stale or duplicate — loop for next event.
                        }
                        Some(Err(error)) => {
                            tracing::warn!(error = %error, "L1 head subscription failed, falling back to polling");
                            self.sub = None;
                        }
                        None => {
                            tracing::warn!("L1 head subscription closed, falling back to polling");
                            self.sub = None;
                        }
                    }
                }
                _ = self.interval.next() => {
                    match self.poller.latest_head().await {
                        Ok(n) => {
                            if let Some(event) = self.process(n) {
                                return Ok(event);
                            }
                            // Stale or duplicate — loop for next event.
                        }
                        Err(SourceError::Provider(msg)) => {
                            tracing::warn!(error = %msg, "L1 head polling error, retrying on next tick");
                            // Transient provider error — continue to next tick.
                        }
                        Err(e) => return Err(e),
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{
            Arc,
            atomic::{AtomicU64, Ordering},
        },
        task::Poll,
    };

    use async_trait::async_trait;
    use base_runtime::{Config, Runner};
    use futures::{StreamExt, stream::BoxStream};

    use super::*;

    struct StreamSub(BoxStream<'static, Result<u64, SourceError>>);

    impl L1HeadSubscription for StreamSub {
        fn take_stream(&mut self) -> BoxStream<'static, Result<u64, SourceError>> {
            std::mem::replace(&mut self.0, futures::stream::pending().boxed())
        }
    }

    struct FixedPoller(u64);

    #[async_trait]
    impl L1HeadPolling for FixedPoller {
        async fn latest_head(&self) -> Result<u64, SourceError> {
            Ok(self.0)
        }
    }

    struct ControlledPoller {
        head: Arc<AtomicU64>,
    }

    impl ControlledPoller {
        fn new(head: Arc<AtomicU64>) -> Self {
            Self { head }
        }
    }

    #[async_trait]
    impl L1HeadPolling for ControlledPoller {
        async fn latest_head(&self) -> Result<u64, SourceError> {
            match self.head.load(Ordering::Relaxed) {
                0 => Err(SourceError::Provider("poll down".to_string())),
                head => Ok(head),
            }
        }
    }

    #[test]
    fn test_hybrid_l1_new_head() {
        Runner::start(Config::seeded(0), |ctx| async move {
            let stream = futures::stream::once(async { Ok(5u64) });
            let mut source = HybridL1HeadSource::new(
                ctx,
                StreamSub(stream.boxed()),
                FixedPoller(5),
                Duration::from_secs(100),
            );

            let event = source.next().await.unwrap();
            assert_eq!(event, L1HeadEvent::NewHead(5));
        });
    }

    #[test]
    fn test_hybrid_l1_duplicate_skipped() {
        Runner::start(Config::seeded(0), |ctx| async move {
            let stream = futures::stream::iter(vec![Ok(5u64), Ok(5u64)]);
            let head = Arc::new(AtomicU64::new(0));
            let mut source = HybridL1HeadSource::new(
                ctx,
                StreamSub(stream.boxed()),
                ControlledPoller::new(Arc::clone(&head)),
                Duration::from_secs(100),
            );

            let event = source.next().await.unwrap();
            assert_eq!(event, L1HeadEvent::NewHead(5));

            head.store(6, Ordering::Relaxed);
            // The duplicate is skipped and polling supplies the next head after closure.
            assert_eq!(source.next().await.unwrap(), L1HeadEvent::NewHead(6));
        });
    }

    #[test]
    fn test_hybrid_l1_stale_dropped() {
        Runner::start(Config::seeded(0), |ctx| async move {
            // Deliver 10, then 9 (stale), then stream closes.
            let stream = futures::stream::iter(vec![Ok(10u64), Ok(9u64)]);
            let head = Arc::new(AtomicU64::new(0));
            let mut source = HybridL1HeadSource::new(
                ctx,
                StreamSub(stream.boxed()),
                ControlledPoller::new(Arc::clone(&head)),
                Duration::from_secs(100),
            );

            let event = source.next().await.unwrap();
            assert_eq!(event, L1HeadEvent::NewHead(10));

            head.store(11, Ordering::Relaxed);
            // 9 < 10 is skipped and polling resumes after the stream closes.
            assert_eq!(source.next().await.unwrap(), L1HeadEvent::NewHead(11));
        });
    }

    #[test]
    fn test_hybrid_l1_closed_subscription_falls_back_to_polling() {
        Runner::start(Config::seeded(0), |ctx| async move {
            let head = Arc::new(AtomicU64::new(0));
            let stream_head = Arc::clone(&head);
            let stream = futures::stream::poll_fn(move |_| {
                stream_head.store(12, Ordering::Relaxed);
                Poll::Ready(None)
            });
            let mut source = HybridL1HeadSource::new(
                ctx,
                StreamSub(stream.boxed()),
                ControlledPoller::new(head),
                Duration::from_secs(10),
            );

            assert_eq!(source.next().await.unwrap(), L1HeadEvent::NewHead(12));
        });
    }

    #[test]
    fn test_hybrid_l1_failed_subscription_falls_back_to_polling() {
        Runner::start(Config::seeded(0), |ctx| async move {
            let head = Arc::new(AtomicU64::new(0));
            let stream_head = Arc::clone(&head);
            let stream = futures::stream::once(async move {
                stream_head.store(12, Ordering::Relaxed);
                Err(SourceError::Provider("ws down".to_string()))
            });
            let mut source = HybridL1HeadSource::new(
                ctx,
                StreamSub(stream.boxed()),
                ControlledPoller::new(head),
                Duration::from_secs(10),
            );

            assert_eq!(source.next().await.unwrap(), L1HeadEvent::NewHead(12));
        });
    }

    #[test]
    fn test_hybrid_l1_polling_uses_virtual_time() {
        Runner::start(Config::seeded(0), |ctx| async move {
            let mut source = HybridL1HeadSource::new(
                ctx,
                StreamSub(futures::stream::pending().boxed()),
                FixedPoller(12),
                Duration::from_secs(10),
            );

            let event = source.next().await.unwrap();
            assert_eq!(event, L1HeadEvent::NewHead(12));
        });
    }
}
