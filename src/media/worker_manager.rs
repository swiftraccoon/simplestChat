#![forbid(unsafe_code)]

// Worker pool management for mediasoup

use crate::media::config::{MediaConfig, checked_worker_port};
use crate::media::types::{MediaError, MediaResult};
use anyhow::Result;
use mediasoup::prelude::*;
use mediasoup::worker::{WorkerDump, WorkerId};
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::RwLock as StdRwLock;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

/// Consumers remain the primary cost signal; reserved and registered routers
/// break ties so rooms created before media starts spread across the pool.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct WorkerLoad {
    consumers: usize,
    routers: usize,
}

/// Unavailable workers have no load entry and must not win against live ones.
fn select_worker_by_load(
    loads: impl IntoIterator<Item = Option<WorkerLoad>>,
) -> MediaResult<(usize, WorkerLoad)> {
    loads
        .into_iter()
        .enumerate()
        .filter_map(|(index, count)| count.map(|count| (index, count)))
        .min_by_key(|(_, count)| *count)
        .ok_or_else(|| MediaError::WorkerError("No live media workers available".to_string()))
}

fn worker_has_capacity(worker_closed: bool, server_closed: Option<bool>) -> bool {
    !worker_closed && server_closed == Some(false)
}

/// Owns one pending or registered router's contribution to allocation load.
///
/// Move this into the room's router entry after creation. Dropping it refunds
/// the reservation synchronously, including on errors, future cancellation,
/// room removal, and manager shutdown. It must not be cloned or released by a
/// spawned task: subsequent allocations must observe cleanup immediately.
#[must_use = "dropping the reservation releases the worker's router load"]
pub(crate) struct RouterLoadReservation {
    worker_id: WorkerId,
    worker_load: Arc<Mutex<HashMap<WorkerId, usize>>>,
}

impl Drop for RouterLoadReservation {
    fn drop(&mut self) {
        let mut load = self.worker_load.lock().unwrap_or_else(|e| e.into_inner());
        // A retired worker's entry may already have been removed. Its ID is
        // never reused, so this cannot decrement a replacement worker's load.
        if let Some(count) = load.get_mut(&self.worker_id) {
            *count = count.saturating_sub(1);
        }
    }
}

/// Manages a pool of mediasoup Workers
pub struct WorkerManager {
    workers: Arc<RwLock<Vec<Worker>>>,
    // Selection and reservation share this short synchronous critical section.
    // No native IPC or await is allowed while it is held.
    worker_load: Arc<Mutex<HashMap<WorkerId, usize>>>,
    webrtc_servers: Arc<RwLock<HashMap<WorkerId, WebRtcServer>>>,
    worker_consumer_counts: Arc<StdRwLock<HashMap<WorkerId, Arc<AtomicUsize>>>>,
    config: Arc<MediaConfig>,
    mediasoup_worker_manager: Arc<mediasoup::worker_manager::WorkerManager>,
    /// Worker deaths are queued here for the room manager's recovery task.
    deaths: tokio::sync::mpsc::Sender<WorkerId>,
    death_events: Mutex<Option<tokio::sync::mpsc::Receiver<WorkerId>>>,
}

