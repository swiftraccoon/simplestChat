#![forbid(unsafe_code)]

//! Bounded, opt-in observations of worker intake and forwarding counters.
//!
//! These are not client-delivery acknowledgments. Worker timestamps are monotonic
//! milliseconds, while observation times use this context's process-local clock.
//! Pause flags and worker counters are separate observations, not an atomic view.

use super::types::ParticipantMedia;
use futures_util::{StreamExt, stream};
use mediasoup::consumer::{Consumer, ConsumerStat, ConsumerStats, WeakConsumer};
use mediasoup::producer::{Producer, ProducerStat, WeakProducer};
use mediasoup_types::rtp_parameters::MediaKind;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use uuid::Uuid;

const MAX_PARTICIPANTS: usize = 64;
const MAX_ENTITIES: usize = 1_024;
const MAX_STREAMS: usize = 16;
const MAX_IN_FLIGHT: usize = 8;
const SNAPSHOT_BUDGET: Duration = Duration::from_millis(750);
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
const REFERENCE_DOMAIN: &[u8] = b"simplestchat-media-v1\0";

type ParticipantRegistry = RwLock<HashMap<String, Arc<Mutex<ParticipantMedia>>>>;

/// Public correlation namespace and monotonic clock shared by all samples.
/// The salt is intentionally disclosed in each response; it is not a credential.
pub struct SnapshotContext {
    salt: [u8; 16],
    started: Instant,
    next_sample: AtomicU64,
}

impl Default for SnapshotContext {
    fn default() -> Self {
        Self::new()
    }
}

impl SnapshotContext {
    pub fn new() -> Self {
        Self {
            salt: *Uuid::new_v4().as_bytes(),
            started: Instant::now(),
            next_sample: AtomicU64::new(1),
        }
    }

    pub fn correlation_salt(&self) -> String {
        hex::encode(self.salt)
    }

    /// Hashes only the closed set of native UUID identity domains.
    pub fn reference(&self, kind: ReferenceKind, id: Uuid) -> String {
        let mut digest = Sha256::new();
        digest.update(REFERENCE_DOMAIN);
        digest.update(self.salt);
        digest.update(kind.label());
        digest.update(b"\0");
        digest.update(id.hyphenated().to_string().as_bytes());
        hex::encode(digest.finalize())
    }

    fn elapsed_us(&self) -> u64 {
        self.started
            .elapsed()
            .as_micros()
            .min(MAX_SAFE_INTEGER as u128) as u64
    }

