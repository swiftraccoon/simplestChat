//! Tests of `src/media/transport_manager.rs`.

#[test]
fn presentation_priority_requires_canonical_video_source() {
    use super::*;
    let screen = AppData::new(ConsumerMediaSource::from_room_source(
        MediaKind::Video,
        Some("screen"),
    ));
    assert_eq!(consumer_video_priority(MediaKind::Video, &screen), 4);
    assert_eq!(consumer_video_priority(MediaKind::Audio, &screen), 1);
    for source in [None, Some("camera"), Some("screen-audio"), Some("Screen")] {
        let data = AppData::new(ConsumerMediaSource::from_room_source(
            MediaKind::Video,
            source,
        ));
        assert_eq!(consumer_video_priority(MediaKind::Video, &data), 1);
    }
    assert_eq!(
        consumer_video_priority(MediaKind::Video, &AppData::default()),
        1
    );
    let untrusted = AppData::new(serde_json::json!({"source": "screen", "priority": 255}));
    assert_eq!(consumer_video_priority(MediaKind::Video, &untrusted), 1);
}

#[test]
fn worker_bitrate_preserves_values_and_saturates_without_wrapping() {
    for bitrate in [0, 200_000, 600_000, u64::from(u32::MAX)] {
        assert_eq!(u64::from(super::application_bitrate(bitrate)), bitrate);
    }
    for bitrate in [u64::from(u32::MAX) + 1, u64::MAX] {
        assert_eq!(super::application_bitrate(bitrate), u32::MAX);
    }
}

use super::*;
use crate::media::config::{MediaConfig, RouterConfig};
use crate::media::router_manager::RouterManager;
use crate::media::worker_manager::WorkerManager;
use crate::source_scan::{
    Invocation, Token, calls, macro_invocations, production_sources, variant_constructions,
};
use std::num::{NonZeroU8, NonZeroU32};

#[tokio::test]
async fn quality_sampler_skips_busy_locks_and_reports_partial_coverage() {
    let manager = TransportManager::new();
    let busy = Arc::new(TokioMutex::new(ParticipantMedia::new("busy".to_owned())));
    let available = Arc::new(TokioMutex::new(ParticipantMedia::new(
        "available".to_owned(),
    )));
    manager.participants.write().extend([
        ("busy".to_owned(), busy.clone()),
        ("available".to_owned(), available),
    ]);
    let held = busy.lock().await;
    let mut cursor = 0;
    let sample = tokio::time::timeout(
        std::time::Duration::from_millis(100),
        manager.quality_sample(&mut cursor, 100),
    )
    .await
    .expect("sampling must not wait for a busy participant");
    assert_eq!(sample.participants_available, 2);
    assert_eq!(sample.participants_sampled, 1);
    assert!(!sample.budget_exhausted);
    assert!(sample.completed_at.is_some());
    drop(held);
    let next = manager.quality_sample(&mut cursor, 0).await;
    assert_eq!(next.participants_sampled, 2);
    assert_eq!(next.transports_requested, 0);
}

struct DropCounter(Arc<AtomicUsize>);

