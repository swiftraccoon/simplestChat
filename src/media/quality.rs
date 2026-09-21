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

#[cfg(test)]
mod tests {
    use super::*;

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
