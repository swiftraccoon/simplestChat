#![forbid(unsafe_code)]

//! Viewer placement across workers with router-to-router pipes, on bounded,
//! disposable local workers. Direct transports keep every packet inside the
//! owned workers; browsers, sockets and databases are not involved.

use super::*;
use crate::media::config::MediaConfig;
use std::future::Future;
use std::net::{Ipv4Addr, UdpSocket};
use std::num::NonZeroU32;
use std::sync::atomic::Ordering;
use std::time::Duration;

const FIXTURE_TIMEOUT: Duration = Duration::from_secs(30);
const PORT_RESERVATION_ATTEMPTS: usize = 64;
const ROOM: &str = "webinar";

fn reserve_worker_ports(worker_count: usize) -> (u16, Vec<UdpSocket>) {
    'candidate: for _ in 0..PORT_RESERVATION_ATTEMPTS {
        let first = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let base = first.local_addr().unwrap().port();
        let mut reservations = vec![first];
        for offset in 1..worker_count {
            let Some(port) = base.checked_add(u16::try_from(offset).unwrap()) else {
                continue 'candidate;
            };
            match UdpSocket::bind((Ipv4Addr::LOCALHOST, port)) {
                Ok(socket) => reservations.push(socket),
                Err(_) => continue 'candidate,
            }
        }
        return (base, reservations);
    }
    panic!("Could not reserve {worker_count} contiguous local UDP ports");
}

/// Two workers, one room whose primary router the harness creates, and the
/// same end-of-test accounting check as the allocation fixtures.
async fn with_room<F, Fut>(test: F)
where
    F: FnOnce(Arc<WorkerManager>, Arc<RouterManager>, Arc<MediaConfig>, WorkerId) -> Fut,
    Fut: Future<Output = ()>,
{
    tokio::time::timeout(FIXTURE_TIMEOUT, async move {
        let (port_base, reservations) = reserve_worker_ports(2);
        let mut config = MediaConfig::default();
        config.worker_config.num_workers = 2;
        config.webrtc_server_port_base = port_base;
        let config = Arc::new(config);
        drop(reservations);

        let workers = Arc::new(WorkerManager::new(config.clone()).await.unwrap());
        let routers = Arc::new(RouterManager::new(workers.clone()));
        routers
            .create_router(ROOM.to_string(), config.router_config.clone())
            .await
            .unwrap();
        let primary = routers.get_worker_id(ROOM).await.unwrap();
        test(workers.clone(), routers.clone(), config, primary).await;

        routers.close_all().await.unwrap();
        let remaining = workers.get_load_distribution().await;
        assert!(
            remaining.values().all(|count| *count == 0),
            "closing every router must refund the viewer routers' reservations too"
        );
        workers.shutdown().await.unwrap();
    })
    .await
    .expect("native placement fixture exceeded its deadline");
}

fn busy(workers: &WorkerManager, worker: WorkerId) {
    workers
        .get_consumer_counter(worker)
        .unwrap()
        .fetch_add(VIEWER_SPREAD_MIN_CONSUMERS, Ordering::Relaxed);
}

fn video_parameters() -> RtpParameters {
    let clock_rate = NonZeroU32::new(90_000).unwrap();
    RtpParameters {
        mid: Some("0".to_owned()),
        codecs: vec![RtpCodecParameters::Video {
            mime_type: MimeTypeVideo::Vp8,
            payload_type: 96,
            clock_rate,
            parameters: RtpCodecParametersParameters::default(),
            rtcp_feedback: vec![RtcpFeedback::Nack, RtcpFeedback::NackPli],
        }],
        encodings: vec![RtpEncodingParameters {
            ssrc: Some(123_456),
            ..RtpEncodingParameters::default()
        }],
        header_extensions: vec![RtpHeaderExtensionParameters {
            uri: RtpHeaderExtensionUri::Mid,
            id: 3,
            encrypt: false,
        }],
        ..RtpParameters::default()
    }
}

#[tokio::test]
async fn viewers_stay_on_the_primary_router_while_its_worker_is_lightly_loaded() {
    with_room(|_workers, routers, config, primary| async move {
        let primary_router = routers.get_router(ROOM).await.unwrap();
        let (router, worker, lease) = routers
            .place_viewer(ROOM, "viewer-1", &config.router_config)
            .await
            .unwrap();
        assert_eq!(router.id(), primary_router.id());
        assert_eq!(worker, primary);
        assert!(lease.is_none(), "a primary placement needs no lease");
        assert_eq!(routers.viewer_router_count(ROOM).await, 0);
        assert_eq!(routers.worker_for_participant("viewer-1"), None);
    })
    .await;
}

