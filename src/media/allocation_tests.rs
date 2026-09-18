#![forbid(unsafe_code)]

//! Native worker-allocation regressions using bounded, disposable local fixtures.

use super::*;
use crate::media::config::MediaConfig;
use std::future::Future;
use std::net::{Ipv4Addr, UdpSocket};
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::sync::{Barrier, Semaphore};
use tokio::task::JoinSet;

const FIXTURE_TIMEOUT: Duration = Duration::from_secs(30);
const PORT_RESERVATION_ATTEMPTS: usize = 64;
const DISTRIBUTION_ROOMS: usize = 8;

/// Hold the entire span until immediately before native listeners start. A free
/// ephemeral base port alone says nothing about the availability of its neighbors.
fn reserve_worker_ports(worker_count: usize) -> (u16, Vec<UdpSocket>) {
    assert!((1..=4).contains(&worker_count));

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

/// Bound initialization, assertions, and shutdown together. Keeping a worker
/// manager outside the test body also checks that dropping the router manager
/// releases its reservations without requiring worker shutdown to clear them.
async fn with_workers<F, Fut>(worker_count: usize, test: F)
where
    F: FnOnce(Arc<WorkerManager>, RouterManager, Arc<MediaConfig>) -> Fut,
    Fut: Future<Output = ()>,
{
    tokio::time::timeout(FIXTURE_TIMEOUT, async move {
        let (port_base, reservations) = reserve_worker_ports(worker_count);
        let mut config = MediaConfig::default();
        config.worker_config.num_workers = worker_count;
        config.webrtc_server_port_base = port_base;
        let config = Arc::new(config);
        drop(reservations);

        let workers = Arc::new(WorkerManager::new(config.clone()).await.unwrap());
        assert_eq!(workers.live_worker_count().await, worker_count);
        let routers = RouterManager::new(workers.clone());
        test(workers.clone(), routers, config).await;

        let remaining = workers.get_load_distribution().await;
        assert_eq!(remaining.len(), worker_count);
        assert!(
            remaining.values().all(|count| *count == 0),
            "router-manager drop must refund every remaining reservation"
        );
        workers.shutdown().await.unwrap();
        assert_eq!(workers.live_worker_count().await, 0);
    })
    .await
    .expect("native allocation fixture exceeded its deadline");
}

async fn assert_balanced(workers: &WorkerManager, worker_count: usize, room_count: usize) {
    let distribution = workers.get_load_distribution().await;
    assert_eq!(distribution.len(), worker_count);
    assert_eq!(distribution.values().sum::<usize>(), room_count);

    let minimum = room_count / worker_count;
    let maximum = room_count.div_ceil(worker_count);
    for (worker_id, count) in distribution {
        assert!((minimum..=maximum).contains(&count));
        assert_eq!(
            workers
                .get_consumer_counter(worker_id)
                .unwrap()
                .load(Ordering::Relaxed),
            0,
            "these fixtures must exercise allocation before media consumers exist"
        );
    }
}

async fn assert_registered_distribution(workers: &WorkerManager, routers: &RouterManager) {
    let reported = workers.get_load_distribution().await;
    let mut registered: HashMap<_, usize> = reported.keys().map(|id| (*id, 0)).collect();
    for room_id in routers.get_active_rooms().await {
        let worker_id = routers.get_worker_id(&room_id).await.unwrap();
        *registered.get_mut(&worker_id).unwrap() += 1;
    }
    assert_eq!(
        reported, registered,
        "load accounting must match the workers that actually own the room routers"
    );
}

async fn check_sequential_distribution(worker_count: usize) {
    with_workers(worker_count, |workers, routers, config| async move {
        for index in 0..DISTRIBUTION_ROOMS {
            routers
                .create_router(format!("sequential-{index}"), config.router_config.clone())
                .await
                .unwrap();
            assert_balanced(&workers, worker_count, index + 1).await;
            assert_registered_distribution(&workers, &routers).await;
        }
        assert_eq!(routers.router_count().await, DISTRIBUTION_ROOMS);
        routers.close_all().await.unwrap();
        assert_balanced(&workers, worker_count, 0).await;
    })
    .await;
}

async fn check_concurrent_distribution(worker_count: usize) {
    with_workers(worker_count, |workers, routers, config| async move {
        let routers = Arc::new(routers);
        let start = Arc::new(Barrier::new(DISTRIBUTION_ROOMS + 1));
        let mut creates = JoinSet::new();
        for index in 0..DISTRIBUTION_ROOMS {
            let routers = routers.clone();
            let config = config.clone();
            let start = start.clone();
            creates.spawn(async move {
                start.wait().await;
                routers
                    .create_router(format!("concurrent-{index}"), config.router_config.clone())
                    .await
            });
        }
        start.wait().await;
        while let Some(result) = creates.join_next().await {
            result.unwrap().unwrap();
        }

        assert_eq!(routers.router_count().await, DISTRIBUTION_ROOMS);
        assert_balanced(&workers, worker_count, DISTRIBUTION_ROOMS).await;
        assert_registered_distribution(&workers, &routers).await;
        routers.close_all().await.unwrap();
        assert_balanced(&workers, worker_count, 0).await;
    })
    .await;
}

#[tokio::test]
async fn sequential_empty_rooms_use_both_workers() {
    check_sequential_distribution(2).await;
}

#[tokio::test]
async fn sequential_empty_rooms_use_all_four_workers() {
    check_sequential_distribution(4).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_empty_rooms_use_both_workers() {
    check_concurrent_distribution(2).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_empty_rooms_use_all_four_workers() {
    check_concurrent_distribution(4).await;
}

#[tokio::test]
async fn router_removal_close_all_and_drop_release_load_for_reuse() {
    with_workers(2, |workers, routers, config| async move {
        for index in 0..4 {
            routers
                .create_router(format!("lifecycle-{index}"), config.router_config.clone())
                .await
                .unwrap();
        }
        assert_balanced(&workers, 2, 4).await;

        let released_worker = routers.get_worker_id("lifecycle-0").await.unwrap();
        routers.remove_router("lifecycle-0").await.unwrap();
        assert!(!routers.has_router("lifecycle-0").await);
        let after_removal = workers.get_load_distribution().await;
        assert_eq!(after_removal[&released_worker], 1);
        assert_balanced(&workers, 2, 3).await;

        assert!(routers.remove_router("lifecycle-0").await.is_err());
        assert_eq!(workers.get_load_distribution().await, after_removal);

        routers
            .create_router("reused-slot".to_string(), config.router_config.clone())
            .await
            .unwrap();
        assert_eq!(
            routers.get_worker_id("reused-slot").await.unwrap(),
            released_worker
        );
        assert_balanced(&workers, 2, 4).await;

        routers.close_all().await.unwrap();
        assert_eq!(routers.router_count().await, 0);
        assert_balanced(&workers, 2, 0).await;
        routers.close_all().await.unwrap();
        assert_balanced(&workers, 2, 0).await;

        routers
            .create_router("after-close-all".to_string(), config.router_config.clone())
            .await
            .unwrap();
        assert_balanced(&workers, 2, 1).await;
        drop(routers);
        assert_balanced(&workers, 2, 0).await;
    })
    .await;
}

#[tokio::test]
async fn invalid_router_configuration_refunds_load_and_keeps_room_id_reusable() {
    with_workers(2, |workers, routers, config| async move {
        routers
            .create_router("existing".to_string(), config.router_config.clone())
            .await
            .unwrap();
        let before_failure = workers.get_load_distribution().await;

        // Codec validation rejects the duplicate payload type before sending a
        // native router-create request, so this does not require a failed worker.
        let codec = config.router_config.media_codecs[0].clone();
        let invalid = RouterConfig {
            media_codecs: vec![codec.clone(), codec],
        };
        let result = routers
            .create_router("retryable".to_string(), invalid)
            .await;
        assert!(matches!(result, Err(MediaError::RouterError(_))));
        assert_eq!(workers.get_load_distribution().await, before_failure);
        assert_eq!(routers.router_count().await, 1);
        assert!(!routers.has_router("retryable").await);
        assert!(routers.has_router("existing").await);

        routers
            .create_router("retryable".to_string(), config.router_config.clone())
            .await
            .unwrap();
        assert_balanced(&workers, 2, 2).await;
    })
    .await;
}

#[tokio::test]
async fn concurrent_duplicate_room_creation_keeps_one_router_and_one_reservation() {
    with_workers(2, |workers, routers, config| async move {
        let (first, second) = tokio::join!(
            routers.create_router("same-room".to_string(), config.router_config.clone()),
            routers.create_router("same-room".to_string(), config.router_config.clone())
        );
        let results = [first, second];
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(results.iter().filter(|result| result.is_err()).count(), 1);
        let retained_id = results.into_iter().find_map(|result| result.ok()).unwrap();
        assert_eq!(
            routers
                .get_router("same-room")
                .await
                .unwrap()
                .id()
                .to_string(),
            retained_id
        );
        assert_eq!(routers.router_count().await, 1);
        assert_balanced(&workers, 2, 1).await;
    })
    .await;
}

#[tokio::test]
async fn cancelled_router_insertion_refunds_load_without_async_cleanup() {
    with_workers(1, |workers, routers, config| async move {
        let (worker, reservation) = workers.reserve_worker().await.unwrap();
        drop(reservation);

        let created = Arc::new(Semaphore::new(0));
        let observer = worker.on_new_router({
            let created = created.clone();
            move |_| created.add_permits(1)
        });
        let readers = routers.routers.read().await;
        let mut create = Box::pin(
            routers.create_router("cancelled".to_string(), config.router_config.clone()),
        );

        // Cancel only after the native Router exists and storage is blocked.
        // This exercises first-party cancellation accounting, not cancellation
        // of an in-flight native IPC allocation.
        tokio::select! {
            result = &mut create => panic!("router insertion finished behind read guard: {result:?}"),
            permit = created.acquire() => permit.unwrap().forget(),
        }
        assert_balanced(&workers, 1, 1).await;
        drop(create);
        assert_balanced(&workers, 1, 0).await;
        assert!(!readers.contains_key("cancelled"));
        drop(readers);
        drop(observer);

        routers
            .create_router("cancelled".to_string(), config.router_config.clone())
            .await
            .unwrap();
        assert_balanced(&workers, 1, 1).await;
    })
    .await;
}
