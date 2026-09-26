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
    /// Exact owner/kind edges for repeatable non-churning workloads.
    pub planned_targets: Option<crate::subscription_plan::PlannedTargets>,
}

/// A stable-peer floor for discovery workloads, or exact planned graph coverage.
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
    /// Absent for historical artifacts and discovery-based workloads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub planned_subscriptions: Option<PlannedSubscriptionCoverage>,
}

/// Exact directed subscriptions observed against the frozen workload graph.
/// A passing graph requires each planned edge for the complete shared window;
/// this is bucketed receipt evidence, not a packet-loss measurement.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlannedSubscriptionCoverage {
    pub targets: crate::subscription_plan::PlannedTargets,
    /// All consumers with known publisher ownership, including invalid edges.
    /// Unknown ownership is a separate attempt failure, never a fallback edge.
    pub realized: Vec<PlannedSubscriptionEdge>,
    pub passed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlannedSubscriptionEdge {
    pub publisher_client_id: String,
    pub producer_id: String,
    pub consumer_id: String,
    pub is_audio: bool,
}

struct PlannedCoverageEvaluation {
    report: PlannedSubscriptionCoverage,
    validated_audio: usize,
    validated_video: usize,
    failure_reasons: Vec<String>,
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
    /// From the resume request to the first packet: for video, the wait for a
    /// keyframe, since a new simulcast consumer forwards nothing before one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_packet_ms: Option<u64>,
    pub eligible_seconds: usize,
    pub seconds_with_packets: usize,
    pub longest_gap_seconds: usize,
    pub passed: bool,
    pub skipped_short_lived: bool,
}

/// Recent delivery failure evidence, using the shared measurement clock.
/// Consumer ordinals refer to the complete, append-only delivery report; the
/// end-exclusive bucket range contains exactly three completed empty seconds.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReceiverStallTrigger {
    pub consumer_ordinal: usize,
    pub ssrc: u32,
    pub is_audio: Option<bool>,
    pub begin_bucket: usize,
    pub end_bucket: usize,
}

struct ConsumerState {
    consumer_id: String,
    producer_id: String,
    ssrc: u32,
    attempt: usize,
    is_audio: Option<bool>,
    planned_end: Option<Instant>,
    // Frozen at registration; packet accounting needs no new lookup or lock.
    full_window: bool,
    created: Instant,
    resumed: Option<Instant>,
    first_packet: Option<Instant>,
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

    /// Inspect existing delivery buckets without adding packet-path counters.
    /// Callers control opt-in polling and one-shot capture admission separately.
    pub fn receiver_stall(&self, ordinal: usize) -> Option<ReceiverStallTrigger> {
        self.receiver_stall_at(ordinal, Instant::now())
    }

    fn receiver_stall_at(&self, ordinal: usize, now: Instant) -> Option<ReceiverStallTrigger> {
        if now < self.window.start
            || now >= self.window.end
            || ordinal == 0
            || ordinal != self.attempts.lock().unwrap().len()
        {
            return None;
        }
        let live_attempt = self
            .measurement
            .lock()
            .unwrap()
            .queued_by_attempt
            .get(&ordinal)
            .is_some_and(|attempt| !attempt.ended && attempt.deadline.is_none_or(|end| now < end));
        if !live_attempt {
            return None;
        }

        // Match delivery_report's lock order. No attempt/measurement guard is
        // retained while inspecting publisher lifetimes and consumer buckets.
        let publishers = self.window.publishers.lock().unwrap();
        let consumers = self.consumers.lock().unwrap();
        consumers.iter().enumerate().find_map(|(index, consumer)| {
            let publisher = publishers.get(&consumer.producer_id);
            if consumer.attempt != ordinal
                || consumer.closed.is_some()
                || consumer.planned_end.is_some_and(|end| now >= end)
                || publisher.is_some_and(|(_, ended)| ended.is_some())
            {
                return None;
            }
            let eligible = consumer_eligible_buckets(&self.window, consumer, publisher, now);
            let begin_bucket = eligible.end.checked_sub(3)?;
            if begin_bucket < eligible.start
                || !consumer
                    .packets_by_second
                    .get(begin_bucket..eligible.end)?
                    .iter()
                    .all(|packets| *packets == 0)
            {
                return None;
            }
            Some(ReceiverStallTrigger {
                consumer_ordinal: index + 1,
                ssrc: consumer.ssrc,
                is_audio: consumer.is_audio,
                begin_bucket,
                end_bucket: eligible.end,
            })
        })
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
        let (attempt, planned_end, full_window) = {
            let attempts = self.attempts.lock().unwrap();
            let plan = attempts.last().and_then(|attempt| attempt.plan.as_ref());
            (
                attempts.len(),
                plan.map(|plan| plan.deadline),
                plan.is_some_and(|plan| plan.planned_targets.is_some()),
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
            full_window,
            created: Instant::now(),
            resumed: None,
            first_packet: None,
            closed: None,
            packets_by_second: vec![
                0;
                self.window.end.duration_since(self.window.start).as_secs()
                    as usize
            ],
        });
    }

