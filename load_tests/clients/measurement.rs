/// One wall-clock interval shared by every client, including clients that churn.
pub struct MeasurementWindow {
    pub start: Instant,
    pub end: Instant,
    publishers: std::sync::Mutex<HashMap<String, (Instant, Option<Instant>)>>,
}

impl MeasurementWindow {
    pub fn new(start: Instant, duration: std::time::Duration) -> Self {
        Self {
            start,
            end: start + duration,
            publishers: std::sync::Mutex::new(HashMap::new()),
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConsumerDelivery {
    pub consumer_id: String,
    pub producer_id: String,
    pub ssrc: u32,
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
    created: Instant,
    closed: Option<Instant>,
    packets_by_second: Vec<u64>,
}

/// Detailed observations are separate from the legacy lifetime counters.
pub struct Measurements {
    connection_start: std::sync::Mutex<Instant>,
    attempts: std::sync::Mutex<Vec<ConnectionAttempt>>,
    window: std::sync::Arc<MeasurementWindow>,
    measurement: std::sync::Mutex<MeasurementMetrics>,
    consumers: std::sync::Mutex<Vec<ConsumerState>>,
    subscriptions: std::sync::Mutex<HashMap<String, bool>>,
    current_producers: std::sync::Mutex<Vec<String>>,
}

impl Measurements {
    fn new(window: std::sync::Arc<MeasurementWindow>) -> Self {
        Self {
            connection_start: std::sync::Mutex::new(Instant::now()),
            attempts: std::sync::Mutex::new(Vec::new()),
            measurement: std::sync::Mutex::new(MeasurementMetrics {
                duration_ms: window.end.duration_since(window.start).as_millis() as u64,
                ..Default::default()
            }),
            window,
            consumers: std::sync::Mutex::new(Vec::new()),
            subscriptions: std::sync::Mutex::new(HashMap::new()),
            current_producers: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// Called inside the spawned task, immediately before each WebSocket attempt.
    pub fn begin_connection_attempt(&self) {
        *self.connection_start.lock().unwrap() = Instant::now();
        self.attempts.lock().unwrap().push(ConnectionAttempt {
            room_join_ms: None,
            send_media_ready_ms: None,
            receive_media_ready_ms: None,
        });
        self.subscriptions.lock().unwrap().clear();
    }

    fn attempt_elapsed_ms(&self) -> u64 {
        self.connection_start.lock().unwrap().elapsed().as_millis() as u64
    }

    pub fn mark_media_ready(&self, is_send: bool) {
        let elapsed = self.attempt_elapsed_ms();
        if let Some(attempt) = self.attempts.lock().unwrap().last_mut() {
            let field = if is_send {
                &mut attempt.send_media_ready_ms
            } else {
                &mut attempt.receive_media_ready_ms
            };
            field.get_or_insert(elapsed);
        }
    }

    pub fn record_publisher(&self, producer_id: &str) {
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
        self.consumers.lock().unwrap().push(ConsumerState {
            consumer_id: consumer_id.to_string(),
            producer_id: producer_id.to_string(),
            ssrc,
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

    fn record_queued(&self, size: usize) {
        if self.window.bucket(Instant::now()).is_some() {
            let mut measurement = self.measurement.lock().unwrap();
            measurement.packets_queued += 1;
            measurement.bytes_queued += size as u64;
        }
    }

    fn record_received(&self, size: usize) {
        if self.window.bucket(Instant::now()).is_some() {
            let mut measurement = self.measurement.lock().unwrap();
            measurement.packets_received += 1;
            measurement.bytes_received += size as u64;
        }
    }

    fn record_ssrc(&self, ssrc: u32) {
        if let Some(bucket) = self.window.bucket(Instant::now()) {
            if let Some(consumer) = self
                .consumers
                .lock()
                .unwrap()
                .iter_mut()
                .rev()
                .find(|c| c.ssrc == ssrc && c.closed.is_none())
            {
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