impl WorkerManager {
    /// Creates a new `WorkerManager` with the specified configuration
    ///
    /// # Errors
    /// Returns an error if worker creation fails
    pub async fn new(config: Arc<MediaConfig>) -> Result<Self> {
        config.validate()?;
        let num_workers = config.worker_config.num_workers;
        info!("Creating WorkerManager with {} workers", num_workers);

        let mediasoup_worker_manager = Arc::new(mediasoup::worker_manager::WorkerManager::new());
        let (deaths, death_events) = tokio::sync::mpsc::channel(64);
        let mut workers = Vec::with_capacity(num_workers);
        let mut worker_load = HashMap::new();
        let mut webrtc_servers = HashMap::new();
        let mut worker_consumer_counts = HashMap::new();

        // Derive announced_address from transport config (if set)
        let announced_address = config
            .webrtc_transport_config
            .listen_ips
            .first()
            .and_then(|li| li.announced_address.clone());

        // Create all workers
        for i in 0..num_workers {
            let worker =
                Self::create_worker_with_manager(&config.worker_config, &mediasoup_worker_manager)
                    .await?;
            let worker_id = worker.id();

            info!("Created worker {} with id: {}", i, worker_id);

            // Set up worker event handlers
            Self::setup_worker_handlers(&worker, i, deaths.clone());

            // Create a WebRtcServer for this worker on a dedicated port
            let port = config.worker_port(i)?;
            let listen_info = ListenInfo {
                protocol: Protocol::Udp,
                ip: IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)),
                announced_address: announced_address.clone(),
                port: Some(port),
                port_range: None,
                flags: None,
                send_buffer_size: None,
                recv_buffer_size: None,
                expose_internal_ip: false,
            };
            let server_options =
                WebRtcServerOptions::new(WebRtcServerListenInfos::new(listen_info));
            let webrtc_server = worker
                .create_webrtc_server(server_options)
                .await
                .map_err(|e| {
                    MediaError::WorkerError(format!(
                        "Failed to create WebRtcServer on port {port} for worker {worker_id}: {e}"
                    ))
                })?;
            info!(
                "Created WebRtcServer on UDP port {} for worker {} (index {})",
                port, worker_id, i
            );
            webrtc_servers.insert(worker_id, webrtc_server);