    pub fn record_resume_requested(&self, consumer_id: &str) {
        let now = Instant::now();
        if let Some(consumer) = self
            .consumers
            .lock()
            .unwrap()
            .iter_mut()
            .rev()
            .find(|c| c.consumer_id == consumer_id && c.closed.is_none())
        {
            consumer.resumed.get_or_insert(now);
        }
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

    /// Joins happen during the ramp, so first packets count outside the window.
    fn record_ssrc(&self, attempt: usize, ssrc: u32) {
        let now = Instant::now();
        let bucket = self.window.bucket(now);
        if let Some(consumer) = self
            .consumers
            .lock()
            .unwrap()
            .iter_mut()
            .rev()
            .find(|c| c.attempt == attempt && c.ssrc == ssrc && c.closed.is_none())
        {
            consumer.first_packet.get_or_insert(now);
            if let Some(bucket) = bucket {
                consumer.packets_by_second[bucket] += 1;
            }
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
                let range =
                    consumer_eligible_buckets(&self.window, consumer, publisher, self.window.end);
                let eligible = consumer.packets_by_second.get(range).unwrap_or_default();
                let (seconds_with_packets, longest_gap_seconds) = delivery_coverage(eligible);
                let passed = if consumer.full_window {
                    !eligible.is_empty()
                        && seconds_with_packets == eligible.len()
                        && consumer.created <= self.window.start
                        && consumer
                            .closed
                            .is_none_or(|closed| closed >= self.window.end)
                        && consumer
                            .planned_end
                            .is_some_and(|end| end >= self.window.end)
                        && publisher.is_some_and(|(created, closed)| {
                            *created <= self.window.start
                                && closed.is_none_or(|closed| closed >= self.window.end)
                        })
                } else {
                    !eligible.is_empty() && seconds_with_packets > 0 && longest_gap_seconds <= 2
                };
                ConsumerDelivery {
                    consumer_id: consumer.consumer_id.clone(),
                    producer_id: consumer.producer_id.clone(),
                    ssrc: consumer.ssrc,
                    attempt: (consumer.attempt > 0).then_some(consumer.attempt),
                    is_audio: consumer.is_audio,
                    packets_by_second: consumer.packets_by_second.clone(),
                    first_packet_ms: consumer.resumed.zip(consumer.first_packet).map(
                        |(resumed, first)| {
                            first.saturating_duration_since(resumed).as_millis() as u64
                        },
                    ),
                    eligible_seconds: eligible.len(),
                    seconds_with_packets,
                    longest_gap_seconds,
                    passed,
                    skipped_short_lived: !consumer.full_window && eligible.is_empty(),
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
                let planned_eligible_seconds = if plan.planned_targets.is_some() {
                    self.window.end.duration_since(self.window.start).as_secs() as usize
                } else {
                    end_bucket.saturating_sub(begin_bucket)
                };
                let validated = |is_audio| delivery.iter()
                    .filter(|consumer| consumer.attempt == Some(index + 1) && consumer.passed
                        && consumer.is_audio == Some(is_audio))
                    .filter_map(|consumer| owners.get(&consumer.producer_id))
                    .filter(|(owner, kind)| *kind == is_audio && *owner != self.client_id && plan.stable_publishers.contains(owner))
                    .map(|(owner, _)| owner)
                    .collect::<std::collections::HashSet<_>>().len();
                let planned = plan.planned_targets.as_ref().map(|targets| {
                    planned_subscription_coverage(
                        &self.client_id,
                        index + 1,
                        targets,
                        delivery,
                        &owners,
                        planned_eligible_seconds,
                    )
                });
                let validated_audio = planned.as_ref().map_or_else(|| validated(true), |coverage| coverage.validated_audio);
                let validated_video = planned.as_ref().map_or_else(|| validated(false), |coverage| coverage.validated_video);
                let packets_queued = measurement.queued_by_attempt.get(&(index + 1))
                    .map_or(0, |queued| queued.packets);
                let mut failure_reasons = Vec::new();
                if report.room_join_ms.is_none() {
                    failure_reasons.push("Room admission did not complete".into());
                }
                if state.failed {
                    failure_reasons.push("A client, signaling or media error was recorded for this attempt".into());
                }
                let planned_subscriptions = planned.map(|mut evaluation| {
                    if planned_eligible_seconds == 0 {
                        evaluation.failure_reasons.push("The planned graph has no complete measurement seconds".into());
                    }
                    if state.started > self.window.start
                        || report.room_join_ms.is_some_and(|elapsed| {
                            state.started + std::time::Duration::from_millis(elapsed) > self.window.start
                        })
                    {
                        evaluation.failure_reasons.push("Planned room admission completed after the shared measurement window began".into());
                    }
                    if plan.deadline < self.window.end {
                        evaluation.failure_reasons.push("Planned attempt lifetime does not cover the full shared measurement window".into());
                    }
                    if evaluation.report.targets.audio.len() != plan.expected_audio
                        || evaluation.report.targets.video.len() != plan.expected_video
                    {
                        evaluation.failure_reasons.push("Planned subscription counts disagree with attempt expectations".into());
                    }
                    // The exact-graph verdict is scoped to the attempt: an
                    // attempt that failed cannot carry a passing graph.
                    evaluation.report.passed =
                        failure_reasons.is_empty() && evaluation.failure_reasons.is_empty();
                    failure_reasons.extend(evaluation.failure_reasons);
                    evaluation.report
                });
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
                let skipped_short_tail = plan.planned_targets.is_none()
                    && planned_eligible_seconds == 0 && failure_reasons.is_empty();
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
                    planned_subscriptions,
                });
            }
            report
        }).collect()
    }
}