    fn next_sample(&self) -> (u64, bool) {
        // Exhaustion cannot wrap or produce a JSON number that loses precision.
        // The endpoint's request bound makes exhaustion impractical; if reached,
        // its terminal identifier is explicitly an incomplete observation.
        match self
            .next_sample
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                (value <= MAX_SAFE_INTEGER).then_some(value.saturating_add(1))
            }) {
            Ok(value) => (value, true),
            Err(_) => (MAX_SAFE_INTEGER, false),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReferenceKind {
    Consumer,
    Producer,
    Transport,
}

impl ReferenceKind {
    fn label(self) -> &'static [u8] {
        match self {
            Self::Consumer => b"consumer",
            Self::Producer => b"producer",
            Self::Transport => b"transport",
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaSnapshot {
    pub schema_version: u8,
    pub correlation_salt: String,
    pub sample_id: u64,
    pub started_us: u64,
    pub finished_us: u64,
    pub coverage: SnapshotCoverage,
    pub entities: Vec<EntitySnapshot>,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotCoverage {
    pub complete: bool,
    pub registry_busy: bool,
    pub participants_observed: usize,
    pub participants_visited: usize,
    pub busy_participants: usize,
    pub participant_limit_reached: bool,
    pub entity_limit_reached: bool,
    pub deadline_reached: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EntitySnapshot {
    pub entity_type: EntityType,
    pub reference: String,
    pub transport_reference: Option<String>,
    pub producer_reference: Option<String>,
    pub kind: MediaKind,
    pub status: EntityStatus,
    pub paused: Option<bool>,
    pub producer_paused: Option<bool>,
    pub observed_us: u64,
    pub streams: Vec<StreamSnapshot>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum EntityType {
    Consumer,
    Producer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EntityStatus {
    Ok,
    Closed,
    Timeout,
    Error,
    NoStreams,
    StreamLimit,
    NotCollected,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamSnapshot {
    pub ssrc: u32,
    pub packet_count: u64,
    pub rtp_bytes: u64,
    pub worker_timestamp_ms: u64,
}

enum WeakEntity {
    Consumer(WeakConsumer),
    Producer(WeakProducer),
}

struct PendingEntity {
    handle: WeakEntity,
    id: Uuid,
    transport_id: Uuid,
    producer_id: Option<Uuid>,
    kind: MediaKind,
}

impl PendingEntity {
    fn consumer(consumer: &Consumer) -> Self {
        Self {
            handle: WeakEntity::Consumer(consumer.downgrade()),
            id: consumer.id().into(),
            transport_id: consumer.transport().id().into(),
            producer_id: Some(consumer.producer_id().into()),
            kind: consumer.kind(),
        }
    }

    fn producer(producer: &Producer) -> Self {
        Self {
            handle: WeakEntity::Producer(producer.downgrade()),
            id: producer.id().into(),
            transport_id: producer.transport().id().into(),
            producer_id: None,
            kind: producer.kind(),
        }
    }

    fn row(&self, context: &SnapshotContext) -> EntitySnapshot {
        let (entity_type, reference_kind) = match self.handle {
            WeakEntity::Consumer(_) => (EntityType::Consumer, ReferenceKind::Consumer),
            WeakEntity::Producer(_) => (EntityType::Producer, ReferenceKind::Producer),
        };
        EntitySnapshot {
            entity_type,
            reference: context.reference(reference_kind, self.id),
            transport_reference: Some(
                context.reference(ReferenceKind::Transport, self.transport_id),
            ),
            producer_reference: self
                .producer_id
                .map(|id| context.reference(ReferenceKind::Producer, id)),
            kind: self.kind,
            status: EntityStatus::NotCollected,
            paused: None,
            producer_paused: None,
            observed_us: context.elapsed_us(),
            streams: Vec::new(),
        }
    }
}

/// Copies a bounded weak-handle inventory without waiting for application locks.
/// No participant or registry guard survives this synchronous function.
fn inventory(
    registry: &ParticipantRegistry,
    deadline: tokio::time::Instant,
    coverage: &mut SnapshotCoverage,
) -> Vec<PendingEntity> {
    let participants = match registry.try_read() {
        Ok(participants) => {
            coverage.participants_observed = participants.len().min(MAX_SAFE_INTEGER as usize);
            coverage.participant_limit_reached = participants.len() > MAX_PARTICIPANTS;
            participants
                .values()
                .take(MAX_PARTICIPANTS)
                .cloned()
                .collect::<Vec<_>>()
        }
        Err(_) => {
            coverage.registry_busy = true;
            return Vec::new();
        }
    };

    let mut entities = Vec::new();
    let mut seen = HashSet::new();
    let mut examined = 0;
    for participant in participants {
        if tokio::time::Instant::now() >= deadline {
            coverage.deadline_reached = true;
            break;
        }
        let Ok(participant) = participant.try_lock() else {
            coverage.busy_participants += 1;
            continue;
        };
        coverage.participants_visited += 1;
        let available = MAX_ENTITIES - examined;
        if participant
            .consumers
            .len()
            .saturating_add(participant.producers.len())
            > available
        {
            coverage.entity_limit_reached = true;
        }
        for pending in participant
            .consumers
            .values()
            .map(PendingEntity::consumer)
            .chain(participant.producers.values().map(PendingEntity::producer))
            .take(available)
        {
            examined += 1;
            let kind = match pending.handle {
                WeakEntity::Consumer(_) => EntityType::Consumer,
                WeakEntity::Producer(_) => EntityType::Producer,
            };
            if seen.insert((kind, pending.id)) {
                entities.push(pending);
            }
        }
        // Continue the bounded participant scan even when the entity budget is
        // full, so an exactly full inventory is not incorrectly called truncated.
        // The budget counts examined entries, including duplicate handles. Thus
        // malformed duplicate registries may conservatively report a limit.
    }
    entities
}

pub(super) async fn collect_snapshot(
    registry: &ParticipantRegistry,
    context: &SnapshotContext,
) -> MediaSnapshot {
    let started_us = context.elapsed_us();
    let deadline = tokio::time::Instant::now() + SNAPSHOT_BUDGET;
    let (sample_id, sample_available) = context.next_sample();
    let mut coverage = SnapshotCoverage::default();
    let pending = inventory(registry, deadline, &mut coverage);
    let mut entities =
        stream::iter(pending.into_iter().enumerate())
            .map(|(index, pending)| async move {
                (index, collect_entity(pending, context, deadline).await)
            })
            .buffer_unordered(MAX_IN_FLIGHT)
            .collect::<Vec<_>>()
            .await;
    entities.sort_unstable_by_key(|(index, _)| *index);
    let entities = entities.into_iter().map(|(_, row)| row).collect::<Vec<_>>();
    coverage.deadline_reached |= tokio::time::Instant::now() >= deadline
        || entities.iter().any(|row| {
            matches!(
                row.status,
                EntityStatus::Timeout | EntityStatus::NotCollected
            )
        });
    coverage.complete = sample_available
        && !coverage.registry_busy
        && coverage.busy_participants == 0
        && !coverage.participant_limit_reached
        && !coverage.entity_limit_reached
        && !coverage.deadline_reached
        && entities.iter().all(|row| row.status == EntityStatus::Ok);
    MediaSnapshot {
        schema_version: 1,
        correlation_salt: context.correlation_salt(),
        sample_id,
        started_us,
        finished_us: context.elapsed_us(),
        coverage,
        entities,
    }
}

async fn collect_entity(
    pending: PendingEntity,
    context: &SnapshotContext,
    deadline: tokio::time::Instant,
) -> EntitySnapshot {
    let mut row = pending.row(context);
    if tokio::time::Instant::now() >= deadline {
        return row;
    }
    // Upgrades occur only after buffer_unordered admits this request. At most
    // eight entities can extend a native handle lifetime, each until the shared
    // deadline; queued observations hold weak handles only.
    match pending.handle {
        WeakEntity::Consumer(weak) => {
            if let Some(consumer) = weak.upgrade() {
                row.paused = Some(consumer.paused());
                row.producer_paused = Some(consumer.producer_paused());
                if consumer.closed() {
                    row.status = EntityStatus::Closed;
                } else {
                    match bounded_request(deadline, consumer.get_stats()).await {
                        Ok(stats) if !consumer.closed() => apply_consumer_stats(&mut row, stats),
                        _ if consumer.closed() => row.status = EntityStatus::Closed,
                        Err(status) => row.status = status,
                        Ok(_) => row.status = EntityStatus::Closed,
                    }
                }
            } else {
                row.status = EntityStatus::Closed;
            }
        }
        WeakEntity::Producer(weak) => {
            if let Some(producer) = weak.upgrade() {
                row.paused = Some(producer.paused());
                if producer.closed() {
                    row.status = EntityStatus::Closed;
                } else {
                    match bounded_request(deadline, producer.get_stats()).await {
                        Ok(stats) if !producer.closed() => apply_producer_stats(&mut row, stats),
                        _ if producer.closed() => row.status = EntityStatus::Closed,
                        Err(status) => row.status = status,
                        Ok(_) => row.status = EntityStatus::Closed,
                    }
                }
            } else {
                row.status = EntityStatus::Closed;
            }
        }
    }
    row.observed_us = context.elapsed_us();
    row
}

async fn bounded_request<T, E>(
    deadline: tokio::time::Instant,
    future: impl Future<Output = Result<T, E>>,
) -> Result<T, EntityStatus> {
    match tokio::time::timeout_at(deadline, future).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(_)) => Err(EntityStatus::Error),
        Err(_) => Err(EntityStatus::Timeout),
    }
}

fn consumer_stream(stat: ConsumerStat) -> (MediaKind, StreamSnapshot) {
    (
        stat.kind,
        StreamSnapshot {
            ssrc: stat.ssrc,
            packet_count: stat.packet_count,
            rtp_bytes: stat.byte_count,
            worker_timestamp_ms: stat.timestamp,
        },
    )
}

fn apply_consumer_stats(row: &mut EntitySnapshot, stats: ConsumerStats) {
    // WithProducer includes selected producer intake. It must never be counted
    // as consumer forwarding or substituted for the independent producer row.
    match stats {
        ConsumerStats::JustConsumer((consumer,)) | ConsumerStats::WithProducer((consumer, _)) => {
            apply_streams(row, std::iter::once(consumer_stream(consumer)))
        }
        ConsumerStats::MultipleConsumers(consumers) => {
            apply_streams(row, consumers.into_iter().map(consumer_stream))
        }
    }
}

fn apply_producer_stats(row: &mut EntitySnapshot, stats: Vec<ProducerStat>) {
    apply_streams(
        row,
        stats.into_iter().map(|stat| {
            (
                stat.kind,
                StreamSnapshot {
                    ssrc: stat.ssrc,
                    packet_count: stat.packet_count,
                    rtp_bytes: stat.byte_count,
                    worker_timestamp_ms: stat.timestamp,
                },
            )
        }),
    );
}

fn apply_streams(
    row: &mut EntitySnapshot,
    streams: impl ExactSizeIterator<Item = (MediaKind, StreamSnapshot)>,
) {
    row.status = match streams.len() {
        0 => EntityStatus::NoStreams,
        count if count > MAX_STREAMS => EntityStatus::StreamLimit,
        _ => EntityStatus::Ok,
    };
    row.streams.clear();
    let mut ssrcs = HashSet::new();
    for (kind, stream) in streams.take(MAX_STREAMS) {
        if kind != row.kind
            || !ssrcs.insert(stream.ssrc)
            || stream.packet_count > MAX_SAFE_INTEGER
            || stream.rtp_bytes > MAX_SAFE_INTEGER
            || stream.worker_timestamp_ms > MAX_SAFE_INTEGER
        {
            row.status = EntityStatus::Error;
            row.streams.clear();
            return;
        }
        row.streams.push(stream);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn context() -> SnapshotContext {
        SnapshotContext {
            salt: std::array::from_fn(|index| index as u8),
            started: Instant::now(),
            next_sample: AtomicU64::new(1),
        }
    }

    fn row() -> EntitySnapshot {
        EntitySnapshot {
            entity_type: EntityType::Consumer,
            reference: context().reference(ReferenceKind::Consumer, Uuid::nil()),
            transport_reference: None,
            producer_reference: None,
            kind: MediaKind::Audio,
            status: EntityStatus::NotCollected,
            paused: None,
            producer_paused: None,
            observed_us: 0,
            streams: Vec::new(),
        }
    }

    fn stat_json(ssrc: u32, packets: u64) -> Value {
        json!({
            "timestamp": 1234,
            "ssrc": ssrc,
            "kind": "audio",
            "mimeType": "audio/opus",
            "packetsLost": 0,
            "fractionLost": 0,
            "jitter": 0,
            "packetsDiscarded": 0,
            "packetsRetransmitted": 0,
            "packetsRepaired": 0,
            "nackCount": 0,
            "nackPacketCount": 0,
            "pliCount": 0,
            "firCount": 0,
            "packetCount": packets,
            "byteCount": 456,
            "bitrate": 0,
            "score": 10,
            "bitrateByLayer": []
        })
    }

    fn consumer_stat(ssrc: u32, packets: u64) -> ConsumerStat {
        serde_json::from_value(stat_json(ssrc, packets)).unwrap()
    }

    fn producer_stat(ssrc: u32, packets: u64) -> ProducerStat {
        serde_json::from_value(stat_json(ssrc, packets)).unwrap()
    }

    #[test]
    fn references_match_cross_language_known_vectors_and_separate_domains() {
        let context = context();
        let id = Uuid::parse_str("00112233-4455-6677-8899-AABBCCDDEEFF").unwrap();
        assert_eq!(
            context.correlation_salt(),
            "000102030405060708090a0b0c0d0e0f"
        );
        for (kind, expected) in [
            (
                ReferenceKind::Consumer,
                "eac2af8305a6e800ecb20b2e3f219717ac02b1a9441211f66b02925a02bd4476",
            ),
            (
                ReferenceKind::Producer,
                "62a02b5b202ec18c5406cdca6fcdf02384a544fd334b3a40554a9b09e8edeb58",
            ),
            (
                ReferenceKind::Transport,
                "d7e23b31b6f5575274a1850ee33100d25e41beba47bad071483cb1c261b7fd65",
            ),
        ] {
            assert_eq!(context.reference(kind, id), expected);
        }
        let other = SnapshotContext::new();
        assert_ne!(other.correlation_salt(), context.correlation_salt());
        assert_ne!(
            other.reference(ReferenceKind::Consumer, id),
            context.reference(ReferenceKind::Consumer, id)
        );
    }

    #[tokio::test]
    async fn empty_registry_is_complete_and_sample_ids_increase() {
        let registry = ParticipantRegistry::default();
        let context = context();
        let first = collect_snapshot(&registry, &context).await;
        let second = collect_snapshot(&registry, &context).await;
        assert!(first.coverage.complete);
        assert!(first.entities.is_empty());
        assert_eq!(first.sample_id, 1);
        assert_eq!(second.sample_id, 2);
        assert!(first.started_us <= first.finished_us);
        assert!(first.finished_us <= second.started_us);
        assert_eq!(first.correlation_salt, second.correlation_salt);
        assert_eq!(first.coverage.participants_observed, 0);
    }

    #[tokio::test]
    async fn samples_never_wrap_or_expose_unsafe_json_numbers() {
        let registry = ParticipantRegistry::default();
        let context = context();
        context
            .next_sample
            .store(MAX_SAFE_INTEGER, Ordering::Relaxed);
        let last = collect_snapshot(&registry, &context).await;
        let exhausted = collect_snapshot(&registry, &context).await;
        assert_eq!(last.sample_id, MAX_SAFE_INTEGER);
        assert!(last.coverage.complete);
        assert_eq!(exhausted.sample_id, MAX_SAFE_INTEGER);
        assert!(!exhausted.coverage.complete);
    }

    #[test]
    fn registry_contention_is_explicit_and_never_waits() {
        let registry = ParticipantRegistry::default();
        let _guard = registry.write().unwrap();
        let mut coverage = SnapshotCoverage::default();
        let started = Instant::now();
        let pending = inventory(
            &registry,
            tokio::time::Instant::now() + SNAPSHOT_BUDGET,
            &mut coverage,
        );
        assert!(started.elapsed() < Duration::from_millis(100));
        assert!(coverage.registry_busy);
        assert!(pending.is_empty());
    }

    #[tokio::test]
    async fn participant_contention_and_scan_limit_are_explicit() {
        let registry = ParticipantRegistry::default();
        let participant = Arc::new(Mutex::new(ParticipantMedia::new("private-id".into())));
        registry
            .write()
            .unwrap()
            .insert("private-id".into(), participant.clone());
        let guard = participant.lock().await;
        let busy = collect_snapshot(&registry, &context()).await;
        assert!(!busy.coverage.complete);
        assert_eq!(busy.coverage.participants_observed, 1);
        assert_eq!(busy.coverage.participants_visited, 0);
        assert_eq!(busy.coverage.busy_participants, 1);
        assert!(!serde_json::to_string(&busy).unwrap().contains("private-id"));
        drop(guard);
        for index in 0..MAX_PARTICIPANTS {
            registry.write().unwrap().insert(
                index.to_string(),
                Arc::new(Mutex::new(ParticipantMedia::new(index.to_string()))),
            );
        }
        let limited = collect_snapshot(&registry, &context()).await;
        assert!(!limited.coverage.complete);
        assert!(limited.coverage.participant_limit_reached);
        assert_eq!(limited.coverage.participants_observed, MAX_PARTICIPANTS + 1);
        assert_eq!(limited.coverage.participants_visited, MAX_PARTICIPANTS);
        assert!(limited.entities.is_empty());
    }

    #[test]
    fn expired_inventory_deadline_never_visits_a_participant() {
        let registry = ParticipantRegistry::default();
        registry.write().unwrap().insert(
            "participant".into(),
            Arc::new(Mutex::new(ParticipantMedia::new("participant".into()))),
        );
        let mut coverage = SnapshotCoverage::default();
        assert!(inventory(&registry, tokio::time::Instant::now(), &mut coverage).is_empty());
        assert!(coverage.deadline_reached);
        assert_eq!(coverage.participants_visited, 0);
    }

    #[tokio::test]
    async fn native_request_errors_and_timeout_are_distinct_and_cancelled() {
        struct DropSignal(Arc<std::sync::atomic::AtomicBool>);
        impl Drop for DropSignal {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Relaxed);
            }
        }
        let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let signal = DropSignal(dropped.clone());
        let timed_out = bounded_request(
            tokio::time::Instant::now() + Duration::from_millis(10),
            async move {
                let _signal = signal;
                std::future::pending::<Result<(), ()>>().await
            },
        )
        .await;
        assert_eq!(timed_out, Err(EntityStatus::Timeout));
        assert!(dropped.load(Ordering::Relaxed));
        let failed = bounded_request(tokio::time::Instant::now() + SNAPSHOT_BUDGET, async {
            Err::<(), _>("private native error must not escape")
        })
        .await;
        assert_eq!(failed, Err(EntityStatus::Error));
        assert_eq!(
            serde_json::to_string(&failed.unwrap_err()).unwrap(),
            "\"error\""
        );
        assert_eq!(
            bounded_request(tokio::time::Instant::now() + SNAPSHOT_BUDGET, async {
                Ok::<_, ()>(5)
            })
            .await,
            Ok(5)
        );
    }

    #[test]
    fn consumer_variants_never_mix_in_producer_intake() {
        for stats in [
            ConsumerStats::JustConsumer((consumer_stat(10, 3),)),
            ConsumerStats::WithProducer((consumer_stat(10, 3), producer_stat(20, 9_999))),
            ConsumerStats::MultipleConsumers(vec![consumer_stat(10, 3)]),
        ] {
            let mut row = row();
            apply_consumer_stats(&mut row, stats);
            assert_eq!(row.status, EntityStatus::Ok);
            assert_eq!(row.streams.len(), 1);
            assert_eq!(row.streams[0].ssrc, 10);
            assert_eq!(row.streams[0].packet_count, 3);
            assert_eq!(row.streams[0].rtp_bytes, 456);
            assert_eq!(row.streams[0].worker_timestamp_ms, 1234);
        }
    }

    #[test]
    fn empty_native_stats_are_no_streams_not_zero_traffic_success() {
        let mut consumer = row();
        apply_consumer_stats(&mut consumer, ConsumerStats::MultipleConsumers(Vec::new()));
        assert_eq!(consumer.status, EntityStatus::NoStreams);
        let mut producer = row();
        apply_producer_stats(&mut producer, Vec::new());
        assert_eq!(producer.status, EntityStatus::NoStreams);
    }

    #[test]
    fn streams_are_bounded_and_reject_unsafe_or_inconsistent_stats() {
        let mut limited = row();
        apply_consumer_stats(
            &mut limited,
            ConsumerStats::MultipleConsumers(
                (0..=MAX_STREAMS)
                    .map(|ssrc| consumer_stat(ssrc as u32, 1))
                    .collect(),
            ),
        );
        assert_eq!(limited.status, EntityStatus::StreamLimit);
        assert_eq!(limited.streams.len(), MAX_STREAMS);
        let mut duplicate = row();
        apply_consumer_stats(
            &mut duplicate,
            ConsumerStats::MultipleConsumers(vec![consumer_stat(5, 1), consumer_stat(5, 2)]),
        );
        assert_eq!(duplicate.status, EntityStatus::Error);
        assert!(duplicate.streams.is_empty());
        for field in ["packetCount", "byteCount", "timestamp"] {
            let mut invalid = stat_json(1, 1);
            invalid[field] = json!(MAX_SAFE_INTEGER + 1);
            let mut invalid_row = row();
            apply_producer_stats(
                &mut invalid_row,
                vec![serde_json::from_value(invalid).unwrap()],
            );
            assert_eq!(invalid_row.status, EntityStatus::Error);
            assert!(invalid_row.streams.is_empty());
        }
        let mut mismatched = row();
        mismatched.kind = MediaKind::Video;
        apply_consumer_stats(
            &mut mismatched,
            ConsumerStats::JustConsumer((consumer_stat(1, 1),)),
        );
        assert_eq!(mismatched.status, EntityStatus::Error);
        assert!(mismatched.streams.is_empty());
    }

    #[tokio::test]
    async fn serialized_schema_is_exact_and_nullable_fields_are_present() {
        let mut snapshot = collect_snapshot(&ParticipantRegistry::default(), &context()).await;
        let mut entity = row();
        apply_consumer_stats(
            &mut entity,
            ConsumerStats::JustConsumer((consumer_stat(1, 2),)),
        );
        snapshot.entities.push(entity);
        let value = serde_json::to_value(snapshot).unwrap();
        fn keys(value: &Value) -> Vec<&str> {
            let mut keys = value
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>();
            keys.sort_unstable();
            keys
        }
        assert_eq!(
            keys(&value),
            [
                "correlationSalt",
                "coverage",
                "entities",
                "finishedUs",
                "sampleId",
                "schemaVersion",
                "startedUs"
            ]
        );
        assert_eq!(
            keys(&value["coverage"]),
            [
                "busyParticipants",
                "complete",
                "deadlineReached",
                "entityLimitReached",
                "participantLimitReached",
                "participantsObserved",
                "participantsVisited",
                "registryBusy"
            ]
        );
        let entity = &value["entities"][0];
        assert_eq!(
            keys(entity),
            [
                "entityType",
                "kind",
                "observedUs",
                "paused",
                "producerPaused",
                "producerReference",
                "reference",
                "status",
                "streams",
                "transportReference"
            ]
        );
        assert_eq!(entity["entityType"], "consumer");
        assert_eq!(entity["kind"], "audio");
        assert_eq!(entity["status"], "ok");
        for field in [
            "paused",
            "producerPaused",
            "producerReference",
            "transportReference",
        ] {
            assert!(entity[field].is_null());
        }
        assert_eq!(
            keys(&entity["streams"][0]),
            ["packetCount", "rtpBytes", "ssrc", "workerTimestampMs"]
        );
    }

    #[tokio::test]
    async fn native_weak_inventory_deduplicates_bounds_and_observes_closed_handles() {
        use mediasoup::prelude::*;
        use std::num::{NonZeroU8, NonZeroU32};

        // A direct transport requires no listening socket or external service.
        // Every native resource is RAII-owned within this bounded test future.
        tokio::time::timeout(Duration::from_secs(10), async {
            let manager = WorkerManager::new();
            let worker = manager
                .create_worker(WorkerSettings::default())
                .await
                .unwrap();
            let codecs = crate::media::config::RouterConfig::default().media_codecs;
            let router = worker
                .create_router(RouterOptions::new(codecs.clone()))
                .await
                .unwrap();
            let transport = router
                .create_direct_transport(DirectTransportOptions::default())
                .await
                .unwrap();
            let producer = transport
                .produce(ProducerOptions::new(
                    MediaKind::Audio,
                    RtpParameters {
                        mid: Some("audio".into()),
                        codecs: vec![RtpCodecParameters::Audio {
                            mime_type: MimeTypeAudio::Opus,
                            payload_type: 111,
                            clock_rate: NonZeroU32::new(48_000).unwrap(),
                            channels: NonZeroU8::new(2).unwrap(),
                            parameters: RtpCodecParametersParameters::default(),
                            rtcp_feedback: vec![],
                        }],
                        ..RtpParameters::default()
                    },
                ))
                .await
                .unwrap();
            let mut options = ConsumerOptions::new(
                producer.id(),
                RtpCapabilities {
                    codecs,
                    ..RtpCapabilities::default()
                },
            );
            options.paused = true;
            let consumer = transport.consume(options).await.unwrap();
            let participant = Arc::new(Mutex::new(ParticipantMedia::new(
                "private-participant".into(),
            )));
            {
                let mut participant = participant.lock().await;
                participant
                    .consumers
                    .insert(consumer.id().to_string(), consumer.clone());
                participant
                    .producers
                    .insert(producer.id().to_string(), producer.clone());
                participant
                    .producers
                    .insert("duplicate-producer".into(), producer.clone());
            }
            let registry = ParticipantRegistry::new(HashMap::from([(
                "private-participant".into(),
                participant.clone(),
            )]));
            let context = context();
            let snapshot = collect_snapshot(&registry, &context).await;
            assert_eq!(
                snapshot.entities.len(),
                2,
                "duplicate producer handles must not duplicate intake"
            );
            assert!(
                !snapshot.coverage.complete,
                "a producer without initialized RTP streams is incomplete"
            );
            let consumer_row = snapshot
                .entities
                .iter()
                .find(|row| row.entity_type == EntityType::Consumer)
                .unwrap();
            assert_eq!(consumer_row.status, EntityStatus::Ok);
            assert_eq!(consumer_row.paused, Some(true));
            assert_eq!(consumer_row.producer_paused, Some(false));
            let producer_row = snapshot
                .entities
                .iter()
                .find(|row| row.entity_type == EntityType::Producer)
                .unwrap();
            assert_eq!(producer_row.status, EntityStatus::NoStreams);
            assert_eq!(producer_row.producer_paused, None);
            assert_eq!(
                consumer_row.producer_reference.as_ref(),
                Some(&producer_row.reference)
            );
            assert_eq!(
                consumer_row.transport_reference,
                producer_row.transport_reference
            );
            let serialized = serde_json::to_string(&snapshot).unwrap();
            for raw in [
                consumer.id().to_string(),
                producer.id().to_string(),
                transport.id().to_string(),
                "private-participant".into(),
            ] {
                assert!(!serialized.contains(&raw));
            }

            let mut coverage = SnapshotCoverage::default();
            let pending = inventory(
                &registry,
                tokio::time::Instant::now() + SNAPSHOT_BUDGET,
                &mut coverage,
            );
            assert_eq!(pending.len(), 2);
            assert!(!coverage.entity_limit_reached);
            {
                let mut participant = participant.lock().await;
                for index in 0..MAX_ENTITIES {
                    participant
                        .producers
                        .insert(format!("duplicate-{index}"), producer.clone());
                }
            }
            let mut limited = SnapshotCoverage::default();
            let bounded = inventory(
                &registry,
                tokio::time::Instant::now() + SNAPSHOT_BUDGET,
                &mut limited,
            );
            assert!(bounded.len() <= 2);
            assert!(
                limited.entity_limit_reached,
                "the scan budget includes duplicate entries"
            );
            drop(bounded);
            let expired = collect_entity(
                PendingEntity::consumer(&consumer),
                &context,
                tokio::time::Instant::now(),
            )
            .await;
            assert_eq!(expired.status, EntityStatus::NotCollected);
            assert_eq!(expired.paused, None);

            registry.write().unwrap().clear();
            drop(participant);
            drop(consumer);
            drop(producer);
            for pending in pending {
                let closed = collect_entity(
                    pending,
                    &context,
                    tokio::time::Instant::now() + SNAPSHOT_BUDGET,
                )
                .await;
                assert_eq!(closed.status, EntityStatus::Closed);
                assert!(closed.streams.is_empty());
                assert_eq!(
                    closed.paused, None,
                    "weak inventory must not keep removed media alive"
                );
            }
        })
        .await
        .expect("native fixture must finish within its bounded cleanup window");
    }
}
