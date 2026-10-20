//! Tests of `src/media/diagnostics.rs`.

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
    let _guard = registry.write();
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
        registry.write().insert(
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
    registry.write().insert(
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
        assert_eq!(row.streams[0].score, 10);
        assert_eq!(row.streams[0].encoding_index, None);
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
    let mut invalid_score = consumer_stat(1, 1);
    invalid_score.score = 11;
    mismatched.kind = MediaKind::Audio;
    apply_consumer_stats(
        &mut mismatched,
        ConsumerStats::JustConsumer((invalid_score,)),
    );
    assert_eq!(mismatched.status, EntityStatus::Error);
    assert!(mismatched.streams.is_empty());
}

#[test]
fn encoding_indices_are_nullable_ssrc_joins_and_reject_ambiguous_metadata() {
    let score = |ssrc, encoding_idx| ProducerScore {
        encoding_idx,
        ssrc,
        rid: Some("private-rid-is-never-serialized".into()),
        score: 0,
    };
    let mut producer = row();
    producer.entity_type = EntityType::Producer;
    apply_producer_stats(
        &mut producer,
        vec![producer_stat(7, 5), producer_stat(8, 6)],
    );
    apply_encoding_indices(&mut producer, vec![score(8, 2)]);
    assert_eq!(producer.streams[0].encoding_index, None);
    assert_eq!(producer.streams[1].encoding_index, Some(2));
    assert_eq!(producer.streams[1].score, 10);
    assert!(
        !serde_json::to_string(&producer)
            .unwrap()
            .contains("private-rid")
    );
    for scores in [
        vec![score(7, 256)],
        vec![score(7, 0), score(7, 1)],
        vec![score(7, 0), score(8, 0)],
    ] {
        apply_producer_stats(&mut producer, vec![producer_stat(7, 1)]);
        apply_encoding_indices(&mut producer, scores);
        assert_eq!(producer.status, EntityStatus::Error);
        assert!(producer.streams.is_empty());
    }
    apply_producer_stats(&mut producer, vec![producer_stat(7, 1)]);
    apply_encoding_indices(
        &mut producer,
        (0..=MAX_STREAMS as u32)
            .map(|index| score(index, index))
            .collect(),
    );
    assert_eq!(producer.status, EntityStatus::StreamLimit);
    assert_eq!(producer.streams[0].encoding_index, None);
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
    assert_eq!(value["schemaVersion"], 2);
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
        [
            "encodingIndex",
            "packetCount",
            "rtpBytes",
            "score",
            "ssrc",
            "workerTimestampMs"
        ]
    );
    assert!(entity["streams"][0]["encodingIndex"].is_null());
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
                .insert(consumer.id(), consumer.clone());
            participant
                .producers
                .insert(producer.id(), producer.clone());
            participant.producers.insert(
                crate::ids::fixtures::prid("duplicate-producer"),
                producer.clone(),
            );
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
                participant.producers.insert(
                    crate::ids::fixtures::prid(&format!("duplicate-{index}")),
                    producer.clone(),
                );
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

        registry.write().clear();
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
