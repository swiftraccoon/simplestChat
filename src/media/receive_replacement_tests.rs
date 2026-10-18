//! Actual worker regressions, executed by hosted Rust tests only.

use super::*;
use crate::media::{MediaConfig, RouterConfig, RouterManager, WorkerManager};
use std::num::{NonZeroU8, NonZeroU32};

struct Fixture {
    manager: TransportManager,
    workers: Arc<WorkerManager>,
    router: Router,
    server: WebRtcServer,
    config: WebRtcTransportConfig,
}

impl Fixture {
    async fn new() -> Self {
        let reservation = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let mut config = MediaConfig::default();
        config.worker_config.num_workers = 1;
        config.webrtc_server_port_base = reservation.local_addr().unwrap().port();
        drop(reservation);
        let config = Arc::new(config);
        let workers = Arc::new(WorkerManager::new(config.clone()).await.unwrap());
        let routers = RouterManager::new(workers.clone());
        routers
            .create_router("replace-receiver".into(), config.router_config.clone())
            .await
            .unwrap();
        let router = routers.get_router("replace-receiver").await.unwrap();
        let worker = routers.get_worker_id("replace-receiver").await.unwrap();
        let server = workers.get_webrtc_server(worker).await.unwrap();
        Self {
            manager: TransportManager::new(),
            workers,
            router,
            server,
            config: config.webrtc_transport_config.clone(),
        }
    }

    fn placement(&self) -> ReceiveReplacementPlacement {
        ReceiveReplacementPlacement {
            router: self.router.clone(),
            webrtc_server: self.server.clone(),
            lease: None,
            retain_existing_lease: false,
        }
    }

    async fn member(&self, id: &str) -> (TransportInfo, TransportInfo) {
        let sent = self
            .manager
            .create_send_transport(id.into(), &self.router, self.server.clone(), &self.config)
            .await
            .unwrap();
        let received = self
            .manager
            .create_recv_transport(id.into(), &self.router, self.server.clone(), &self.config)
            .await
            .unwrap();
        (sent, received)
    }

    async fn finish(self) {
        let ids = self
            .manager
            .participants
            .read()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        for id in ids {
            self.manager.remove_participant(&id).await.unwrap();
        }
        drop(self.router);
        drop(self.server);
        self.workers.shutdown().await.unwrap();
    }
}

fn microphone() -> RtpParameters {
    RtpParameters {
        codecs: vec![RtpCodecParameters::Audio {
            mime_type: MimeTypeAudio::Opus,
            payload_type: 111,
            clock_rate: NonZeroU32::new(48_000).unwrap(),
            channels: NonZeroU8::new(2).unwrap(),
            parameters: RtpCodecParametersParameters::default(),
            rtcp_feedback: vec![],
        }],
        encodings: vec![RtpEncodingParameters {
            ssrc: Some(4231),
            ..Default::default()
        }],
        ..Default::default()
    }
}

fn camera() -> RtpParameters {
    RtpParameters {
        mid: Some("video".into()),
        codecs: vec![RtpCodecParameters::Video {
            mime_type: MimeTypeVideo::Vp8,
            payload_type: 96,
            clock_rate: NonZeroU32::new(90_000).unwrap(),
            parameters: RtpCodecParametersParameters::default(),
            rtcp_feedback: vec![],
        }],
        encodings: [5141, 5142, 5143]
            .into_iter()
            .map(|ssrc| RtpEncodingParameters {
                ssrc: Some(ssrc),
                scalability_mode: "L1T3".parse().unwrap(),
                ..Default::default()
            })
            .collect(),
        ..Default::default()
    }
}