fn planned_subscription_coverage(
    client_id: &str,
    ordinal: usize,
    targets: &crate::subscription_plan::PlannedTargets,
    delivery: &[ConsumerDelivery],
    owners: &HashMap<String, (String, bool)>,
    planned_seconds: usize,
) -> PlannedCoverageEvaluation {
    let mut failure_reasons = Vec::new();
    let mut expected = std::collections::BTreeSet::new();
    for (is_audio, publishers) in [(true, &targets.audio), (false, &targets.video)] {
        for publisher in publishers {
            if publisher == client_id || !expected.insert((publisher.clone(), is_audio)) {
                failure_reasons.push(format!(
                    "Invalid self or duplicate planned {} target {publisher}",
                    media_kind(is_audio),
                ));
            }
        }
    }

    let mut realized = Vec::new();
    let mut observed = std::collections::BTreeMap::<_, Vec<&ConsumerDelivery>>::new();
    let mut consumer_ids = std::collections::HashSet::new();
    for consumer in delivery
        .iter()
        .filter(|consumer| consumer.attempt == Some(ordinal))
    {
        if !consumer_ids.insert(&consumer.consumer_id) {
            failure_reasons.push(format!(
                "Duplicate planned consumer identifier {}",
                consumer.consumer_id,
            ));
        }
        let Some((publisher, is_audio)) = owners.get(&consumer.producer_id) else {
            failure_reasons.push(format!(
                "Unknown publisher owner for consumer {} and producer {}",
                consumer.consumer_id, consumer.producer_id,
            ));
            continue;
        };
        realized.push(PlannedSubscriptionEdge {
            publisher_client_id: publisher.clone(),
            producer_id: consumer.producer_id.clone(),
            consumer_id: consumer.consumer_id.clone(),
            is_audio: *is_audio,
        });
        if consumer.is_audio != Some(*is_audio) {
            failure_reasons.push(format!(
                "Consumer {} media kind does not match owned {} producer {}",
                consumer.consumer_id,
                media_kind(*is_audio),
                consumer.producer_id,
            ));
        }
        let edge = (publisher.clone(), *is_audio);
        if !expected.contains(&edge) {
            failure_reasons.push(format!(
                "Unexpected planned {} edge from {publisher}",
                media_kind(*is_audio),
            ));
        }
        observed.entry(edge).or_default().push(consumer);
    }

    let mut validated_audio = 0;
    let mut validated_video = 0;
    for (publisher, is_audio) in &expected {
        let consumers = observed
            .get(&(publisher.clone(), *is_audio))
            .map_or(&[][..], Vec::as_slice);
        if consumers.len() != 1 {
            failure_reasons.push(format!(
                "Expected exactly one planned {} edge from {publisher}, observed {}",
                media_kind(*is_audio),
                consumers.len(),
            ));
            continue;
        }
        let consumer = consumers[0];
        if !consumer.passed
            || consumer.skipped_short_lived
            || consumer.is_audio != Some(*is_audio)
            || consumer.eligible_seconds != planned_seconds
            || consumer.packets_by_second.len() != planned_seconds
            || consumer.packets_by_second.contains(&0)
        {
            failure_reasons.push(format!(
                "Planned {} edge from {publisher} did not deliver in every shared measurement second with a full-window owned lifetime",
                media_kind(*is_audio),
            ));
            continue;
        }
        if *is_audio {
            validated_audio += 1;
        } else {
            validated_video += 1;
        }
    }
    realized.sort_unstable_by(|left, right| {
        (
            &left.publisher_client_id,
            !left.is_audio,
            &left.producer_id,
            &left.consumer_id,
        )
            .cmp(&(
                &right.publisher_client_id,
                !right.is_audio,
                &right.producer_id,
                &right.consumer_id,
            ))
    });
    failure_reasons.sort_unstable();
    failure_reasons.dedup();
    PlannedCoverageEvaluation {
        report: PlannedSubscriptionCoverage {
            targets: targets.clone(),
            realized,
            passed: failure_reasons.is_empty(),
        },
        validated_audio,
        validated_video,
        failure_reasons,
    }
}

