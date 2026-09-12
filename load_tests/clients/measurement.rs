/// One wall-clock interval shared by every client, including clients that churn.
pub struct MeasurementWindow {
    pub start: Instant,
    pub end: Instant,
    publishers: std::sync::Mutex<HashMap<String, (Instant, Option<Instant>)>>,
    publisher_owners: std::sync::Mutex<HashMap<String, (String, bool)>>,
}

impl MeasurementWindow {
    pub fn new(start: Instant, duration: std::time::Duration) -> Self {
        Self {
            start,
            end: start + duration,
            publishers: std::sync::Mutex::new(HashMap::new()),
            publisher_owners: std::sync::Mutex::new(HashMap::new()),
        }
    }

    fn bucket(&self, now: Instant) -> Option<usize> {
        (now >= self.start && now < self.end)
            .then(|| now.duration_since(self.start).as_secs() as usize)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MeasurementMetrics {
    pub duration_ms: u64,
    pub packets_queued: u64,
    pub packets_received: u64,
    pub bytes_queued: u64,
    pub bytes_received: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionAttempt {
    pub room_join_ms: Option<u64>,
    pub send_media_ready_ms: Option<u64>,
    pub receive_media_ready_ms: Option<u64>,
    /// Absent in historical artifacts: absence is not successful coverage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coverage: Option<AttemptCoverage>,
}

/// Expectations frozen before socket setup; delayed setup cannot shorten them.
#[derive(Debug, Clone)]
pub struct AttemptPlan {
    pub deadline: Instant,
    pub stable_publishers: std::sync::Arc<std::collections::HashSet<String>>,
    pub expected_audio: usize,
    pub expected_video: usize,
    pub is_publisher: bool,
}

/// A capped floor of distinct stable peers, not complete dynamic-publisher fan-out.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttemptCoverage {
    pub planned_eligible_seconds: usize,
    pub expected_audio: usize,
    pub expected_video: usize,
    pub validated_audio: usize,
    pub validated_video: usize,
    pub packets_queued: u64,
    pub passed: bool,
    pub skipped_short_tail: bool,
    pub failure_reasons: Vec<String>,
}

struct AttemptState {
    started: Instant,
    plan: Option<AttemptPlan>,
    report: ConnectionAttempt,
    failed: bool,
}

struct MeasurementState {
    total: MeasurementMetrics,
    // Shares the existing counter lock: no additional lock on the send hot path.
    // Closing an attempt freezes its count before native cleanup starts.
    queued_by_attempt: HashMap<usize, AttemptQueued>,
}

struct AttemptQueued {
    packets: u64,
    ended: bool,
    deadline: Option<Instant>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConsumerDelivery {
    pub consumer_id: String,
    pub producer_id: String,
    pub ssrc: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_audio: Option<bool>,
    pub packets_by_second: Vec<u64>,
    pub eligible_seconds: usize,
    pub seconds_with_packets: usize,
    pub longest_gap_seconds: usize,
    pub passed: bool,
    pub skipped_short_lived: bool,
}

struct ConsumerState {
    consumer_id: String,
    producer_id: String,
    ssrc: u32,
    attempt: usize,
    is_audio: Option<bool>,
    planned_end: Option<Instant>,
    created: Instant,
    closed: Option<Instant>,
    packets_by_second: Vec<u64>,
}

/// Detailed observations are separate from the legacy lifetime counters.
pub struct Measurements {
    client_id: String,
    connection_start: std::sync::Mutex<Instant>,
    attempts: std::sync::Mutex<Vec<AttemptState>>,
    window: std::sync::Arc<MeasurementWindow>,
    measurement: std::sync::Mutex<MeasurementState>,
    consumers: std::sync::Mutex<Vec<ConsumerState>>,
    subscriptions: std::sync::Mutex<HashMap<String, bool>>,
    current_producers: std::sync::Mutex<Vec<String>>,
}

impl Measurements {
    fn new(client_id: String, window: std::sync::Arc<MeasurementWindow>) -> Self {
        Self {
            client_id,
            connection_start: std::sync::Mutex::new(Instant::now()),
            attempts: std::sync::Mutex::new(Vec::new()),
            measurement: std::sync::Mutex::new(MeasurementState {
                total: MeasurementMetrics {
                    duration_ms: window.end.duration_since(window.start).as_millis() as u64,
                    ..Default::default()
                },
                queued_by_attempt: HashMap::new(),
            }),
            window,
            consumers: std::sync::Mutex::new(Vec::new()),
            subscriptions: std::sync::Mutex::new(HashMap::new()),
            current_producers: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// Called inside the spawned task, immediately before each WebSocket attempt.
    pub fn begin_planned_attempt(&self, plan: AttemptPlan) -> usize {
        self.begin_attempt(Some(plan))
    }

    #[cfg(test)]
    pub fn begin_connection_attempt(&self) {
        self.begin_attempt(None);
    }

    fn begin_attempt(&self, plan: Option<AttemptPlan>) -> usize {
        let started = Instant::now();
        let deadline = plan.as_ref().map(|p| p.deadline);
        *self.connection_start.lock().unwrap() = started;
        let mut attempts = self.attempts.lock().unwrap();
        attempts.push(AttemptState {
            started,
            plan,
            failed: false,
            report: ConnectionAttempt {
                room_join_ms: None,
                send_media_ready_ms: None,
                receive_media_ready_ms: None,
                coverage: None,
            },
        });
        let ordinal = attempts.len();
        self.measurement.lock().unwrap().queued_by_attempt.insert(
            ordinal,
            AttemptQueued {
                packets: 0,
                ended: false,
                deadline,
            },
        );
        self.subscriptions.lock().unwrap().clear();
        ordinal
    }

    fn attempt_elapsed_ms(&self) -> u64 {
        self.connection_start.lock().unwrap().elapsed().as_millis() as u64
    }

    pub fn mark_media_ready_for_attempt(&self, ordinal: usize, is_send: bool) {
        let mut attempts = self.attempts.lock().unwrap();
        if let Some(attempt) = ordinal
            .checked_sub(1)
            .and_then(|index| attempts.get_mut(index))
        {
            let elapsed = attempt.started.elapsed().as_millis() as u64;
            let report = &mut attempt.report;
            let field = if is_send {
                &mut report.send_media_ready_ms
            } else {
                &mut report.receive_media_ready_ms
            };
            field.get_or_insert(elapsed);
        }
    }

    pub fn record_publisher(&self, producer_id: &str, is_audio: bool) {
        self.window
            .publisher_owners
            .lock()
            .unwrap()
            .insert(producer_id.to_string(), (self.client_id.clone(), is_audio));
        self.window
            .publishers
            .lock()
            .unwrap()
            .insert(producer_id.to_string(), (Instant::now(), None));
        self.current_producers
            .lock()
            .unwrap()
            .push(producer_id.to_string());
    }

    /// Return false for duplicate discovery of a producer already requested.
    pub fn subscribe(&self, producer_id: &str, is_audio: bool) -> bool {
        self.subscriptions
            .lock()
            .unwrap()
            .insert(producer_id.to_string(), is_audio)
            .is_none()
    }

    /// Only an owned publisher's completed lifetime can exclude a new request.
    /// Unknown IDs and server notifications are not evidence of owned retirement.
    pub fn producer_retired(&self, producer_id: &str) -> bool {
        self.window
            .publishers
            .lock()
            .unwrap()
            .get(producer_id)
            .is_some_and(|(_, ended)| ended.is_some())
    }

    /// Stop new subscription work at the planned boundary, while allowing the
    /// signaling loop to finish owned departure and diagnostic cleanup.
    pub fn attempt_accepts_work(&self, ordinal: usize) -> bool {
        let now = Instant::now();
        self.measurement
            .lock()
            .unwrap()
            .queued_by_attempt
            .get(&ordinal)
            .is_some_and(|attempt| !attempt.ended && attempt.deadline.is_none_or(|end| now < end))
    }

    /// A server notification is not authority to excuse missing media from a
    /// generator that is still publishing. Only our owned lifecycle may do that.
    pub fn close_producer(&self, producer_id: &str) -> (Option<bool>, bool) {
        let unexpected = self
            .window
            .publishers
            .lock()
            .unwrap()
            .get(producer_id)
            .is_some_and(|(_, ended)| ended.is_none());
        let now = Instant::now();
        for consumer in self
            .consumers
            .lock()
            .unwrap()
            .iter_mut()
            .filter(|c| c.producer_id == producer_id)
        {
            if !unexpected {
                consumer.closed.get_or_insert(now);
            }
        }
        (
            self.subscriptions.lock().unwrap().remove(producer_id),
            unexpected,
        )
    }

    pub fn record_consumer(&self, consumer_id: &str, producer_id: &str, ssrc: u32) {
        let (attempt, planned_end) = {
            let attempts = self.attempts.lock().unwrap();
            (
                attempts.len(),
                attempts
                    .last()
                    .and_then(|a| a.plan.as_ref())
                    .map(|p| p.deadline),
            )
        };
        let is_audio = self.subscriptions.lock().unwrap().get(producer_id).copied();
        self.consumers.lock().unwrap().push(ConsumerState {
            consumer_id: consumer_id.to_string(),
            producer_id: producer_id.to_string(),
            ssrc,
            attempt,
            is_audio,
            planned_end,
            created: Instant::now(),
            closed: None,
            packets_by_second: vec![
                0;
                self.window.end.duration_since(self.window.start).as_secs()
                    as usize
            ],
        });
    }

    pub fn end_session(&self) {
        let now = Instant::now();
        let ordinal = self.attempts.lock().unwrap().len();
        if let Some(queued) = self
            .measurement
            .lock()
            .unwrap()
            .queued_by_attempt
            .get_mut(&ordinal)
        {
            queued.ended = true;
        }
        for consumer in self.consumers.lock().unwrap().iter_mut() {
            consumer.closed.get_or_insert(now);
        }
        let mut publishers = self.window.publishers.lock().unwrap();
        for producer in self.current_producers.lock().unwrap().drain(..) {
            if let Some((_, end)) = publishers.get_mut(&producer) {
                *end = Some(now);
            }
        }
    }

    fn record_queued(&self, attempt: usize, size: usize) {
        let now = Instant::now();
        if self.window.bucket(now).is_some() {
            let mut measurement = self.measurement.lock().unwrap();
            measurement.total.packets_queued += 1;
            measurement.total.bytes_queued += size as u64;
            if let Some(queued) = measurement.queued_by_attempt.get_mut(&attempt)
                && !queued.ended
                && queued.deadline.is_none_or(|deadline| now < deadline)
            {
                queued.packets += 1;
            }
        }
    }

    fn record_received(&self, size: usize) {
        if self.window.bucket(Instant::now()).is_some() {
            let mut measurement = self.measurement.lock().unwrap();
            measurement.total.packets_received += 1;
            measurement.total.bytes_received += size as u64;
        }
    }

    fn record_ssrc(&self, attempt: usize, ssrc: u32) {
        if let Some(bucket) = self.window.bucket(Instant::now())
            && let Some(consumer) = self
                .consumers
                .lock()
                .unwrap()
                .iter_mut()
                .rev()
                .find(|c| c.attempt == attempt && c.ssrc == ssrc && c.closed.is_none())
        {
            consumer.packets_by_second[bucket] += 1;
        }
    }

    fn delivery_report(&self) -> Vec<ConsumerDelivery> {
        let publishers = self.window.publishers.lock().unwrap();
        self.consumers
            .lock()
            .unwrap()
            .iter()
            .map(|consumer| {
                let publisher = publishers.get(&consumer.producer_id);
                // Allow subscription batching/renegotiation to settle, then inspect
                // complete seconds only. Intentional publisher/client churn ends
                // eligibility immediately, not after the server's reconnect grace.
                let start =
                    (consumer.created + std::time::Duration::from_secs(3)).max(self.window.start);
                let start = publisher.map_or(start, |(created, _)| {
                    start.max(*created + std::time::Duration::from_secs(3))
                });
                let end = consumer
                    .closed
                    .unwrap_or(self.window.end)
                    .min(self.window.end);
                let end = consumer.planned_end.map_or(end, |planned| end.min(planned));
                let end = publisher
                    .and_then(|(_, end)| *end)
                    .map_or(end, |closed| end.min(closed));
                let begin_bucket = start
                    .saturating_duration_since(self.window.start)
                    .as_millis()
                    .div_ceil(1000) as usize;
                let end_bucket =
                    end.saturating_duration_since(self.window.start).as_secs() as usize;
                let eligible = consumer
                    .packets_by_second
                    .get(begin_bucket..end_bucket)
                    .unwrap_or_default();
                let (seconds_with_packets, longest_gap_seconds) = delivery_coverage(eligible);
                ConsumerDelivery {
                    consumer_id: consumer.consumer_id.clone(),
                    producer_id: consumer.producer_id.clone(),
                    ssrc: consumer.ssrc,
                    attempt: (consumer.attempt > 0).then_some(consumer.attempt),
                    is_audio: consumer.is_audio,
                    packets_by_second: consumer.packets_by_second.clone(),
                    eligible_seconds: eligible.len(),
                    seconds_with_packets,
                    longest_gap_seconds,
                    passed: !eligible.is_empty()
                        && seconds_with_packets > 0
                        && longest_gap_seconds <= 2,
                    skipped_short_lived: eligible.is_empty(),
                }
            })
            .collect()
    }

    fn attempt_report(&self, delivery: &[ConsumerDelivery]) -> Vec<ConnectionAttempt> {
        let owners = self.window.publisher_owners.lock().unwrap();
        let attempts = self.attempts.lock().unwrap();
        let measurement = self.measurement.lock().unwrap();
        attempts.iter().enumerate().map(|(index, state)| {
            let mut report = state.report.clone();
            if let Some(plan) = &state.plan {
                let start = (state.started + std::time::Duration::from_secs(3)).max(self.window.start);
                let end = plan.deadline.min(self.window.end);
                let begin_bucket = start.saturating_duration_since(self.window.start)
                    .as_millis().div_ceil(1000) as usize;
                let end_bucket = end.saturating_duration_since(self.window.start).as_secs() as usize;
                let planned_eligible_seconds = end_bucket.saturating_sub(begin_bucket);
                let validated = |is_audio| delivery.iter()
                    .filter(|consumer| consumer.attempt == Some(index + 1) && consumer.passed
                        && consumer.is_audio == Some(is_audio))
                    .filter_map(|consumer| owners.get(&consumer.producer_id))
                    .filter(|(owner, kind)| *kind == is_audio && *owner != self.client_id && plan.stable_publishers.contains(owner))
                    .map(|(owner, _)| owner)
                    .collect::<std::collections::HashSet<_>>().len();
                let validated_audio = validated(true);
                let validated_video = validated(false);
                let packets_queued = measurement.queued_by_attempt.get(&(index + 1))
                    .map_or(0, |queued| queued.packets);
                let mut failure_reasons = Vec::new();
                if report.room_join_ms.is_none() {
                    failure_reasons.push("Room admission did not complete".into());
                }
                if state.failed {
                    failure_reasons.push("A client, signaling or media error was recorded for this attempt".into());
                }
                if planned_eligible_seconds > 0 {
                    for (kind, expected, observed) in [
                        ("audio", plan.expected_audio, validated_audio),
                        ("video", plan.expected_video, validated_video),
                    ] {
                        if observed < expected {
                            failure_reasons.push(format!(
                                "Expected {expected} distinct stable {kind} publishers with validated delivery, observed {observed}"
                            ));
                        }
                    }
                    if plan.is_publisher && packets_queued == 0 {
                        failure_reasons.push("Publisher queued no media in this attempt's measurement interval".into());
                    }
                }
                let skipped_short_tail = planned_eligible_seconds == 0 && failure_reasons.is_empty();
                report.coverage = Some(AttemptCoverage {
                    planned_eligible_seconds,
                    expected_audio: plan.expected_audio,
                    expected_video: plan.expected_video,
                    validated_audio,
                    validated_video,
                    packets_queued,
                    passed: planned_eligible_seconds > 0 && failure_reasons.is_empty(),
                    skipped_short_tail,
                    failure_reasons,
                });
            }
            report
        }).collect()
    }
}

fn delivery_coverage(buckets: &[u64]) -> (usize, usize) {
    let mut active = 0;
    let mut gap = 0;
    let mut longest = 0;
    for packets in buckets {
        if *packets > 0 {
            active += 1;
            gap = 0;
        } else {
            gap += 1;
            longest = longest.max(gap);
        }
    }
    (active, longest)
}