#[tokio::test]
async fn viewers_spread_to_another_worker_once_the_primary_worker_is_busy() {
    with_room(|workers, routers, config, primary| async move {
        busy(&workers, primary);
        let (first, worker, lease_1) = routers
            .place_viewer(ROOM, "viewer-1", &config.router_config)
            .await
            .unwrap();
        assert_ne!(worker, primary);
        assert!(lease_1.is_some());
        let (second, second_worker, lease_2) = routers
            .place_viewer(ROOM, "viewer-2", &config.router_config)
            .await
            .unwrap();
        assert_eq!(
            second_worker, worker,
            "the room reuses one viewer router per worker"
        );
        assert_eq!(second.id(), first.id());
        assert_eq!(routers.viewer_router_count(ROOM).await, 1);
        assert_eq!(routers.worker_for_participant("viewer-1"), Some(worker));
        assert_eq!(
            first.rtp_capabilities(),
            routers.get_router(ROOM).await.unwrap().rtp_capabilities(),
            "clients keep negotiating against the primary router's capabilities"
        );

        // A repeated placement for a placed participant is stable.
        let (again, again_worker, again_lease) = routers
            .place_viewer(ROOM, "viewer-1", &config.router_config)
            .await
            .unwrap();
        assert_eq!(again.id(), first.id());
        assert_eq!(again_worker, worker);
        assert!(
            again_lease.is_none(),
            "the existing lease keeps the placement"
        );

        drop(lease_1);
        assert_eq!(routers.worker_for_participant("viewer-1"), None);
        drop(lease_2);
        assert_eq!(routers.worker_for_participant("viewer-2"), None);
        assert_eq!(
            routers.viewer_router_count(ROOM).await,
            1,
            "viewer routers live with the room, not with individual viewers"
        );
    })
    .await;
}

#[tokio::test]
async fn a_producer_is_piped_once_per_viewer_router_and_consumed_by_its_kept_id() {
    with_room(|workers, routers, config, primary| async move {
        let primary_router = routers.get_router(ROOM).await.unwrap();
        let publisher = primary_router
            .create_direct_transport(DirectTransportOptions::default())
            .await
            .unwrap();
        let producer = publisher
            .produce(ProducerOptions::new(MediaKind::Video, video_parameters()))
            .await
            .unwrap();
        let producer_id = producer.id();

        busy(&workers, primary);
        let (viewer_router, _worker, _lease) = routers
            .place_viewer(ROOM, "viewer-1", &config.router_config)
            .await
            .unwrap();
        routers
            .ensure_piped(ROOM, "viewer-1", producer_id)
            .await
            .unwrap();
        routers
            .ensure_piped(ROOM, "viewer-1", producer_id)
            .await
            .unwrap();
        assert_eq!(routers.pipe_count(ROOM).await, 1);

        let viewer = viewer_router
            .create_direct_transport(DirectTransportOptions::default())
            .await
            .unwrap();
        let consumer = viewer
            .consume(ConsumerOptions::new(
                producer_id,
                RtpCapabilities {
                    codecs: config.router_config.media_codecs.clone(),
                    ..RtpCapabilities::default()
                },
            ))
            .await
            .unwrap();
        assert_eq!(consumer.producer_id(), producer_id);

        // The pipe follows the producer: the viewer's consumer closes with it,
        // and a later request cannot revive the pipe.
        drop(producer);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while !consumer.closed() {
            assert!(
                tokio::time::Instant::now() < deadline,
                "pipe did not follow the producer close"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(
            routers
                .ensure_piped(ROOM, "viewer-1", producer_id)
                .await
                .is_err()
        );
        assert_eq!(routers.pipe_count(ROOM).await, 0);
    })
    .await;
}

#[tokio::test]
async fn removing_the_room_router_closes_its_viewer_routers_too() {
    with_room(|workers, routers, config, primary| async move {
        busy(&workers, primary);
        let (viewer_router, _worker, _lease) = routers
            .place_viewer(ROOM, "viewer-1", &config.router_config)
            .await
            .unwrap();
        assert_eq!(routers.viewer_router_count(ROOM).await, 1);
        routers.remove_router(ROOM).await.unwrap();
        assert_eq!(routers.viewer_router_count(ROOM).await, 0);
        assert_eq!(routers.pipe_count(ROOM).await, 0);
        // The manager dropped its handles and refunded every reservation; this
        // clone is the only thing keeping the native viewer router alive.
        let remaining = workers.get_load_distribution().await;
        assert!(remaining.values().all(|count| *count == 0));
        assert!(!viewer_router.closed());
        drop(viewer_router);
    })
    .await;
}

#[tokio::test]
async fn a_dead_worker_loses_only_the_viewer_routers_it_hosted() {
    with_room(|workers, routers, config, primary| async move {
        busy(&workers, primary);
        let (_router, worker, _lease) = routers
            .place_viewer(ROOM, "viewer-1", &config.router_config)
            .await
            .unwrap();
        assert_eq!(routers.drop_viewer_routers_on_worker(worker).await, 1);
        assert_eq!(routers.viewer_router_count(ROOM).await, 0);
        assert!(
            routers.has_router(ROOM).await,
            "the primary router is untouched"
        );
        // A stale placement never hands out the dropped router.
        let (again, again_worker, _again_lease) = routers
            .place_viewer(ROOM, "viewer-1", &config.router_config)
            .await
            .unwrap();
        assert!(!again.closed());
        assert!(again_worker == primary || routers.viewer_router_count(ROOM).await == 1);
    })
    .await;
}