fn media_kind(is_audio: bool) -> &'static str {
    if is_audio { "audio" } else { "video" }
}

/// Planned graphs inspect the entire shared window without lifetime exemptions.
/// Discovery workloads allow subscription/renegotiation to settle, then inspect
/// complete seconds; owned departures end their eligibility without grace.
fn consumer_eligible_buckets(
    window: &MeasurementWindow,
    consumer: &ConsumerState,
    publisher: Option<&(Instant, Option<Instant>)>,
    observed_until: Instant,
) -> std::ops::Range<usize> {
    if consumer.full_window {
        return 0..observed_until
            .min(window.end)
            .saturating_duration_since(window.start)
            .as_secs() as usize;
    }
    let start = (consumer.created + std::time::Duration::from_secs(3)).max(window.start);
    let start = publisher.map_or(start, |(created, _)| {
        start.max(*created + std::time::Duration::from_secs(3))
    });
    let end = consumer
        .closed
        .unwrap_or(window.end)
        .min(window.end)
        .min(observed_until);
    let end = consumer.planned_end.map_or(end, |planned| end.min(planned));
    let end = publisher
        .and_then(|(_, end)| *end)
        .map_or(end, |closed| end.min(closed));
    let begin_bucket = start
        .saturating_duration_since(window.start)
        .as_millis()
        .div_ceil(1000) as usize;
    let end_bucket = end.saturating_duration_since(window.start).as_secs() as usize;
    begin_bucket..end_bucket
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

#[cfg(test)]
mod planned_subscription_tests {
    use super::*;
    use crate::subscription_plan::PlannedTargets;
    use std::sync::Arc;
    use std::time::Duration;

    fn fixture() -> (Measurements, Instant) {
        let start = Instant::now() - Duration::from_secs(20);
        let observations = Measurements::new(
            "viewer".into(),
            Arc::new(MeasurementWindow::new(start, Duration::from_secs(20))),
        );
        observations.begin_planned_attempt(AttemptPlan {
            deadline: start + Duration::from_secs(20),
            stable_publishers: Arc::default(),
            expected_audio: 1,
            expected_video: 1,
            is_publisher: false,
            planned_targets: Some(PlannedTargets {
                audio: vec!["audio-peer".into()],
                video: vec!["video-peer".into()],
            }),
        });
        let mut attempts = observations.attempts.lock().unwrap();
        attempts[0].started = start - Duration::from_secs(5);
        attempts[0].report.room_join_ms = Some(1);
        drop(attempts);
        (observations, start)
    }

    fn edge(observations: &Measurements, producer: &str, publisher: &str, is_audio: bool) {
        let created = observations.window.start - Duration::from_secs(3);
        observations
            .window
            .publisher_owners
            .lock()
            .unwrap()
            .insert(producer.into(), (publisher.into(), is_audio));
        observations
            .window
            .publishers
            .lock()
            .unwrap()
            .insert(producer.into(), (created, None));
        observations.subscribe(producer, is_audio);
        observations.record_consumer(producer, producer, 123);
        let mut consumers = observations.consumers.lock().unwrap();
        let consumer = consumers.last_mut().unwrap();
        consumer.created = created;
        consumer.packets_by_second.fill(1);
    }

    fn complete_graph(observations: &Measurements) {
        // Deliberately register in reverse canonical report order.
        edge(observations, "video", "video-peer", false);
        edge(observations, "audio", "audio-peer", true);
    }

    fn coverage(observations: &Measurements) -> AttemptCoverage {
        observations.attempt_report(&observations.delivery_report())[0]
            .coverage
            .clone()
            .unwrap()
    }

    fn assert_failed(observations: &Measurements) -> AttemptCoverage {
        let report = coverage(observations);
        assert!(!report.passed && !report.skipped_short_tail);
        assert_eq!(report.planned_eligible_seconds, 20);
        assert!(!report.planned_subscriptions.as_ref().unwrap().passed);
        assert!(!report.failure_reasons.is_empty());
        report
    }

    #[test]
    fn exact_graph_requires_full_window_delivery_and_serializes_canonical_evidence() {
        let (observations, _) = fixture();
        complete_graph(&observations);
        let report = coverage(&observations);
        assert!(report.passed && !report.skipped_short_tail);
        assert_eq!((report.validated_audio, report.validated_video), (1, 1));
        assert_eq!(report.planned_eligible_seconds, 20);
        let graph = report.planned_subscriptions.as_ref().unwrap();
        assert!(graph.passed);
        assert_eq!(graph.realized.len(), 2);
        assert_eq!(graph.realized[0].publisher_client_id, "audio-peer");
        assert_eq!(graph.realized[1].publisher_client_id, "video-peer");
        for delivery in observations.delivery_report() {
            assert!(delivery.passed && !delivery.skipped_short_lived);
            assert_eq!(delivery.eligible_seconds, 20);
            assert_eq!(delivery.seconds_with_packets, 20);
        }
        let value = serde_json::to_value(&report).unwrap();
        assert_eq!(
            value["plannedSubscriptions"]["targets"]["audio"][0],
            "audio-peer"
        );
        assert_eq!(
            value["plannedSubscriptions"]["realized"][0]["consumerId"],
            "audio"
        );
        assert_eq!(
            value["plannedSubscriptions"]["realized"][0]["producerId"],
            "audio"
        );
        assert_eq!(
            value["plannedSubscriptions"]["realized"][0]["isAudio"],
            true
        );
        let restored: AttemptCoverage = serde_json::from_value(value).unwrap();
        assert!(restored.planned_subscriptions.unwrap().passed);
    }

    #[test]
    fn correct_subscription_count_with_wrong_target_is_not_coverage() {
        let (observations, _) = fixture();
        edge(&observations, "audio", "different-peer", true);
        edge(&observations, "video", "video-peer", false);
        let report = assert_failed(&observations);
        assert_eq!((report.validated_audio, report.validated_video), (0, 1));
        assert_eq!(report.planned_subscriptions.unwrap().realized.len(), 2);
        assert!(
            report
                .failure_reasons
                .iter()
                .any(|reason| reason.contains("Unexpected planned audio edge"))
        );
        assert!(
            report
                .failure_reasons
                .iter()
                .any(|reason| reason.contains("audio-peer, observed 0"))
        );
    }

    #[test]
    fn unknown_owner_has_no_fallback_and_keeps_an_explicit_failure() {
        let (observations, _) = fixture();
        complete_graph(&observations);
        observations
            .window
            .publisher_owners
            .lock()
            .unwrap()
            .remove("audio");
        let report = assert_failed(&observations);
        assert_eq!(report.planned_subscriptions.unwrap().realized.len(), 1);
        assert!(
            report
                .failure_reasons
                .iter()
                .any(|reason| reason.contains("Unknown publisher owner for consumer audio"))
        );
    }

    #[test]
    fn consumer_kind_must_match_authoritative_owned_producer_kind() {
        let (observations, _) = fixture();
        complete_graph(&observations);
        observations.consumers.lock().unwrap()[1].is_audio = Some(false);
        let report = assert_failed(&observations);
        assert_eq!((report.validated_audio, report.validated_video), (0, 1));
        assert!(
            report
                .failure_reasons
                .iter()
                .any(|reason| reason.contains("media kind does not match"))
        );
    }

    #[test]
    fn duplicate_and_extra_edges_fail_even_when_expected_counts_are_available() {
        let (observations, _) = fixture();
        complete_graph(&observations);
        edge(&observations, "duplicate-audio", "audio-peer", true);
        let duplicate = assert_failed(&observations);
        assert_eq!(
            (duplicate.validated_audio, duplicate.validated_video),
            (0, 1)
        );
        assert_eq!(duplicate.planned_subscriptions.unwrap().realized.len(), 3);
        assert!(
            duplicate
                .failure_reasons
                .iter()
                .any(|reason| reason.contains("audio-peer, observed 2"))
        );

        let (observations, _) = fixture();
        complete_graph(&observations);
        edge(&observations, "extra", "extra-peer", true);
        let extra = assert_failed(&observations);
        assert_eq!((extra.validated_audio, extra.validated_video), (1, 1));
        assert_eq!(extra.planned_subscriptions.unwrap().realized.len(), 3);
    }

    #[test]
    fn a_failed_attempt_never_reports_a_passing_planned_graph() {
        let (observations, _) = fixture();
        complete_graph(&observations);
        observations.attempts.lock().unwrap()[0].report.room_join_ms = None;
        let failed = assert_failed(&observations);
        assert!(!failed.planned_subscriptions.unwrap().passed);
    }

    #[test]
    fn absent_publishers_and_failed_setup_do_not_remove_planned_expectations() {
        let (observations, _) = fixture();
        let absent = assert_failed(&observations);
        assert_eq!((absent.expected_audio, absent.expected_video), (1, 1));
        assert_eq!((absent.validated_audio, absent.validated_video), (0, 0));
        assert!(absent.planned_subscriptions.unwrap().realized.is_empty());
        observations.attempts.lock().unwrap()[0].report.room_join_ms = None;
        let failed_setup = assert_failed(&observations);
        assert!(
            failed_setup
                .failure_reasons
                .iter()
                .any(|reason| reason == "Room admission did not complete")
        );
    }

    #[test]
    fn late_attempt_and_late_admission_cannot_shorten_the_measurement_window() {
        for late_start in [false, true] {
            let (observations, start) = fixture();
            complete_graph(&observations);
            {
                let mut attempts = observations.attempts.lock().unwrap();
                if late_start {
                    attempts[0].started = start + Duration::from_secs(19);
                } else {
                    attempts[0].report.room_join_ms = Some(5001);
                }
            }
            let report = assert_failed(&observations);
            assert!(
                report
                    .failure_reasons
                    .iter()
                    .any(|reason| reason.contains("admission completed after"))
            );
        }
    }

    #[test]
    fn late_consumer_or_late_publisher_cannot_pass_using_a_shorter_lifetime() {
        for late_publisher in [false, true] {
            let (observations, start) = fixture();
            complete_graph(&observations);
            if late_publisher {
                observations
                    .window
                    .publishers
                    .lock()
                    .unwrap()
                    .get_mut("audio")
                    .unwrap()
                    .0 = start + Duration::from_secs(19);
            } else {
                observations.consumers.lock().unwrap()[1].created = start + Duration::from_secs(19);
            }
            assert_failed(&observations);
            let report = observations.delivery_report();
            assert!(!report[1].passed && !report[1].skipped_short_lived);
            assert_eq!(report[1].eligible_seconds, 20);
        }
    }

    #[test]
    fn early_consumer_publisher_or_attempt_departure_cannot_excuse_missing_tail() {
        for early_boundary in ["consumer", "publisher", "attempt"] {
            let (observations, start) = fixture();
            complete_graph(&observations);
            let early_end = start + Duration::from_secs(1);
            match early_boundary {
                "consumer" => observations.consumers.lock().unwrap()[1].closed = Some(early_end),
                "publisher" => {
                    observations
                        .window
                        .publishers
                        .lock()
                        .unwrap()
                        .get_mut("audio")
                        .unwrap()
                        .1 = Some(early_end)
                }
                "attempt" => {
                    observations.consumers.lock().unwrap()[1].planned_end = Some(early_end);
                    observations.attempts.lock().unwrap()[0]
                        .plan
                        .as_mut()
                        .unwrap()
                        .deadline = early_end;
                }
                _ => unreachable!(),
            }
            assert_failed(&observations);
            assert_eq!(observations.delivery_report()[1].eligible_seconds, 20);
        }
    }

    #[test]
    fn one_empty_bucket_at_any_position_fails_the_strict_graph() {
        for empty_bucket in [0, 9, 19] {
            let (observations, _) = fixture();
            complete_graph(&observations);
            observations.consumers.lock().unwrap()[1].packets_by_second[empty_bucket] = 0;
            assert_failed(&observations);
            let report = observations.delivery_report();
            assert!(!report[1].passed && !report[1].skipped_short_lived);
            assert_eq!(report[1].longest_gap_seconds, 1);
            assert_eq!(report[1].seconds_with_packets, 19);
        }
    }

    #[test]
    fn same_peer_can_have_one_audio_and_one_video_edge_but_not_reused_consumer_ids() {
        let (observations, _) = fixture();
        observations.attempts.lock().unwrap()[0]
            .plan
            .as_mut()
            .unwrap()
            .planned_targets
            .as_mut()
            .unwrap()
            .video = vec!["audio-peer".into()];
        edge(&observations, "video", "audio-peer", false);
        edge(&observations, "audio", "audio-peer", true);
        let graph = coverage(&observations).planned_subscriptions.unwrap();
        assert!(graph.passed);
        assert!(graph.realized[0].is_audio);
        assert!(!graph.realized[1].is_audio);
        observations.consumers.lock().unwrap()[1].consumer_id = "video".into();
        let report = assert_failed(&observations);
        assert!(
            report
                .failure_reasons
                .iter()
                .any(|reason| reason.contains("Duplicate planned consumer identifier"))
        );
    }

    #[test]
    fn unplanned_artifacts_omit_exact_graph_evidence_without_inventing_it() {
        let (observations, _) = fixture();
        observations.attempts.lock().unwrap()[0]
            .plan
            .as_mut()
            .unwrap()
            .planned_targets = None;
        let value = serde_json::to_value(coverage(&observations)).unwrap();
        assert!(value.get("plannedSubscriptions").is_none());
        let restored: AttemptCoverage = serde_json::from_value(value).unwrap();
        assert!(restored.planned_subscriptions.is_none());
    }
}

#[cfg(test)]
mod receiver_stall_tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;

    fn fixture() -> (Measurements, Instant) {
        let start = Instant::now() + Duration::from_secs(60);
        let observations = Measurements::new(
            "receiver".into(),
            Arc::new(MeasurementWindow::new(start, Duration::from_secs(30))),
        );
        begin_attempt(&observations, start + Duration::from_secs(30));
        (observations, start)
    }

    fn begin_attempt(observations: &Measurements, deadline: Instant) {
        observations.begin_planned_attempt(AttemptPlan {
            deadline,
            stable_publishers: Arc::new(Default::default()),
            expected_audio: 1,
            expected_video: 1,
            is_publisher: false,
            planned_targets: None,
        });
    }

    fn consumer(observations: &Measurements, producer: &str, created: Instant, audio: bool) {
        observations.subscribe(producer, audio);
        observations.record_consumer(producer, producer, 123);
        observations
            .consumers
            .lock()
            .unwrap()
            .last_mut()
            .unwrap()
            .created = created;
    }

    #[test]
    fn receiver_stall_requires_three_latest_complete_empty_eligible_seconds() {
        let (observations, start) = fixture();
        consumer(
            &observations,
            "video",
            start - Duration::from_secs(3),
            false,
        );
        assert!(
            observations
                .receiver_stall_at(1, start + Duration::from_millis(2999))
                .is_none()
        );
        let trigger = observations
            .receiver_stall_at(1, start + Duration::from_secs(3))
            .unwrap();
        assert_eq!((trigger.begin_bucket, trigger.end_bucket), (0, 3));
        assert_eq!(
            (trigger.consumer_ordinal, trigger.ssrc, trigger.is_audio),
            (1, 123, Some(false))
        );

        // A packet in the current incomplete second does not rewrite the
        // preceding three empty completed seconds.
        observations.consumers.lock().unwrap()[0].packets_by_second[3] = 1;
        assert!(
            observations
                .receiver_stall_at(1, start + Duration::from_millis(3999))
                .is_some()
        );
        assert!(
            observations
                .receiver_stall_at(1, start + Duration::from_secs(4))
                .is_none()
        );
        assert!(
            observations
                .receiver_stall_at(1, start + Duration::from_secs(6))
                .is_none()
        );
        let resumed_gap = observations
            .receiver_stall_at(1, start + Duration::from_secs(7))
            .unwrap();
        assert_eq!((resumed_gap.begin_bucket, resumed_gap.end_bucket), (4, 7));
    }

    #[test]
    fn receiver_stall_shares_settling_and_partial_start_geometry_with_delivery() {
        let (observations, start) = fixture();
        consumer(
            &observations,
            "video",
            start + Duration::from_millis(500),
            false,
        );
        assert!(
            observations
                .receiver_stall_at(1, start + Duration::from_millis(6999))
                .is_none()
        );
        let trigger = observations
            .receiver_stall_at(1, start + Duration::from_secs(7))
            .unwrap();
        assert_eq!((trigger.begin_bucket, trigger.end_bucket), (4, 7));
        assert_eq!(observations.delivery_report()[0].eligible_seconds, 26);

        observations
            .window
            .publishers
            .lock()
            .unwrap()
            .insert("video".into(), (start + Duration::from_millis(1500), None));
        assert!(
            observations
                .receiver_stall_at(1, start + Duration::from_millis(7999))
                .is_none()
        );
        let trigger = observations
            .receiver_stall_at(1, start + Duration::from_secs(8))
            .unwrap();
        assert_eq!((trigger.begin_bucket, trigger.end_bucket), (5, 8));
        assert_eq!(observations.delivery_report()[0].eligible_seconds, 25);
    }

    #[test]
    fn receiver_stall_excludes_warmup_shared_end_and_partial_final_second() {
        let (observations, start) = fixture();
        consumer(
            &observations,
            "audio",
            start - Duration::from_secs(10),
            true,
        );
        assert!(
            observations
                .receiver_stall_at(1, start - Duration::from_nanos(1))
                .is_none()
        );
        assert!(observations.receiver_stall_at(1, start).is_none());
        let trigger = observations
            .receiver_stall_at(1, start + Duration::from_millis(29999))
            .unwrap();
        assert_eq!((trigger.begin_bucket, trigger.end_bucket), (26, 29));
        assert!(
            observations
                .receiver_stall_at(1, start + Duration::from_secs(30))
                .is_none()
        );
        assert!(
            observations
                .receiver_stall_at(1, start + Duration::from_secs(31))
                .is_none()
        );
    }

    #[test]
    fn receiver_stall_requires_current_live_attempt_and_retains_full_consumer_ordinal() {
        let (observations, start) = fixture();
        consumer(&observations, "old", start - Duration::from_secs(3), true);
        assert!(
            observations
                .receiver_stall_at(0, start + Duration::from_secs(4))
                .is_none()
        );
        assert!(
            observations
                .receiver_stall_at(2, start + Duration::from_secs(4))
                .is_none()
        );
        begin_attempt(&observations, start + Duration::from_secs(30));
        assert!(
            observations
                .receiver_stall_at(1, start + Duration::from_secs(4))
                .is_none()
        );
        assert!(
            observations
                .receiver_stall_at(2, start + Duration::from_secs(4))
                .is_none()
        );
        consumer(
            &observations,
            "current",
            start - Duration::from_secs(3),
            false,
        );
        let trigger = observations
            .receiver_stall_at(2, start + Duration::from_secs(4))
            .unwrap();
        assert_eq!(trigger.consumer_ordinal, 2);
        observations.end_session();
        assert!(
            observations
                .receiver_stall_at(2, start + Duration::from_secs(4))
                .is_none()
        );
    }

    #[test]
    fn receiver_stall_ends_at_planned_deadline_even_without_owned_cleanup() {
        let (observations, start) = fixture();
        begin_attempt(&observations, start + Duration::from_millis(7500));
        consumer(&observations, "audio", start - Duration::from_secs(3), true);
        assert!(
            observations
                .receiver_stall_at(2, start + Duration::from_millis(7499))
                .is_some()
        );
        assert!(
            observations
                .receiver_stall_at(2, start + Duration::from_millis(7500))
                .is_none()
        );
        assert!(
            observations
                .receiver_stall_at(2, start + Duration::from_secs(8))
                .is_none()
        );
        assert_eq!(observations.delivery_report()[0].eligible_seconds, 7);
        assert!(observations.consumers.lock().unwrap()[0].closed.is_none());
    }

    #[test]
    fn receiver_stall_skips_closed_and_owned_retired_but_keeps_unknown_publishers() {
        let (observations, start) = fixture();
        for producer in ["closed", "retired", "unknown"] {
            consumer(
                &observations,
                producer,
                start - Duration::from_secs(3),
                true,
            );
        }
        observations.consumers.lock().unwrap()[0].closed = Some(start + Duration::from_secs(4));
        observations.window.publishers.lock().unwrap().insert(
            "retired".into(),
            (
                start - Duration::from_secs(3),
                Some(start + Duration::from_secs(4)),
            ),
        );
        let trigger = observations
            .receiver_stall_at(1, start + Duration::from_secs(5))
            .unwrap();
        assert_eq!(trigger.consumer_ordinal, 3);
        assert_eq!((trigger.begin_bucket, trigger.end_bucket), (2, 5));
        observations.consumers.lock().unwrap()[2].closed = Some(start + Duration::from_secs(5));
        assert!(
            observations
                .receiver_stall_at(1, start + Duration::from_secs(5))
                .is_none()
        );
    }

    #[test]
    fn receiver_stall_does_not_excuse_server_closure_of_live_owned_publisher() {
        let (observations, start) = fixture();
        consumer(&observations, "live", start - Duration::from_secs(3), false);
        observations
            .window
            .publishers
            .lock()
            .unwrap()
            .insert("live".into(), (start - Duration::from_secs(3), None));
        let (_, unexpected) = observations.close_producer("live");
        assert!(unexpected);
        assert!(
            observations
                .receiver_stall_at(1, start + Duration::from_secs(4))
                .is_some()
        );
        assert!(observations.consumers.lock().unwrap()[0].closed.is_none());

        let (unknown, unknown_start) = fixture();
        consumer(
            &unknown,
            "unknown",
            unknown_start - Duration::from_secs(3),
            false,
        );
        assert!(
            unknown
                .receiver_stall_at(1, unknown_start + Duration::from_secs(4))
                .is_some()
        );
        let (_, unexpected) = unknown.close_producer("unknown");
        assert!(!unexpected);
        assert!(
            unknown
                .receiver_stall_at(1, unknown_start + Duration::from_secs(4))
                .is_none()
        );
    }

    #[test]
    fn receiver_stall_skips_healthy_consumers_without_copying_or_changing_buckets() {
        let (observations, start) = fixture();
        consumer(
            &observations,
            "healthy",
            start - Duration::from_secs(3),
            true,
        );
        consumer(
            &observations,
            "stalled",
            start - Duration::from_secs(3),
            false,
        );
        observations.consumers.lock().unwrap()[0]
            .packets_by_second
            .fill(1);
        let before = observations.delivery_report();
        let trigger = observations
            .receiver_stall_at(1, start + Duration::from_secs(10))
            .unwrap();
        assert_eq!(trigger.consumer_ordinal, 2);
        let after = observations.delivery_report();
        assert_eq!(
            serde_json::to_value(before).unwrap(),
            serde_json::to_value(after).unwrap()
        );
        let value = serde_json::to_value(&trigger).unwrap();
        assert_eq!(value["consumerOrdinal"], 2);
        assert_eq!(value["beginBucket"], 7);
        assert_eq!(value["endBucket"], 10);
        let restored: ReceiverStallTrigger = serde_json::from_value(value).unwrap();
        assert_eq!(restored.ssrc, trigger.ssrc);
    }
}