#[tokio::test]
async fn receive_replacement_preserves_sender_and_producer_retires_consumers_and_replays_once() {
    let f = Fixture::new().await;
    let (sent, received) = f.member("publisher-viewer").await;
    let producer = f
        .manager
        .create_producer(
            "publisher-viewer",
            MediaKind::Audio,
            microphone(),
            AppData::default(),
        )
        .await
        .unwrap();
    let camera = f
        .manager
        .create_producer(
            "publisher-viewer",
            MediaKind::Video,
            camera(),
            AppData::default(),
        )
        .await
        .unwrap();
    let video_consumer = f
        .manager
        .create_consumer(
            "publisher-viewer",
            camera.id(),
            RtpCapabilities {
                codecs: RouterConfig::default().media_codecs,
                ..Default::default()
            },
            AppData::default(),
            None,
            None,
        )
        .await
        .unwrap();
    let video_weak = video_consumer.downgrade();
    drop(video_consumer);
    let consumer = f
        .manager
        .create_consumer(
            "publisher-viewer",
            producer.id(),
            RtpCapabilities {
                codecs: RouterConfig::default().media_codecs,
                ..Default::default()
            },
            AppData::default(),
            None,
            None,
        )
        .await
        .unwrap();
    let consumer_weak = consumer.downgrade();
    drop(consumer);
    let old_weak = {
        let p = f.manager.get_participant_lock("publisher-viewer").unwrap();
        p.lock().await.recv_transport.as_ref().unwrap().downgrade()
    };
    let (bwe, _events) = mpsc::channel(32);
    f.manager
        .subscribe_bwe_events("publisher-viewer", bwe.clone())
        .await
        .unwrap();
    f.manager
        .set_bandwidth_ceiling("publisher-viewer", 0)
        .await
        .unwrap();
    assert_eq!(
        f.manager
            .get_participant_lock("publisher-viewer")
            .unwrap()
            .lock()
            .await
            .consumer_layers
            .len(),
        1
    );
    let old_id = received.id.parse().unwrap();
    let replacement = f
        .manager
        .replace_recv_transport(
            "publisher-viewer",
            old_id,
            f.placement(),
            &f.config,
            Some(bwe),
        )
        .await
        .unwrap();
    assert_ne!(replacement.id, received.id);
    assert!(
        old_weak.upgrade().is_none(),
        "the retired receiver has no retained application handle"
    );
    assert!(consumer_weak.upgrade().is_none());
    assert!(video_weak.upgrade().is_none());
    assert!(!producer.closed(), "outgoing voice remains published");
    {
        let p = f.manager.get_participant_lock("publisher-viewer").unwrap();
        let p = p.lock().await;
        assert_eq!(p.send_transport.as_ref().unwrap().id().to_string(), sent.id);
        assert!(!p.send_transport.as_ref().unwrap().closed());
        assert_eq!(p.producers.len(), 2);
        assert!(p.consumers.is_empty());
        assert!(p.consumer_layers.is_empty());
        assert_eq!(p.bandwidth_spatial_ceiling, None);
        assert_eq!(p.recv_connect_applied, None);
        let stats = p
            .recv_transport
            .as_ref()
            .unwrap()
            .get_stats()
            .await
            .unwrap();
        assert_eq!(stats[0].min_outgoing_bitrate, Some(100_000));
        assert_eq!(stats[0].max_outgoing_bitrate, Some(3_000_000));
    }
    assert_eq!(f.manager.bwe_trace_handlers.read().unwrap().len(), 1);
    let replay = f
        .manager
        .replace_recv_transport("publisher-viewer", old_id, f.placement(), &f.config, None)
        .await
        .unwrap();
    assert_eq!(
        replacement.id, replay.id,
        "a lost response cannot allocate another receiver"
    );
    assert!(
        !f.manager
            .is_current_recv_transport("publisher-viewer", old_id)
            .await
    );
    assert!(
        f.manager
            .set_bandwidth_ceiling_for_transport("publisher-viewer", Some(old_id), 0)
            .await
            .is_err(),
        "a queued old BWE event must not cap the new receiver"
    );
    assert_eq!(
        f.manager
            .get_participant_lock("publisher-viewer")
            .unwrap()
            .lock()
            .await
            .bandwidth_spatial_ceiling,
        None
    );
    let fresh = f
        .manager
        .create_consumer(
            "publisher-viewer",
            camera.id(),
            RtpCapabilities {
                codecs: RouterConfig::default().media_codecs,
                ..Default::default()
            },
            AppData::default(),
            None,
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        fresh.preferred_layers().unwrap().spatial_layer,
        2,
        "a replacement does not inherit the retired receiver's low bandwidth tier"
    );
    drop(fresh);
    let next = f
        .manager
        .replace_recv_transport(
            "publisher-viewer",
            replacement.id.parse().unwrap(),
            f.placement(),
            &f.config,
            None,
        )
        .await
        .unwrap();
    assert!(
        f.manager
            .replace_recv_transport("publisher-viewer", old_id, f.placement(), &f.config, None)
            .await
            .is_err()
    );
    assert!(
        f.manager
            .is_current_recv_transport("publisher-viewer", next.id.parse().unwrap())
            .await
    );
    assert!(!producer.closed());
    assert!(!camera.closed());
    drop(camera);
    drop(producer);
    f.finish().await;
}

#[tokio::test]
async fn receive_replacement_rejects_wrong_resource_and_failed_candidate_without_retiring_old() {
    let f = Fixture::new().await;
    let (sent, received) = f.member("one").await;
    let (_, other) = f.member("two").await;
    for wrong in [
        sent.id.parse().unwrap(),
        other.id.parse().unwrap(),
        Uuid::new_v4().to_string().parse().unwrap(),
    ] {
        assert!(
            f.manager
                .replace_recv_transport("one", wrong, f.placement(), &f.config, None)
                .await
                .is_err()
        );
    }
    let mut invalid = f.config.clone();
    invalid.min_outgoing_bitrate = 4_000_000;
    assert!(
        f.manager
            .replace_recv_transport(
                "one",
                received.id.parse().unwrap(),
                f.placement(),
                &invalid,
                None
            )
            .await
            .is_err()
    );
    assert!(
        f.manager
            .is_current_recv_transport("one", received.id.parse().unwrap())
            .await
    );
    assert!(
        f.manager
            .is_current_recv_transport("two", other.id.parse().unwrap())
            .await
    );
    assert!(
        f.manager
            .recv_replacement_replay("one", received.id.parse().unwrap())
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        f.manager
            .get_participant("one")
            .await
            .unwrap()
            .producers
            .is_empty()
    );
    f.finish().await;
}
