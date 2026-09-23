//! Server-side media quality sample: what the SFU itself knows about every
//! consumer, producer and receive transport from RTCP and its own send path.
//! Nothing here comes from a client report.

/// Consumer and producer scores run from 0 to 10.
pub const SCORE_BUCKETS: usize = 11;
/// Upper bounds of the downlink loss buckets (fractions), plus an overflow bucket.
pub const LOSS_BOUNDS: [f64; 5] = [0.005, 0.01, 0.02, 0.05, 0.10];
/// Upper bounds of the available outgoing bitrate buckets in bit/s, plus overflow.
pub const BITRATE_BOUNDS: [u32; 5] = [150_000, 300_000, 600_000, 1_000_000, 2_000_000];
/// Spatial layers 0, 1 and 2, plus "none" (audio, paused or not yet known).
pub const SPATIAL_BUCKETS: usize = 4;

fn bucket<T: PartialOrd>(bounds: &[T], value: T) -> usize {
    bounds
        .iter()
        .position(|bound| value <= *bound)
        .unwrap_or(bounds.len())
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct QualitySample {
    pub completed_at: Option<std::time::Instant>,
    pub completed_wall_time: Option<std::time::SystemTime>,
    pub duration: std::time::Duration,
    pub participants_available: u64,
    pub participants_sampled: u64,
    pub transports_available: u64,
    pub transports_selected: u64,
    pub transports_requested: u64,
    pub budget_exhausted: bool,
    pub consumers: u64,
    pub consumers_paused: u64,
    pub consumer_scores: [u64; SCORE_BUCKETS],
    pub consumer_score_sum: u64,
    pub video_consumers_by_spatial: [u64; SPATIAL_BUCKETS],
    pub producers: u64,
    pub producer_scores: [u64; SCORE_BUCKETS],
    pub producer_score_sum: u64,
    pub transports_sampled: u64,
    pub transport_stats_failed: u64,
    pub loss_buckets: [u64; LOSS_BOUNDS.len() + 1],
    pub loss_sum: f64,
    pub bitrate_buckets: [u64; BITRATE_BOUNDS.len() + 1],
    pub bitrate_sum: u64,
}

impl QualitySample {
    pub fn record_consumer(&mut self, score: u8, spatial: Option<u8>, video: bool, paused: bool) {
        self.consumers += 1;
        if paused {
            self.consumers_paused += 1;
        }
        let score = usize::from(score.min(10));
        self.consumer_scores[score] += 1;
        self.consumer_score_sum += score as u64;
        if video {
            let index = match spatial {
                Some(layer) if !paused && usize::from(layer) < SPATIAL_BUCKETS - 1 => {
                    usize::from(layer)
                }
                _ => SPATIAL_BUCKETS - 1,
            };
            self.video_consumers_by_spatial[index] += 1;
        }
    }

    /// The producer's worst stream score; a producer without scores yet is skipped.
    pub fn record_producer(&mut self, scores: impl IntoIterator<Item = u8>) {
        let Some(worst) = scores.into_iter().map(|score| score.min(10)).min() else {
            return;
        };
        self.producers += 1;
        self.producer_scores[usize::from(worst)] += 1;
        self.producer_score_sum += u64::from(worst);
    }

    pub fn record_transport(
        &mut self,
        loss_sent: Option<f64>,
        available_outgoing_bitrate: Option<u32>,
    ) {
        self.transports_sampled += 1;
        if let Some(loss) = loss_sent.filter(|loss| loss.is_finite()) {
            let loss = loss.clamp(0.0, 1.0);
            self.loss_buckets[bucket(&LOSS_BOUNDS, loss)] += 1;
            self.loss_sum += loss;
        }
        if let Some(bitrate) = available_outgoing_bitrate {
            self.bitrate_buckets[bucket(&BITRATE_BOUNDS, bitrate)] += 1;
            self.bitrate_sum += u64::from(bitrate);
        }
    }

    /// Transports whose loss was reported (the estimator existed).
    pub fn loss_count(&self) -> u64 {
        self.loss_buckets.iter().sum()
    }

    pub fn bitrate_count(&self) -> u64 {
        self.bitrate_buckets.iter().sum()
    }
}

pub(super) struct TransportReading {
    pub loss_sent: Option<f64>,
    pub available_outgoing_bitrate: Option<u32>,
}

/// Requests are owned futures, so leaving the total budget drops every pending
/// wait. Per-request deadlines and concurrency are independent of the number of
/// selected transports; missed/failed observations remain explicitly counted.
pub(super) async fn collect_transport_stats<F>(
    sample: &mut QualitySample,
    requests: impl Iterator<Item = F>,
    deadline: tokio::time::Instant,
) where
    F: std::future::Future<Output = Option<TransportReading>>,
{
    use futures_util::{StreamExt, stream};
    use std::sync::atomic::{AtomicU64, Ordering};
    let requested = AtomicU64::new(0);
    let requests = stream::iter(requests.map(|request| {
        let requested = &requested;
        async move {
            requested.fetch_add(1, Ordering::Relaxed);
            tokio::time::timeout(std::time::Duration::from_millis(250), request).await
        }
    }))
    .buffer_unordered(8);
    tokio::pin!(requests);
    loop {
        match tokio::time::timeout_at(deadline, requests.next()).await {
            Ok(Some(Ok(Some(stat)))) => {
                sample.record_transport(stat.loss_sent, stat.available_outgoing_bitrate)
            }
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(_) => {
                sample.budget_exhausted = true;
                break;
            }
        }
    }
    sample.transports_requested = requested.load(Ordering::Relaxed);
    sample.transport_stats_failed = sample.transports_requested - sample.transports_sampled;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn stalled_transport_requests_obey_one_budget_and_a_concurrency_cap() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        struct Active<'a>(&'a AtomicUsize);
        impl Drop for Active<'_> {
            fn drop(&mut self) {
                self.0.fetch_sub(1, Ordering::SeqCst);
            }
        }
        let active = AtomicUsize::new(0);
        let maximum = AtomicUsize::new(0);
        let requests = (0..100).map(|_| async {
            let concurrent = active.fetch_add(1, Ordering::SeqCst) + 1;
            maximum.fetch_max(concurrent, Ordering::SeqCst);
            let _active = Active(&active);
            std::future::pending::<Option<TransportReading>>().await
        });
        let mut sample = QualitySample::default();
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            collect_transport_stats(
                &mut sample,
                requests,
                tokio::time::Instant::now() + std::time::Duration::from_millis(20),
            ),
        )
        .await
        .expect("the entire sample has one short budget");
        assert_eq!(maximum.load(Ordering::SeqCst), 8);
        assert_eq!(
            active.load(Ordering::SeqCst),
            0,
            "pending requests dropped at the deadline"
        );
        assert_eq!(sample.transports_requested, 8);
        assert_eq!(sample.transports_sampled, 0);
        assert_eq!(sample.transport_stats_failed, 8);
        assert!(sample.budget_exhausted);
    }

    #[tokio::test]
    async fn transport_successes_failures_and_missing_estimates_are_distinct() {
        let requests = (0..3).map(|index| async move {
            match index {
                0 => Some(TransportReading {
                    loss_sent: Some(0.02),
                    available_outgoing_bitrate: Some(300_000),
                }),
                1 => Some(TransportReading {
                    loss_sent: None,
                    available_outgoing_bitrate: None,
                }),
                _ => None,
            }
        });
        let mut sample = QualitySample::default();
        collect_transport_stats(
            &mut sample,
            requests,
            tokio::time::Instant::now() + std::time::Duration::from_secs(1),
        )
        .await;
        assert_eq!(sample.transports_requested, 3);
        assert_eq!(sample.transports_sampled, 2);
        assert_eq!(sample.transport_stats_failed, 1);
        assert_eq!(sample.loss_count(), 1);
        assert_eq!(sample.bitrate_count(), 1);
        assert!(!sample.budget_exhausted);
    }

    #[test]
    fn consumers_are_bucketed_by_score_and_active_video_layer() {
        let mut sample = QualitySample::default();
        sample.record_consumer(10, Some(2), true, false);
        sample.record_consumer(7, Some(0), true, false);
        sample.record_consumer(12, Some(1), true, true);
        sample.record_consumer(9, None, false, false);
        assert_eq!(sample.consumers, 4);
        assert_eq!(sample.consumers_paused, 1);
        assert_eq!(sample.consumer_scores[10], 2, "scores above ten clamp");
        assert_eq!(sample.consumer_scores[7], 1);
        assert_eq!(sample.consumer_score_sum, 36);
        assert_eq!(
            sample.video_consumers_by_spatial,
            [1, 0, 1, 1],
            "paused video counts as none"
        );
    }

    #[test]
    fn producers_use_their_worst_stream_and_need_a_score() {
        let mut sample = QualitySample::default();
        sample.record_producer([10, 6, 8]);
        sample.record_producer([]);
        assert_eq!(sample.producers, 1);
        assert_eq!(sample.producer_scores[6], 1);
        assert_eq!(sample.producer_score_sum, 6);
    }

    #[test]
    fn transport_loss_and_bitrate_fall_into_bounded_buckets() {
        let mut sample = QualitySample::default();
        sample.record_transport(Some(0.0), Some(120_000));
        sample.record_transport(Some(0.02), Some(600_000));
        sample.record_transport(Some(0.5), Some(5_000_000));
        sample.record_transport(None, None);
        sample.record_transport(Some(f64::NAN), Some(300_001));
        assert_eq!(sample.transports_sampled, 5);
        assert_eq!(sample.loss_buckets, [1, 0, 1, 0, 0, 1]);
        assert_eq!(sample.loss_count(), 3);
        assert!((sample.loss_sum - 0.52).abs() < 1e-9);
        assert_eq!(sample.bitrate_buckets, [1, 0, 2, 0, 0, 1]);
        assert_eq!(sample.bitrate_count(), 4);
    }
}