impl Drop for DropCounter {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

#[test]
fn default_consumer_cap_covers_a_32_person_camera_and_microphone_room() {
    // Every participant consumes audio and video from each peer, so a room
    // of N publishers needs 2 * (N - 1) consumers per participant.
    const {
        assert!(super::super::config::DEFAULT_MAX_CONSUMERS_PER_PARTICIPANT >= 2 * 31);
    }
}

#[test]
fn lifecycle_identity_contains_only_the_canonical_public_uuid() {
    let participant_id = Uuid::new_v4();
    let namespace = format!(
        "private room\u{1f}{}\u{1f}{}",
        Uuid::new_v4(),
        participant_id.to_string().to_uppercase()
    );
    assert_eq!(lifecycle_participant_id(&namespace), Some(participant_id));
}

#[test]
fn lifecycle_identity_rejects_unfamiliar_media_namespaces() {
    let participant_id = Uuid::new_v4();
    let session_id = Uuid::new_v4();
    for namespace in [
        "arbitrary private data".to_string(),
        participant_id.to_string(),
        format!("private room\u{1f}{participant_id}"),
        format!("private room\u{1f}not-a-session-uuid\u{1f}{participant_id}"),
        format!("private room\u{1f}{session_id}\u{1f}not-a-participant-uuid"),
        format!("\u{1f}{session_id}\u{1f}{participant_id}"),
    ] {
        assert_eq!(lifecycle_participant_id(&namespace), None);
    }
}

#[test]
fn operational_logs_name_a_media_namespace_by_its_public_uuid() {
    let participant_id = Uuid::new_v4();
    let namespace = format!("private room\u{1f}{}\u{1f}{participant_id}", Uuid::new_v4());
    assert_eq!(logged_participant(&namespace), participant_id.to_string());
    assert_eq!(logged_participant("private room"), "unknown");
}

#[test]
fn replacing_bwe_subscription_drops_the_previous_handler() {
    let manager = TransportManager::new();
    let drops = Arc::new(AtomicUsize::new(0));
    let old_generation = Uuid::new_v4();
    let new_generation = Uuid::new_v4();
    manager.replace_bwe_trace_handler(old_generation, Box::new(DropCounter(drops.clone())));
    assert_eq!(drops.load(Ordering::Relaxed), 0);

    manager.replace_bwe_trace_handler(old_generation, Box::new(DropCounter(drops.clone())));
    assert_eq!(drops.load(Ordering::Relaxed), 1);

    manager.replace_bwe_trace_handler(new_generation, Box::new(DropCounter(drops.clone())));
    manager.clear_bwe_trace_handler(old_generation);
    assert_eq!(drops.load(Ordering::Relaxed), 2);
    assert!(
        manager
            .bwe_trace_handlers
            .read()
            .contains_key(&new_generation),
        "cleanup of an old generation must retain a reused ID's handler"
    );
    manager.clear_bwe_trace_handler(new_generation);
    assert_eq!(drops.load(Ordering::Relaxed), 3);
}

#[test]
fn stale_participant_handle_is_rejected_after_replacement() {
    let manager = TransportManager::new();
    let participant_id = "reused-participant";
    let old = manager.get_or_create_participant(participant_id);
    assert!(manager.participant_is_current(participant_id, &old));

    let replacement = Arc::new(TokioMutex::new(ParticipantMedia::new(
        participant_id.to_string(),
    )));
    manager
        .participants
        .write()
        .insert(participant_id.to_string(), replacement.clone());

    assert!(!manager.participant_is_current(participant_id, &old));
    assert!(manager.participant_is_current(participant_id, &replacement));
}

#[test]
fn unchanged_producer_pause_state_is_suppressed() {
    assert!(!producer_pause_transition_needed(false, false));
    assert!(!producer_pause_transition_needed(true, true));
    assert!(producer_pause_transition_needed(false, true));
    assert!(producer_pause_transition_needed(true, false));
}

#[test]
fn unchanged_consumer_control_state_is_suppressed() {
    assert!(!consumer_pause_transition_needed(false, false));
    assert!(!consumer_pause_transition_needed(true, true));
    assert!(consumer_pause_transition_needed(false, true));
    assert!(consumer_pause_transition_needed(true, false));

    let layers = ConsumerLayers {
        spatial_layer: 1,
        temporal_layer: Some(2),
    };
    assert!(!consumer_layers_transition_needed(Some(layers), layers));
    assert!(consumer_layers_transition_needed(None, layers));
    assert!(consumer_layers_transition_needed(
        Some(layers),
        ConsumerLayers {
            spatial_layer: 2,
            temporal_layer: Some(2),
        }
    ));
}

#[tokio::test]
async fn test_transport_creation() {
    // Keep this test isolated from a running local application and other
    // media tests by asking the OS for an available UDP port first.
    let reservation = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let mut config = MediaConfig::default();
    config.worker_config.num_workers = 1;
    config.webrtc_server_port_base = reservation.local_addr().unwrap().port();
    drop(reservation);
    let config = Arc::new(config);
    let worker_manager = Arc::new(WorkerManager::new(config.clone()).await.unwrap());
    let router_manager = RouterManager::new(worker_manager.clone());
    let transport_manager = TransportManager::new();

    // Create router first
    let room_id = "test-room";
    router_manager
        .create_router(room_id.to_string(), RouterConfig::default())
        .await
        .unwrap();
    let router = router_manager.get_router(room_id).await.unwrap();

    // Look up the WebRtcServer for this room's worker
    let worker_id = router_manager.get_worker_id(room_id).await.unwrap();
    let webrtc_server = worker_manager.get_webrtc_server(worker_id).await.unwrap();
    let consumer_counter = worker_manager.get_consumer_counter(worker_id).unwrap();

    // Create transports via WebRtcServer (shared port)
    let participant_id = "test-participant";
    let send_transport = transport_manager
        .create_send_transport(
            participant_id.to_string(),
            &router,
            webrtc_server.clone(),
            &config.webrtc_transport_config,
        )
        .await;

    assert!(send_transport.is_ok());

    let recv_transport = transport_manager
        .create_recv_transport(
            participant_id.to_string(),
            &router,
            webrtc_server.clone(),
            &config.webrtc_transport_config,
        )
        .await;

    assert!(recv_transport.is_ok());

    // Read the real worker's applied policy, not only the Rust configuration.
    // The outgoing floor and both directional ceilings must remain applied.
    {
        let participant = transport_manager
            .get_participant_lock(participant_id)
            .unwrap();
        let participant = participant.lock().await;
        let sent = participant
            .send_transport
            .as_ref()
            .unwrap()
            .get_stats()
            .await
            .unwrap();
        let received = participant
            .recv_transport
            .as_ref()
            .unwrap()
            .get_stats()
            .await
            .unwrap();
        assert_eq!(sent.len(), 1);
        assert_eq!(received.len(), 1);
        assert_eq!(sent[0].max_incoming_bitrate, Some(3_000_000));
        assert_eq!(received[0].max_outgoing_bitrate, Some(3_000_000));
        assert_eq!(received[0].min_outgoing_bitrate, Some(100_000));
    }

    // Programmatic overrides must reach the worker without losing its cap.
    let override_id = "explicit-minimum".to_string();
    let mut overridden = config.webrtc_transport_config.clone();
    overridden.min_outgoing_bitrate = 60_000;
    transport_manager
        .create_recv_transport(override_id.clone(), &router, webrtc_server, &overridden)
        .await
        .unwrap();
    {
        let participant = transport_manager
            .get_participant_lock(&override_id)
            .unwrap();
        let participant = participant.lock().await;
        let stats = participant
            .recv_transport
            .as_ref()
            .unwrap()
            .get_stats()
            .await
            .unwrap();
        assert_eq!(stats.len(), 1);
        assert_eq!(stats[0].min_outgoing_bitrate, Some(60_000));
        assert_eq!(stats[0].max_outgoing_bitrate, Some(3_000_000));
    }
    transport_manager
        .remove_participant(&override_id)
        .await
        .unwrap();

    let producer = transport_manager
        .create_producer(
            participant_id,
            send_transport.as_ref().unwrap().id,
            MediaKind::Audio,
            RtpParameters {
                mid: Some("audio".to_string()),
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
            AppData::default(),
        )
        .await
        .unwrap();

    // Exercise more retries than the per-session cap. Closing each failed or
    // retired receiver must release both native ownership and allocation load.
    // Active and paused producers require the same browser-ready acknowledgment.
    transport_manager.get_or_create_participant("other-session");
    for attempt in 0..=super::super::config::DEFAULT_MAX_CONSUMERS_PER_PARTICIPANT {
        let producer_paused = attempt > 0;
        if producer_paused {
            transport_manager
                .pause_producer(participant_id, producer.id())
                .await
                .unwrap();
        }
        let consumer = transport_manager
            .create_consumer(
                participant_id,
                producer.id(),
                RtpCapabilities {
                    codecs: config.router_config.media_codecs.clone(),
                    ..RtpCapabilities::default()
                },
                AppData::default(),
                None,
                Some(consumer_counter.clone()),
            )
            .await
            .unwrap();
        let info = crate::media::types::ConsumerInfo::from_consumer(&consumer);
        assert_eq!(consumer_counter.load(Ordering::Relaxed), 1);
        assert!(
            !transport_manager
                .close_consumer("other-session", info.id)
                .await
                .unwrap(),
            "another participant cannot close the receiver"
        );
        assert!(info.paused, "new consumers must wait for browser readiness");
        assert_eq!(info.producer_paused, producer_paused);
        assert!(consumer.dump().await.unwrap().paused);

        assert!(
            transport_manager
                .resume_consumer(participant_id, info.id)
                .await
                .unwrap(),
            "the first browser acknowledgment must resume the worker consumer"
        );
        let resumed = consumer.dump().await.unwrap();
        assert!(!resumed.paused);
        assert_eq!(resumed.producer_paused, producer_paused);
        assert!(
            !transport_manager
                .resume_consumer(participant_id, info.id)
                .await
                .unwrap(),
            "repeated acknowledgments remain idempotent"
        );
        let weak = consumer.downgrade();
        drop(consumer);
        assert!(
            transport_manager
                .close_consumer(participant_id, info.id)
                .await
                .unwrap()
        );
        assert!(
            weak.upgrade().is_none(),
            "no application consumer handle remains"
        );
        assert_eq!(consumer_counter.load(Ordering::Relaxed), 0);
        assert!(
            !transport_manager
                .close_consumer(participant_id, info.id)
                .await
                .unwrap(),
            "repeated closure must be a no-op"
        );
    }
}
#[test]
fn effective_layers_take_the_stricter_of_client_and_bandwidth_ceilings() {
    let top = ConsumerLayers {
        spatial_layer: 2,
        temporal_layer: Some(2),
    };
    assert_eq!(effective_layers(top, None, None), top);
    assert_eq!(
        effective_layers(
            top,
            Some(ConsumerLayers {
                spatial_layer: 0,
                temporal_layer: None,
            }),
            None
        ),
        ConsumerLayers {
            spatial_layer: 0,
            temporal_layer: Some(2),
        },
        "an absent client temporal layer means the top one"
    );
    assert_eq!(
        effective_layers(top, None, Some(1)),
        ConsumerLayers {
            spatial_layer: 1,
            temporal_layer: Some(2),
        }
    );
    assert_eq!(
        effective_layers(
            top,
            Some(ConsumerLayers {
                spatial_layer: 2,
                temporal_layer: Some(1),
            }),
            Some(0)
        ),
        ConsumerLayers {
            spatial_layer: 0,
            temporal_layer: Some(1),
        },
        "the stricter spatial ceiling wins and the client's temporal choice is kept"
    );
    assert_eq!(
        effective_layers(
            top,
            Some(ConsumerLayers {
                spatial_layer: 7,
                temporal_layer: Some(9),
            }),
            Some(5)
        ),
        top,
        "requests above the stream's layers clamp to the top"
    );
}

fn simulcast_video_parameters() -> RtpParameters {
    let encoding = |ssrc: u32| RtpEncodingParameters {
        ssrc: Some(ssrc),
        scalability_mode: "L1T3".parse().unwrap(),
        ..RtpEncodingParameters::default()
    };
    RtpParameters {
        mid: Some("video".to_string()),
        codecs: vec![RtpCodecParameters::Video {
            mime_type: MimeTypeVideo::Vp8,
            payload_type: 96,
            clock_rate: NonZeroU32::new(90_000).unwrap(),
            parameters: RtpCodecParametersParameters::default(),
            rtcp_feedback: vec![],
        }],
        encodings: vec![encoding(1001), encoding(1002), encoding(1003)],
        ..RtpParameters::default()
    }
}

#[tokio::test]
async fn bandwidth_ceiling_merges_with_the_client_ceiling_and_skips_audio() {
    let reservation = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let mut config = MediaConfig::default();
    config.worker_config.num_workers = 1;
    config.webrtc_server_port_base = reservation.local_addr().unwrap().port();
    drop(reservation);
    let config = Arc::new(config);
    let worker_manager = Arc::new(WorkerManager::new(config.clone()).await.unwrap());
    let router_manager = RouterManager::new(worker_manager.clone());
    let transport_manager = TransportManager::new();
    let room_id = "layers-room";
    router_manager
        .create_router(room_id.to_string(), RouterConfig::default())
        .await
        .unwrap();
    let router = router_manager.get_router(room_id).await.unwrap();
    let worker_id = router_manager.get_worker_id(room_id).await.unwrap();
    let webrtc_server = worker_manager.get_webrtc_server(worker_id).await.unwrap();
    let pid = "viewer".to_string();
    let sent = transport_manager
        .create_send_transport(
            pid.clone(),
            &router,
            webrtc_server.clone(),
            &config.webrtc_transport_config,
        )
        .await
        .unwrap();
    transport_manager
        .create_recv_transport(
            pid.clone(),
            &router,
            webrtc_server,
            &config.webrtc_transport_config,
        )
        .await
        .unwrap();
    let capabilities = RtpCapabilities {
        codecs: config.router_config.media_codecs.clone(),
        ..RtpCapabilities::default()
    };
    let audio_producer = transport_manager
        .create_producer(
            &pid,
            sent.id,
            MediaKind::Audio,
            RtpParameters {
                mid: Some("audio".to_string()),
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
            AppData::default(),
        )
        .await
        .unwrap();
    let video_producer = transport_manager
        .create_producer(
            &pid,
            sent.id,
            MediaKind::Video,
            simulcast_video_parameters(),
            AppData::default(),
        )
        .await
        .unwrap();
    let audio = transport_manager
        .create_consumer(
            &pid,
            audio_producer.id(),
            capabilities.clone(),
            AppData::default(),
            None,
            None,
        )
        .await
        .unwrap();
    let video = transport_manager
        .create_consumer(
            &pid,
            video_producer.id(),
            capabilities.clone(),
            AppData::default(),
            None,
            None,
        )
        .await
        .unwrap();
    let audio_id = audio.id();
    let video_id = video.id();
    assert_eq!(video.r#type(), ConsumerType::Simulcast);
    assert_eq!(video.priority(), 1);
    assert_eq!(audio.priority(), 1);
    let top = ConsumerLayers {
        spatial_layer: 2,
        temporal_layer: Some(2),
    };
    assert_eq!(video.preferred_layers(), Some(top));

    // Audio has no layers: neither writer may spend a worker request on it.
    assert!(
        !transport_manager
            .set_preferred_layers(
                &pid,
                audio_id,
                ConsumerLayers {
                    spatial_layer: 1,
                    temporal_layer: None,
                },
            )
            .await
            .unwrap()
    );
    assert_eq!(audio.preferred_layers(), None);
    assert_eq!(
        transport_manager
            .set_bandwidth_ceiling(&pid, 2)
            .await
            .unwrap(),
        Vec::<ConsumerId>::new(),
        "the top tier matches the worker's default ceiling: nothing to write"
    );

    // The bandwidth tier lowers the video consumer only.
    assert_eq!(
        transport_manager
            .set_bandwidth_ceiling(&pid, 1)
            .await
            .unwrap(),
        vec![video_id]
    );
    assert!(
        transport_manager
            .apply_layer_ceilings(&pid, video_id)
            .await
            .unwrap()
    );
    assert!(
        !transport_manager
            .apply_layer_ceilings(&pid, video_id)
            .await
            .unwrap(),
        "a second application is a no-op"
    );
    assert_eq!(
        video.preferred_layers(),
        Some(ConsumerLayers {
            spatial_layer: 1,
            temporal_layer: Some(2),
        })
    );

    // A stricter client ceiling stays in force when bandwidth recovers.
    assert!(
        transport_manager
            .set_preferred_layers(
                &pid,
                video_id,
                ConsumerLayers {
                    spatial_layer: 0,
                    temporal_layer: Some(1),
                },
            )
            .await
            .unwrap()
    );
    assert_eq!(
        transport_manager
            .set_bandwidth_ceiling(&pid, 2)
            .await
            .unwrap(),
        Vec::<ConsumerId>::new()
    );
    assert_eq!(
        video.preferred_layers(),
        Some(ConsumerLayers {
            spatial_layer: 0,
            temporal_layer: Some(1),
        }),
        "the bandwidth tier must not push a capped tile back to the top layer"
    );

    // Lifting the client ceiling restores the bandwidth tier, and a lower
    // tier applies to consumers created while it is in force.
    assert!(
        transport_manager
            .set_preferred_layers(
                &pid,
                video_id,
                ConsumerLayers {
                    spatial_layer: 2,
                    temporal_layer: None,
                },
            )
            .await
            .unwrap()
    );
    assert_eq!(video.preferred_layers(), Some(top));
    assert_eq!(
        transport_manager
            .set_bandwidth_ceiling(&pid, 0)
            .await
            .unwrap(),
        vec![video_id]
    );
    assert!(
        transport_manager
            .apply_layer_ceilings(&pid, video_id)
            .await
            .unwrap()
    );
    let late = transport_manager
        .create_consumer(
            &pid,
            video_producer.id(),
            capabilities,
            AppData::new(ConsumerMediaSource::ScreenVideo),
            None,
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        late.priority(),
        4,
        "canonical screen source receives video bandwidth priority"
    );
    assert_eq!(
        late.preferred_layers(),
        Some(ConsumerLayers {
            spatial_layer: 0,
            temporal_layer: Some(2),
        }),
        "a consumer created under a low tier starts capped without an extra request"
    );
    drop(video);
    assert!(
        transport_manager
            .close_consumer(&pid, video_id)
            .await
            .unwrap()
    );
    let participant = transport_manager.get_participant(&pid).await.unwrap();
    assert!(!participant.consumer_layers.contains_key(&video_id));
    assert!(participant.consumers.contains_key(&late.id()));
}
/// A loopback port free for both UDP and TCP, so a TCP-enabled WebRtcServer
/// can bind both in tests.
fn reserve_udp_and_tcp_port() -> u16 {
    for _ in 0..50 {
        let udp = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let port = udp.local_addr().unwrap().port();
        if std::net::TcpListener::bind(("0.0.0.0", port)).is_ok() {
            drop(udp);
            return port;
        }
    }
    panic!("no port free for both UDP and TCP");
}

#[tokio::test]
async fn tcp_listener_adds_tcp_ice_candidates_and_is_off_by_default() {
    for tcp in [false, true] {
        let mut config = MediaConfig::default();
        config.worker_config.num_workers = 1;
        config.webrtc_server_port_base = reserve_udp_and_tcp_port();
        assert!(!config.webrtc_transport_config.enable_tcp, "TCP is opt-in");
        config.set_tcp(tcp);
        assert_eq!(config.webrtc_server_tcp, tcp);
        assert_eq!(
            config.webrtc_transport_config.enable_tcp, tcp,
            "the transport flag must follow the listener, never claim TCP without one"
        );
        let config = Arc::new(config);
        let worker_manager = Arc::new(WorkerManager::new(config.clone()).await.unwrap());
        let router_manager = RouterManager::new(worker_manager.clone());
        let transport_manager = TransportManager::new();
        let room_id = format!("tcp-room-{tcp}");
        router_manager
            .create_router(room_id.to_string(), RouterConfig::default())
            .await
            .unwrap();
        let router = router_manager.get_router(&room_id).await.unwrap();
        let worker_id = router_manager.get_worker_id(&room_id).await.unwrap();
        let webrtc_server = worker_manager.get_webrtc_server(worker_id).await.unwrap();
        let info = transport_manager
            .create_send_transport(
                "candidate-check".to_string(),
                &router,
                webrtc_server,
                &config.webrtc_transport_config,
            )
            .await
            .unwrap();
        let protocols: Vec<Protocol> = info
            .ice_candidates
            .iter()
            .map(|candidate| candidate.protocol)
            .collect();
        assert!(protocols.contains(&Protocol::Udp), "{protocols:?}");
        assert_eq!(protocols.contains(&Protocol::Tcp), tcp, "{protocols:?}");
    }
}

/// The tracing macros an operational log is written with, every level: a room
/// must stay out of the debug output an operator turns on to diagnose too.
const LOG_MACROS: [&str; 5] = ["trace", "debug", "info", "warn", "error"];

/// Whether the arguments of a log invocation or of an error construction name
/// a room or a media namespace. The scans recognize these by name, so the rule
/// is only as good as the names: a word that is `rid`, ends in `room_id`,
/// `room_ids`, `media_id` or `media_ids`, or starts with `room_id_` or
/// `media_id_` counts as a room identifier, `media_participant_id` as the
/// namespace `RoomManager::media_participant_id` builds, and, in the media
/// layer, whose `participant_id` parameters hold that namespace,
/// `participant_id` too. Such a word may reach a log only reduced (see
/// `reduced`), a format string may not capture one inline, and a structured
/// field named `room` must hold a log handle. A room identifier or namespace
/// held under any other name escapes the scans, so the bindings they read carry
/// these names. A field named `participant_id` is not itself a value: the
/// lifecycle markers keep that name for the public UUID, and whatever value
/// follows the `=` is checked on its own.
fn names_a_room(arguments: &[Token<'_>], media: bool) -> bool {
    arguments
        .iter()
        .enumerate()
        .any(|(at, token)| match *token {
            Token::Word("media_participant_id") => true,
            Token::Word("room") if field_name(arguments, at) => {
                !reduced_value(&arguments[at + 2..])
            }
            Token::Word(word) if names_an_identifier(word, media) => {
                !reduced(arguments, at) && !(word == "participant_id" && field_name(arguments, at))
            }
            Token::Text(text) => inline_captures(text)
                .into_iter()
                .any(|name| names_an_identifier(name, media)),
            _ => false,
        })
}

/// Whether a word names a room identifier or a media namespace under the scans'
/// naming rule (see `names_a_room`).
fn names_an_identifier(word: &str, media: bool) -> bool {
    word == "rid"
        || ["room_id", "room_ids", "media_id", "media_ids"]
            .into_iter()
            .any(|suffix| word.ends_with(suffix))
        || ["room_id_", "media_id_"]
            .into_iter()
            .any(|prefix| word.starts_with(prefix))
        || word == "media_participant_id"
        || (media && word == "participant_id")
}

/// The names a format string captures inline, `{name}` or `{name:…}`, escaped
/// braces aside.
fn inline_captures(text: &str) -> Vec<&str> {
    let mut names = Vec::new();
    let mut rest = text;
    while let Some(open) = rest.find('{') {
        let after = &rest[open + 1..];
        if let Some(escaped) = after.strip_prefix('{') {
            rest = escaped;
            continue;
        }
        let length = after
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .unwrap_or(after.len());
        if length > 0 && after[length..].starts_with(['}', ':']) {
            names.push(&after[..length]);
        }
        rest = after;
    }
    names
}

/// Whether a structured field's value, the tokens after its `=`, is a log
/// handle: `<expression>.log_handle()` or `room_log_handle(…)`, through `%` or
/// `?` or neither.
fn reduced_value(after: &[Token<'_>]) -> bool {
    use Token::{Mark, Word};
    let value = after
        .strip_prefix(&[Mark('%')])
        .or_else(|| after.strip_prefix(&[Mark('?')]))
        .unwrap_or(after);
    let mut depth = 0_usize;
    let end = value
        .iter()
        .position(|token| {
            match *token {
                Mark('(' | '[' | '{') => depth += 1,
                Mark(')' | ']' | '}') => depth = depth.saturating_sub(1),
                Mark(',') if depth == 0 => return true,
                _ => {}
            }
            false
        })
        .unwrap_or(value.len());
    let value = &value[..end];
    value.ends_with(&[Mark('.'), Word("log_handle"), Mark('('), Mark(')')])
        || (value.starts_with(&[Word("room_log_handle"), Mark('(')])
            && value.last() == Some(&Mark(')')))
}

/// Whether the identifier at `at` reaches the log only through the reduction
/// that hides it: as the receiver of `.log_handle()`, or as the one argument,
/// borrowed or not and with or without rustfmt's trailing comma, of
/// `room_log_handle` or `logged_participant`.
fn reduced(arguments: &[Token<'_>], at: usize) -> bool {
    use Token::{Mark, Word};
    let after = &arguments[at + 1..];
    let before = &arguments[..at];
    let before = before.strip_suffix(&[Mark('&')]).unwrap_or(before);
    let closing = after.strip_prefix(&[Mark(',')]).unwrap_or(after);
    after.starts_with(&[Mark('.'), Word("log_handle"), Mark('('), Mark(')')])
        || (closing.first() == Some(&Mark(')'))
            && ["room_log_handle", "logged_participant"]
                .into_iter()
                .any(|reducer| before.ends_with(&[Word(reducer), Mark('(')])))
}

/// Whether the identifier at `at` names a structured field (`name = value`).
fn field_name(arguments: &[Token<'_>], at: usize) -> bool {
    arguments.get(at + 1) == Some(&Token::Mark('='))
        && arguments.get(at + 2) != Some(&Token::Mark('='))
}

#[test]
fn the_log_scan_flags_each_way_of_naming_a_room() {
    // (invocation, in the media layer, names a room or a namespace)
    let cases = [
        (r#"warn!(%room_id, %error, "Failed")"#, false, true),
        (r#"warn!(room_id = %req.id, "Failed")"#, false, true),
        (r#"warn!(room_id = %self.room_id, "Failed")"#, false, true),
        (
            r#"info!("Participant {} joined room {}", participant_id, room_id)"#,
            false,
            true,
        ),
        (
            r#"info!("Participant {participant_id} joined room {room_id}")"#,
            false,
            true,
        ),
        (r#"info!("Removed {}", media_participant_id)"#, false, true),
        (r#"info!("Removed {media_participant_id}")"#, false, true),
        (
            r#"info!("Missing {}", participant_id.to_string())"#,
            true,
            true,
        ),
        (
            r#"info!("Created transport {} for {}", id, participant_id)"#,
            true,
            true,
        ),
        (
            r#"info!(participant_id = %participant_id, "Created")"#,
            true,
            true,
        ),
        (
            r#"info!("Created transport for {participant_id}")"#,
            true,
            true,
        ),
        (
            r#"warn!(room = %room_id.log_handle(), %error, "Failed")"#,
            false,
            false,
        ),
        (
            r#"warn!(room = %self.room_id.log_handle(), "Failed")"#,
            false,
            false,
        ),
        (
            r#"info!("Participant {} joined room {}", participant_id, room_id.log_handle())"#,
            false,
            false,
        ),
        (
            r#"info!("Created router for room {}", room_log_handle(&room_id))"#,
            true,
            false,
        ),
        (
            r#"info!("Created transport for {}", logged_participant(&participant_id))"#,
            true,
            false,
        ),
        (
            r#"info!("Missing {}", logged_participant(participant_id,))"#,
            true,
            false,
        ),
        (
            r#"info!("Missing {}", logged_participant(participant_id, room_id))"#,
            true,
            true,
        ),
        (r#"trace!("Room {} drained", room_id)"#, false, true),
        (r#"debug!(%room_id, %participant_id, "Gone")"#, false, true),
        (
            r#"debug!(target: "simplestchat::lifecycle", participant_id = %lifecycle_id, "lifecycle")"#,
            true,
            false,
        ),
        (
            r#"debug!(participant_id = participant_id.as_str(), "Gone")"#,
            true,
            true,
        ),
        (r#"info!("Expired in room {}", rid)"#, false, true),
        (r#"warn!(%rid, "Expired")"#, false, true),
        (
            r#"warn!(room = %id, "Stored image is not raster")"#,
            false,
            true,
        ),
        (r#"warn!("Failed for {target_room_id}")"#, false, true),
        (r#"warn!("Failed for {target_room_id:?}")"#, false, true),
        (r#"warn!("Literal {{room_id}} braces")"#, false, false),
        (r#"warn!("Removed {}", target_media_id)"#, false, true),
        (
            r#"warn!(room = %room_log_handle(&req.id), %error, "Failed")"#,
            false,
            false,
        ),
        (r#"warn!(room = %id.log_handle(), "Stored")"#, false, false),
        (
            r#"info!("Expired in room {}", timer_room_id.log_handle())"#,
            false,
            false,
        ),
        (r#"warn!("Closing {room_ids:?}")"#, false, true),
        (r#"info!("Saved {}", room_id_text)"#, false, true),
        (
            r#"info!("Updated {}", update_room_identity(&room))"#,
            false,
            false,
        ),
    ];
    for (source, media, names) in cases {
        let invocations = macro_invocations(source, &LOG_MACROS).unwrap();
        assert_eq!(invocations.len(), 1, "{source}");
        assert_eq!(
            names_a_room(&invocations[0].arguments, media),
            names,
            "{source}"
        );
    }
}

/// Room identifiers are names their owners chose, and a media namespace holds
/// one beside the media session, so operational logs in the room, media and
/// signaling layers, subdirectories included, name a room only by its log
/// handle and a participant only by its public UUID, at every level. The scan
/// recognizes identifiers by name (see `names_a_room`).
#[test]
fn operational_logs_never_name_rooms() {
    let mut offenders = Vec::new();
    for directory in ["room", "media", "signaling"] {
        for (name, production) in production_sources(directory) {
            let invocations = macro_invocations(&production, &LOG_MACROS)
                .unwrap_or_else(|| panic!("{name}: a log invocation is never closed"));
            for invocation in invocations {
                if names_a_room(&invocation.arguments, name.starts_with("media/")) {
                    offenders.push(format!("{name}:{}", invocation.line));
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "{} operational logs name a room or a media namespace:\n{}",
        offenders.len(),
        offenders.join("\n")
    );
}

/// A media error's text reaches operational logs through the `%error` of the
/// room layer's warnings, so it names a room and a participant the way those
/// logs do: by the room's log handle and the participant's public UUID.
#[test]
fn media_errors_never_name_rooms() {
    let mut offenders = Vec::new();
    for (name, production) in production_sources("media") {
        let constructions = variant_constructions(&production, "MediaError")
            .unwrap_or_else(|| panic!("{name}: a MediaError construction is never closed"));
        for construction in constructions {
            if names_a_room(&construction.arguments, true) {
                offenders.push(format!("{name}:{}", construction.line));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "{} media errors name a room or a media namespace:\n{}",
        offenders.len(),
        offenders.join("\n")
    );
}

/// The social error helpers (social.rs) that build a refusal from a message.
const SOCIAL_ERROR_HELPERS: [&str; 8] = [
    "refusal",
    "invalid",
    "forbidden",
    "not_found",
    "rate_limited",
    "unavailable",
    "timed_out",
    "failed",
];

/// The message of an `ensure!`: the tokens after its condition, which ends at
/// the first top-level comma; the condition may name a room to look it up.
fn ensure_message<'a>(arguments: &[Token<'a>]) -> Vec<Token<'a>> {
    let mut depth = 0_usize;
    for (at, token) in arguments.iter().enumerate() {
        match *token {
            Token::Mark('(' | '[' | '{') => depth += 1,
            Token::Mark(')' | ']' | '}') => depth = depth.saturating_sub(1),
            Token::Mark(',') if depth == 0 => return arguments[at + 1..].to_vec(),
            _ => {}
        }
    }
    Vec::new()
}

#[test]
fn the_error_scan_reads_only_the_message_of_an_ensure() {
    // (ensure! invocation, its message names a room)
    let checks = [
        (
            r#"ensure!(rooms.get(room_id).is_some_and(|current| same(current, &room)), "Room is no longer active")"#,
            false,
        ),
        (r#"ensure!(ready, "Room {} is gone", room_id)"#, true),
        (r#"ensure!(ready, "Room {room_id} is gone")"#, true),
        (
            r#"ensure!(ready, "Room {} is gone", room_id.log_handle())"#,
            false,
        ),
    ];
    for (source, names) in checks {
        let found = macro_invocations(source, &["ensure"]).unwrap();
        assert_eq!(
            names_a_room(&ensure_message(&found[0].arguments), false),
            names,
            "{source}"
        );
    }
}

/// The constructions a scan found in the file `name`, failing the scan when
/// one never closes.
fn closed<'a>(name: &str, found: Option<Vec<Invocation<'a>>>) -> Vec<Invocation<'a>> {
    found.unwrap_or_else(|| panic!("{name}: an error construction is never closed"))
}

/// A room-layer error's text reaches operational logs the same way, through the
/// `%error` of a warning or a media error that wraps it (`media_ops` turns a
/// room lookup's failure into a `MediaError::ConsumerError`), so the room
/// layer, subdirectories included, names a room only by its log handle in its
/// `anyhow!` and `bail!` errors, the message of its `ensure!` checks, the
/// arguments of `.context(…)` and `.with_context(…)`, the social error helpers
/// and its `Refusal`, `RoomError` and `MediaError` constructions.
#[test]
fn room_errors_never_name_rooms() {
    let mut offenders = Vec::new();
    for (name, production) in production_sources("room") {
        let mut constructions = closed(&name, macro_invocations(&production, &["anyhow", "bail"]));
        constructions.extend(
            closed(&name, macro_invocations(&production, &["ensure"]))
                .into_iter()
                .map(|check| Invocation {
                    line: check.line,
                    arguments: ensure_message(&check.arguments),
                }),
        );
        constructions.extend(closed(
            &name,
            calls(&production, &["context", "with_context"]),
        ));
        constructions.extend(closed(&name, calls(&production, &SOCIAL_ERROR_HELPERS)));
        for type_name in ["Refusal", "RoomError", "MediaError"] {
            constructions.extend(closed(&name, variant_constructions(&production, type_name)));
        }
        let mut lines: Vec<usize> = constructions
            .iter()
            .filter(|construction| names_a_room(&construction.arguments, false))
            .map(|construction| construction.line)
            .collect();
        lines.sort_unstable();
        lines.dedup();
        offenders.extend(lines.into_iter().map(|line| format!("{name}:{line}")));
    }
    assert!(
        offenders.is_empty(),
        "{} room errors name a room or a media namespace:\n{}",
        offenders.len(),
        offenders.join("\n")
    );
}
