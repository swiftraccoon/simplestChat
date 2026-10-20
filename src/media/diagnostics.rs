#![forbid(unsafe_code)]

//! Bounded, opt-in observations of worker intake and forwarding counters.
//!
//! These are not client-delivery acknowledgments. Worker timestamps are monotonic
//! milliseconds, while observation times use this context's process-local clock.
//! Pause flags and worker counters are separate observations, not an atomic view.

use super::types::ParticipantMedia;
use crate::sync::RwLock;
use futures_util::{StreamExt, stream};
use mediasoup::consumer::{Consumer, ConsumerStat, ConsumerStats, WeakConsumer};
use mediasoup::producer::{Producer, ProducerScore, ProducerStat, WeakProducer};
use mediasoup_types::rtp_parameters::MediaKind;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
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
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
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
    pub score: u8,
    /// Cached producer metadata, joined by SSRC; null for consumers or when the
    /// independently observed score notification has not arrived yet.
    pub encoding_index: Option<u8>,
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
        Some(participants) => {
            coverage.participants_observed = participants.len().min(MAX_SAFE_INTEGER as usize);
            coverage.participant_limit_reached = participants.len() > MAX_PARTICIPANTS;
            participants
                .values()
                .take(MAX_PARTICIPANTS)
                .cloned()
                .collect::<Vec<_>>()
        }
        None => {
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
        schema_version: 2,
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
                        Ok(stats) if !producer.closed() => {
                            apply_producer_stats(&mut row, stats);
                            apply_encoding_indices(&mut row, producer.score());
                        }
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
            score: stat.score,
            encoding_index: None,
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
                    score: stat.score,
                    encoding_index: None,
                },
            )
        }),
    );
}

fn apply_encoding_indices(row: &mut EntitySnapshot, scores: Vec<ProducerScore>) {
    if scores.len() > MAX_STREAMS {
        row.status = EntityStatus::StreamLimit;
        return;
    }
    let mut by_ssrc = HashMap::new();
    let mut indices = HashSet::new();
    for score in scores {
        let Ok(index) = u8::try_from(score.encoding_idx) else {
            row.status = EntityStatus::Error;
            row.streams.clear();
            return;
        };
        if by_ssrc.insert(score.ssrc, index).is_some() || !indices.insert(index) {
            row.status = EntityStatus::Error;
            row.streams.clear();
            return;
        }
    }
    for stream in &mut row.streams {
        stream.encoding_index = by_ssrc.get(&stream.ssrc).copied();
    }
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
            || stream.score > 10
        {
            row.status = EntityStatus::Error;
            row.streams.clear();
            return;
        }
        row.streams.push(stream);
    }
}

#[cfg(test)]
#[path = "diagnostics_tests.rs"]
mod tests;