            worker_load.insert(worker_id, 0);
            worker_consumer_counts.insert(worker_id, Arc::new(AtomicUsize::new(0)));
            workers.push(worker);
        }

        Ok(Self {
            workers: Arc::new(RwLock::new(workers)),
            worker_load: Arc::new(Mutex::new(worker_load)),
            webrtc_servers: Arc::new(RwLock::new(webrtc_servers)),
            worker_consumer_counts: Arc::new(StdRwLock::new(worker_consumer_counts)),
            config,
            mediasoup_worker_manager,
            deaths,
            death_events: Mutex::new(Some(death_events)),
        })
    }

    /// The death queue, handed once to the recovery task that owns it.
    pub fn take_death_events(&self) -> Option<tokio::sync::mpsc::Receiver<WorkerId>> {
        self.death_events
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
    }

    /// Queues a death exactly as the native callback does, for recovery tests.
    #[cfg(test)]
    pub(crate) fn simulate_death(&self, worker_id: WorkerId) -> bool {
        self.deaths.try_send(worker_id).is_ok()
    }

    /// Drops a live worker's listener and waits until its port can be bound
    /// again. A worker thread cannot be killed through the API, and this is
    /// the state a dead worker leaves behind for recreation.
    #[cfg(test)]
    pub(crate) async fn release_worker_listener(&self, worker_id: WorkerId) -> bool {
        let position = self
            .workers
            .read()
            .await
            .iter()
            .position(|worker| worker.id() == worker_id);
        let Some(port) = position.and_then(|pos| self.config.worker_port(pos).ok()) else {
            return false;
        };
        let Some(listener) = self.webrtc_servers.write().await.remove(&worker_id) else {
            return false;
        };
        drop(listener);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if std::net::UdpSocket::bind((Ipv4Addr::UNSPECIFIED, port)).is_ok() {
                return true;
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }

    /// Creates a single worker with the given configuration
    async fn create_worker_with_manager(
        config: &crate::media::config::WorkerConfig,
        manager: &mediasoup::worker_manager::WorkerManager,
    ) -> Result<Worker> {
        let worker_settings = config.to_worker_settings();

        let worker = manager
            .create_worker(worker_settings)
            .await
            .map_err(|e| MediaError::WorkerError(format!("Failed to create worker: {e}")))?;

        Ok(worker)
    }

    /// Sets up event handlers for a worker
    fn setup_worker_handlers(
        worker: &Worker,
        worker_index: usize,
        deaths: tokio::sync::mpsc::Sender<WorkerId>,
    ) {
        let worker_id = worker.id();

        // Handle worker death: allocation already excludes a closed worker; the
        // recovery task recreates it and closes its rooms with a rejoin notice.
        // .detach() — dropping the HandlerId would unregister the callback immediately
        worker
            .on_dead({
                move |reason| {
                    error!(
                        ?reason,
                        "Worker {} (index {}) died; queuing recreation and room recovery",
                        worker_id, worker_index
                    );
                    if deaths.try_send(worker_id).is_err() {
                        error!(
                            "Worker death queue is unavailable; worker {} will not be recreated until restart",
                            worker_id
                        );
                    }
                }
            })
            .detach();

        // Handle new WebRTC server (for TCP/TLS connections)
        worker
            .on_new_webrtc_server({
                move |webrtc_server| {
                    debug!("New WebRTC server created on worker {}", worker_index);

                    webrtc_server
                        .on_close({
                            move || {
                                debug!("WebRTC server closed on worker {}", worker_index);
                            }
                        })
                        .detach();
                }
            })
            .detach();
    }

    /// Reserves a router slot on an open worker with an open shared listener.
    ///
    /// Consumer count is the primary score; pending and registered router
    /// counts break ties. Selection and increment are serialized so concurrent
    /// empty-room creation does not repeatedly choose the same worker.
    /// The returned reservation must live with the registered router entry.
    ///
    /// # Errors
    /// Returns `MediaError::WorkerError` if no eligible worker is available or
    /// its router reservation count cannot be incremented.
    pub(crate) async fn reserve_worker(&self) -> MediaResult<(Worker, RouterLoadReservation)> {
        let workers = self.workers.read().await;
        let servers = self.webrtc_servers.read().await;

        let mut load = self.worker_load.lock().unwrap_or_else(|e| e.into_inner());
        let consumer_counts = self
            .worker_consumer_counts
            .read()
            .unwrap_or_else(|e| e.into_inner());
        let (best_idx, best_load) = select_worker_by_load(workers.iter().map(|worker| {
            let worker_id = worker.id();
            if !worker_has_capacity(
                worker.closed(),
                servers.get(&worker_id).map(WebRtcServer::closed),
            ) {
                return None;
            }
            Some(WorkerLoad {
                consumers: consumer_counts.get(&worker_id)?.load(Ordering::Relaxed),
                routers: *load.get(&worker_id)?,
            })
        }))?;

        let worker = workers[best_idx].clone();
        let worker_id = worker.id();

        let reserved_load = best_load.routers.checked_add(1).ok_or_else(|| {
            MediaError::WorkerError("Worker router reservation count exhausted".to_string())
        })?;
        load.insert(worker_id, reserved_load);
        drop(load);
        drop(consumer_counts);
        let reservation = RouterLoadReservation {
            worker_id,
            worker_load: Arc::clone(&self.worker_load),
        };

        debug!(
            "Selected worker {} (index {}, {} consumers, {} router reservations)",
            worker_id, best_idx, best_load.consumers, reserved_load
        );
        Ok((worker, reservation))
    }

    /// Gets the WebRtcServer associated with a worker.
    ///
    /// # Errors
    /// Returns `MediaError::WorkerError` if no open WebRtcServer exists for the given worker_id
    pub async fn get_webrtc_server(&self, worker_id: WorkerId) -> MediaResult<WebRtcServer> {
        let servers = self.webrtc_servers.read().await;
        servers
            .get(&worker_id)
            .filter(|server| !server.closed())
            .cloned()
            .ok_or_else(|| {
                MediaError::WorkerError(format!("No open WebRtcServer for worker: {worker_id}"))
            })
    }

    /// Gets the consumer counter for a specific worker (for real-time tracking)
    pub fn get_consumer_counter(&self, worker_id: WorkerId) -> Option<Arc<AtomicUsize>> {
        let counts = self
            .worker_consumer_counts
            .read()
            .unwrap_or_else(|e| e.into_inner());
        counts.get(&worker_id).cloned()
    }

    /// Counts workers with an open shared WebRTC listener without making IPC requests.
    pub async fn live_worker_count(&self) -> usize {
        let workers = self.workers.read().await;
        let servers = self.webrtc_servers.read().await;
        workers
            .iter()
            .filter(|worker| {
                worker_has_capacity(
                    worker.closed(),
                    servers.get(&worker.id()).map(WebRtcServer::closed),
                )
            })
            .count()
    }

    /// Gets current worker statistics
    pub async fn get_worker_stats(&self) -> Vec<WorkerDump> {
        let workers = self.workers.read().await;
        let mut stats = Vec::new();

        for worker in workers.iter() {
            if let Ok(worker_stats) = worker.dump().await {
                stats.push(worker_stats);
            }
        }

        stats
    }

    /// Gets pending plus registered router counts for allocation tie-breaking.
    pub async fn get_load_distribution(&self) -> HashMap<WorkerId, usize> {
        self.worker_load
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Checks if a worker is still alive
    pub async fn is_worker_alive(&self, worker_id: WorkerId) -> bool {
        let workers = self.workers.read().await;
        workers.iter().any(|w| w.id() == worker_id && !w.closed())
    }

    /// Replaces a dead worker with a new one on the same listener port.
    ///
    /// The replacement is fully built before the registries change, so a
    /// failure leaves the dead entry in place (already excluded from
    /// allocation) for a later attempt instead of silently shrinking the pool.
    ///
    /// # Errors
    /// Returns an error if the worker or its listener cannot be created; the
    /// pool is unchanged in that case.
    pub async fn recreate_worker(&self, dead_worker_id: WorkerId) -> MediaResult<()> {
        warn!("Recreating dead worker: {}", dead_worker_id);

        let mut workers = self.workers.write().await;
        let Some(pos) = workers.iter().position(|w| w.id() == dead_worker_id) else {
            warn!(
                "Worker {} is not in the pool; nothing to recreate",
                dead_worker_id
            );
            return Ok(());
        };

        let new_worker = Self::create_worker_with_manager(
            &self.config.worker_config,
            &self.mediasoup_worker_manager,
        )
        .await
        .map_err(|e| MediaError::WorkerError(format!("Failed to recreate worker: {e}")))?;
        let new_worker_id = new_worker.id();
        info!("Created replacement worker with id: {}", new_worker_id);
        Self::setup_worker_handlers(&new_worker, pos, self.deaths.clone());

        // The listener reuses the dead worker's port so announced addresses stay stable.
        let announced_address = self
            .config
            .webrtc_transport_config
            .listen_ips
            .first()
            .and_then(|li| li.announced_address.clone());
        let port = self.config.worker_port(pos).map_err(|error| {
            MediaError::ConfigurationError(format!("Invalid worker port configuration: {error}"))
        })?;
        let listen_info = ListenInfo {
            protocol: Protocol::Udp,
            ip: IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)),
            announced_address,
            port: Some(port),
            port_range: None,
            flags: None,
            send_buffer_size: None,
            recv_buffer_size: None,
            expose_internal_ip: false,
        };
        let server_options = WebRtcServerOptions::new(WebRtcServerListenInfos::new(listen_info));
        let webrtc_server = new_worker
            .create_webrtc_server(server_options)
            .await
            .map_err(|e| {
                MediaError::WorkerError(format!(
                    "Failed to create WebRtcServer for replacement worker {new_worker_id}: {e}"
                ))
            })?;

        // Swap the registries only now that the replacement is ready.
        let old_worker = std::mem::replace(&mut workers[pos], new_worker);
        {
            let mut servers = self.webrtc_servers.write().await;
            servers.remove(&dead_worker_id);
            servers.insert(new_worker_id, webrtc_server);
        }
        {
            let mut load = self.worker_load.lock().unwrap_or_else(|e| e.into_inner());
            load.remove(&dead_worker_id);
            load.insert(new_worker_id, 0);
        }
        {
            let mut counts = self
                .worker_consumer_counts
                .write()
                .unwrap_or_else(|e| e.into_inner());
            counts.remove(&dead_worker_id);
            counts.insert(new_worker_id, Arc::new(AtomicUsize::new(0)));
        }
        drop(old_worker);

        info!(
            "Replacement worker {} fully initialized with WebRtcServer on port {}",
            new_worker_id, port
        );
        Ok(())
    }

    /// Updates worker settings (requires recreating workers)
    ///
    /// # Errors
    /// Returns an error if worker recreation fails
    pub async fn update_settings(
        &self,
        new_config: crate::media::config::WorkerConfig,
    ) -> MediaResult<()> {
        info!("Updating worker settings");

        checked_worker_port(
            self.config.webrtc_server_port_base,
            new_config.num_workers,
            new_config.num_workers.saturating_sub(1),
        )
        .map_err(|error| {
            MediaError::ConfigurationError(format!("Invalid worker configuration: {error}"))
        })?;

        let announced_address = self
            .config
            .webrtc_transport_config
            .listen_ips
            .first()
            .and_then(|li| li.announced_address.clone());

        // This is a simplified version - in production you'd want graceful migration
        let mut workers = self.workers.write().await;
        let mut new_workers = Vec::new();
        let mut new_load = HashMap::new();
        let mut new_servers = HashMap::new();
        let mut new_consumer_counts = HashMap::new();

        // Close old workers - they close automatically when dropped

        // Create new workers with updated settings
        for i in 0..new_config.num_workers {
            let worker =
                Self::create_worker_with_manager(&new_config, &self.mediasoup_worker_manager)
                    .await
                    .map_err(|e| {
                        MediaError::WorkerError(format!("Failed to create worker: {e}"))
                    })?;

            let worker_id = worker.id();
            Self::setup_worker_handlers(&worker, i, self.deaths.clone());

            // Create WebRtcServer for each new worker
            let port = checked_worker_port(
                self.config.webrtc_server_port_base,
                new_config.num_workers,
                i,
            )
            .map_err(|error| {
                MediaError::ConfigurationError(format!(
                    "Invalid worker port configuration: {error}"
                ))
            })?;
            let listen_info = ListenInfo {
                protocol: Protocol::Udp,
                ip: IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)),
                announced_address: announced_address.clone(),
                port: Some(port),
                port_range: None,
                flags: None,
                send_buffer_size: None,
                recv_buffer_size: None,
                expose_internal_ip: false,
            };
            let server_options =
                WebRtcServerOptions::new(WebRtcServerListenInfos::new(listen_info));
            let webrtc_server = worker
                .create_webrtc_server(server_options)
                .await
                .map_err(|e| {
                    MediaError::WorkerError(format!(
                        "Failed to create WebRtcServer on port {port} for worker {worker_id}: {e}"
                    ))
                })?;

            new_load.insert(worker_id, 0);
            new_servers.insert(worker_id, webrtc_server);
            new_consumer_counts.insert(worker_id, Arc::new(AtomicUsize::new(0)));
            new_workers.push(worker);
        }

        // Clear old servers before dropping old workers
        self.webrtc_servers.write().await.clear();

        *workers = new_workers;
        *self.worker_load.lock().unwrap_or_else(|e| e.into_inner()) = new_load;
        *self.webrtc_servers.write().await = new_servers;
        {
            let mut counts = self
                .worker_consumer_counts
                .write()
                .unwrap_or_else(|e| e.into_inner());
            *counts = new_consumer_counts;
        }

        info!("Worker settings updated successfully");
        Ok(())
    }

    /// Gracefully shuts down all workers
    pub async fn shutdown(&self) -> Result<()> {
        info!("Shutting down all workers");

        // Drop WebRtcServers first (they reference workers)
        self.webrtc_servers.write().await.clear();

        let mut workers = self.workers.write().await;

        // Workers are automatically closed when dropped
        workers.clear();

        self.worker_load
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        self.worker_consumer_counts
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .clear();

        info!("All workers shut down successfully");
        Ok(())
    }
}

impl Drop for WorkerManager {
    fn drop(&mut self) {
        // Workers will be closed automatically when dropped
        // This is just for logging
        debug!("WorkerManager being dropped");
    }
}

/// Reserves `count` contiguous UDP ports for a test pool. The sockets are
/// released on return, just before the native listeners bind the ports.
#[cfg(test)]
pub(crate) fn reserve_worker_ports(count: u16) -> u16 {
    for _ in 0..64 {
        let first = std::net::UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).unwrap();
        let base = first.local_addr().unwrap().port();
        let rest: Option<Vec<std::net::UdpSocket>> = (1..count)
            .map(|offset| {
                base.checked_add(offset)
                    .and_then(|port| std::net::UdpSocket::bind((Ipv4Addr::UNSPECIFIED, port)).ok())
            })
            .collect();
        if rest.is_some() {
            return base;
        }
    }
    panic!("could not reserve {count} contiguous local UDP ports");
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn two_worker_manager() -> WorkerManager {
        let mut config = MediaConfig::default();
        config.worker_config.num_workers = 2;
        config.webrtc_server_port_base = reserve_worker_ports(2);
        WorkerManager::new(Arc::new(config)).await.unwrap()
    }

    async fn worker_ids(manager: &WorkerManager) -> Vec<WorkerId> {
        manager
            .workers
            .read()
            .await
            .iter()
            .map(Worker::id)
            .collect()
    }

    #[tokio::test]
    async fn recreate_worker_replaces_a_worker_whose_listener_is_gone() {
        let manager = two_worker_manager().await;
        let ids = worker_ids(&manager).await;
        assert!(manager.release_worker_listener(ids[0]).await);
        assert_eq!(manager.live_worker_count().await, 1);

        manager.recreate_worker(ids[0]).await.unwrap();

        assert_eq!(manager.live_worker_count().await, 2);
        let replaced = worker_ids(&manager).await;
        assert_ne!(replaced[0], ids[0], "the dead worker is replaced in place");
        assert_eq!(replaced[1], ids[1], "the live worker is untouched");
        assert!(manager.get_webrtc_server(replaced[0]).await.is_ok());
        assert!(manager.get_webrtc_server(ids[0]).await.is_err());
        let load = manager.get_load_distribution().await;
        assert_eq!(load.get(&replaced[0]), Some(&0));
        assert!(!load.contains_key(&ids[0]));
        assert!(manager.get_consumer_counter(replaced[0]).is_some());
        assert!(manager.get_consumer_counter(ids[0]).is_none());
        manager.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn recreate_worker_keeps_the_pool_when_the_replacement_cannot_bind() {
        let manager = two_worker_manager().await;
        let ids = worker_ids(&manager).await;

        // The worker is alive and still holds its port, so the replacement
        // cannot bind it; the pool must not lose the slot.
        let error = manager.recreate_worker(ids[0]).await.unwrap_err();
        assert!(matches!(error, MediaError::WorkerError(_)), "{error}");

        assert_eq!(manager.live_worker_count().await, 2);
        assert_eq!(worker_ids(&manager).await, ids);
        assert!(manager.get_webrtc_server(ids[0]).await.is_ok());
        manager.shutdown().await.unwrap();
    }

    fn load(consumers: usize, routers: usize) -> WorkerLoad {
        WorkerLoad { consumers, routers }
    }

    #[test]
    fn closed_workers_or_listeners_have_no_ready_capacity() {
        assert!(worker_has_capacity(false, Some(false)));
        assert!(!worker_has_capacity(true, Some(false)), "dead worker");
        assert!(!worker_has_capacity(false, Some(true)), "closed listener");
        assert!(!worker_has_capacity(true, Some(true)));
        assert!(
            !worker_has_capacity(false, None),
            "listener not initialized"
        );
    }

    #[test]
    fn worker_selection_skips_unavailable_workers() {
        assert_eq!(
            select_worker_by_load([None, Some(load(8, 1)), Some(load(3, 2)), None]).unwrap(),
            (2, load(3, 2)),
            "dead workers must not outrank live workers carrying calls"
        );
        assert_eq!(
            select_worker_by_load([None, Some(load(usize::MAX, usize::MAX))]).unwrap(),
            (1, load(usize::MAX, usize::MAX)),
            "a live worker remains selectable at any load"
        );
    }

    #[test]
    fn worker_selection_prioritizes_consumers_and_breaks_ties_by_router_load() {
        assert_eq!(
            select_worker_by_load([Some(load(1, 0)), Some(load(0, usize::MAX))]).unwrap(),
            (1, load(0, usize::MAX)),
            "router counts must not override the primary consumer score or overflow a sum"
        );
        assert_eq!(
            select_worker_by_load([Some(load(0, 3)), Some(load(0, 1)), Some(load(0, 2))]).unwrap(),
            (1, load(0, 1)),
            "rooms without consumers must spread by pending and registered router counts"
        );
        assert_eq!(
            select_worker_by_load([Some(load(7, 3)), Some(load(7, 1))]).unwrap(),
            (1, load(7, 1)),
            "router counts also break ties between workers already carrying media"
        );
        assert_eq!(
            select_worker_by_load([Some(load(0, 0)), Some(load(0, 0))]).unwrap(),
            (0, load(0, 0)),
            "equal scores keep a deterministic worker order"
        );
    }

    #[test]
    fn cancelling_a_reservation_refunds_load_without_a_runtime() {
        let worker_id = "00000000-0000-4000-8000-000000000001".parse().unwrap();
        let worker_load = Arc::new(Mutex::new(HashMap::from([(worker_id, 1)])));
        let reservation = RouterLoadReservation {
            worker_id,
            worker_load: Arc::clone(&worker_load),
        };
        let mut future = Box::pin(async move {
            std::future::pending::<()>().await;
            drop(reservation);
        });
        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
        assert!(std::future::Future::poll(future.as_mut(), &mut context).is_pending());
        assert_eq!(worker_load.lock().unwrap()[&worker_id], 1);
        drop(future);
        assert_eq!(worker_load.lock().unwrap()[&worker_id], 0);
    }

    #[test]
    fn retired_worker_reservations_do_not_change_replacement_load() {
        let old_id = "00000000-0000-4000-8000-000000000001".parse().unwrap();
        let new_id = "00000000-0000-4000-8000-000000000002".parse().unwrap();
        let worker_load = Arc::new(Mutex::new(HashMap::from([(old_id, 1)])));
        let reservation = RouterLoadReservation {
            worker_id: old_id,
            worker_load: Arc::clone(&worker_load),
        };
        {
            let mut load = worker_load.lock().unwrap();
            load.remove(&old_id);
            load.insert(new_id, 4);
        }
        drop(reservation);
        assert_eq!(*worker_load.lock().unwrap(), HashMap::from([(new_id, 4)]));
    }

    #[test]
    fn worker_selection_reports_no_live_capacity() {
        for loads in [vec![], vec![None], vec![None, None]] {
            assert!(matches!(
                select_worker_by_load(loads),
                Err(MediaError::WorkerError(message))
                    if message == "No live media workers available"
            ));
        }
    }

    #[tokio::test]
    async fn test_worker_creation() {
        // Do not compete with an application already listening on the default
        // media ports. Release an OS-selected port immediately before startup.
        let reservation = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let mut config = MediaConfig::default();
        config.worker_config.num_workers = 1;
        config.webrtc_server_port_base = reservation.local_addr().unwrap().port();
        drop(reservation);
        let config = Arc::new(config);
        let manager = WorkerManager::new(config).await;
        assert!(manager.is_ok());

        if let Ok(manager) = manager {
            assert_eq!(manager.live_worker_count().await, 1);
            let worker = manager.reserve_worker().await;
            assert!(worker.is_ok());
            assert_eq!(
                manager
                    .get_load_distribution()
                    .await
                    .values()
                    .sum::<usize>(),
                1
            );
            drop(worker);
            assert_eq!(
                manager
                    .get_load_distribution()
                    .await
                    .values()
                    .sum::<usize>(),
                0
            );

            // Keep the listener alive but remove it from the registry. This
            // exercises missing-listener exclusion without killing a worker.
            let (worker_id, listener) = {
                let mut servers = manager.webrtc_servers.write().await;
                let worker_id = *servers.keys().next().unwrap();
                (worker_id, servers.remove(&worker_id).unwrap())
            };
            assert!(!listener.closed());
            assert_eq!(manager.live_worker_count().await, 0);
            assert!(manager.reserve_worker().await.is_err());
            assert!(manager.get_webrtc_server(worker_id).await.is_err());
            assert_eq!(manager.get_load_distribution().await[&worker_id], 0);
            manager
                .webrtc_servers
                .write()
                .await
                .insert(worker_id, listener);
            assert!(manager.get_webrtc_server(worker_id).await.is_ok());
            assert_eq!(manager.live_worker_count().await, 1);
            manager.shutdown().await.unwrap();
            assert_eq!(manager.live_worker_count().await, 0);
        }
    }
}
