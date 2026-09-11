#![forbid(unsafe_code)]

// Worker pool management for mediasoup

use crate::media::config::{MediaConfig, checked_worker_port};
use crate::media::types::{MediaError, MediaResult};
use anyhow::Result;
use mediasoup::prelude::*;
use mediasoup::worker::{WorkerDump, WorkerId};
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::sync::RwLock as StdRwLock;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

/// Unavailable workers have no load entry and must not win against live ones.
fn select_worker_by_load(
    loads: impl IntoIterator<Item = Option<usize>>,
) -> MediaResult<(usize, usize)> {
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

/// Manages a pool of mediasoup Workers
pub struct WorkerManager {
    workers: Arc<RwLock<Vec<Worker>>>,
    worker_load: Arc<RwLock<HashMap<WorkerId, usize>>>,
    webrtc_servers: Arc<RwLock<HashMap<WorkerId, WebRtcServer>>>,
    worker_consumer_counts: Arc<StdRwLock<HashMap<WorkerId, Arc<AtomicUsize>>>>,
    config: Arc<MediaConfig>,
    mediasoup_worker_manager: Arc<mediasoup::worker_manager::WorkerManager>,
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
            Self::setup_worker_handlers(&worker, i);

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
            worker_load: Arc::new(RwLock::new(worker_load)),
            webrtc_servers: Arc::new(RwLock::new(webrtc_servers)),
            worker_consumer_counts: Arc::new(StdRwLock::new(worker_consumer_counts)),
            config,
            mediasoup_worker_manager,
        })
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
    fn setup_worker_handlers(worker: &Worker, worker_index: usize) {
        let worker_id = worker.id();

        // Handle worker death
        // .detach() — dropping the HandlerId would unregister the callback immediately
        worker
            .on_dead({
                move |reason| {
                    error!(
                        ?reason,
                        "Worker {} (index {}) died; excluded from new room allocation. Restart the server to restore worker capacity",
                        worker_id, worker_index
                    );
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

    /// Gets the least loaded worker based on real-time consumer counts.
    /// Returns both the Worker and its WorkerId (needed to look up the WebRtcServer).
    /// Selection reserves one router-load slot before returning. The caller
    /// must decrement that slot if router creation fails, or on router removal;
    /// this is separate from the consumer counters used to choose a worker.
    ///
    /// # Errors
    /// Returns `MediaError::WorkerError` if no live workers are available
    pub async fn get_least_loaded_worker(&self) -> MediaResult<(Worker, WorkerId)> {
        let workers = self.workers.read().await;

        // A dead worker's consumers close and its count falls to zero, so
        // exclude closed workers before comparing live-worker load.
        let (best_idx, best_count) = {
            let consumer_counts = self
                .worker_consumer_counts
                .read()
                .unwrap_or_else(|e| e.into_inner());
            select_worker_by_load(workers.iter().map(|worker| {
                (!worker.closed()).then(|| {
                    consumer_counts
                        .get(&worker.id())
                        .map(|count| count.load(Ordering::Relaxed))
                        .unwrap_or(0)
                })
            }))?
        }; // consumer_counts guard dropped here

        let worker = workers[best_idx].clone();
        let worker_id = worker.id();

        // Increment router load counter for this worker
        let mut load = self.worker_load.write().await;
        if let Some(count) = load.get_mut(&worker_id) {
            *count += 1;
        }

        debug!(
            "Selected worker {} (index {}, {} consumers)",
            worker_id, best_idx, best_count
        );
        Ok((worker, worker_id))
    }

    /// Gets the WebRtcServer associated with a worker.
    ///
    /// # Errors
    /// Returns `MediaError::WorkerError` if no WebRtcServer exists for the given worker_id
    pub async fn get_webrtc_server(&self, worker_id: WorkerId) -> MediaResult<WebRtcServer> {
        let servers = self.webrtc_servers.read().await;
        servers.get(&worker_id).cloned().ok_or_else(|| {
            MediaError::WorkerError(format!("No WebRtcServer found for worker: {worker_id}"))
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

    /// Decrements the load counter for a worker (called when a router is closed)
    ///
    /// # Errors
    /// This function currently never returns an error but returns `Result` for API consistency
    pub async fn decrement_worker_load(&self, worker_id: WorkerId) -> MediaResult<()> {
        let mut load = self.worker_load.write().await;
        if let Some(count) = load.get_mut(&worker_id) {
            *count = count.saturating_sub(1);
            debug!("Decremented load for worker {} to {}", worker_id, *count);
        }
        Ok(())
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

    /// Gets the current load distribution across workers
    pub async fn get_load_distribution(&self) -> HashMap<WorkerId, usize> {
        self.worker_load.read().await.clone()
    }

    /// Checks if a worker is still alive
    pub async fn is_worker_alive(&self, worker_id: WorkerId) -> bool {
        let workers = self.workers.read().await;
        workers.iter().any(|w| w.id() == worker_id && !w.closed())
    }

    /// Recreates a dead worker (for fault tolerance)
    ///
    /// # Errors
    /// Returns an error if the worker cannot be recreated
    pub async fn recreate_worker(&self, dead_worker_id: WorkerId) -> MediaResult<()> {
        warn!("Recreating dead worker: {}", dead_worker_id);

        let mut workers = self.workers.write().await;

        // Find and remove the dead worker
        if let Some(pos) = workers.iter().position(|w| w.id() == dead_worker_id) {
            drop(workers.remove(pos));

            // Create a new worker
            let new_worker = Self::create_worker_with_manager(
                &self.config.worker_config,
                &self.mediasoup_worker_manager,
            )
            .await
            .map_err(|e| MediaError::WorkerError(format!("Failed to recreate worker: {e}")))?;

            let new_worker_id = new_worker.id();
            info!("Created replacement worker with id: {}", new_worker_id);

            // Set up handlers
            Self::setup_worker_handlers(&new_worker, pos);

            // Create WebRtcServer for replacement worker (reuse same port)
            let announced_address = self
                .config
                .webrtc_transport_config
                .listen_ips
                .first()
                .and_then(|li| li.announced_address.clone());
            let port = self.config.worker_port(pos).map_err(|error| {
                MediaError::ConfigurationError(format!(
                    "Invalid worker port configuration: {error}"
                ))
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
            let server_options =
                WebRtcServerOptions::new(WebRtcServerListenInfos::new(listen_info));
            let webrtc_server = new_worker
                .create_webrtc_server(server_options)
                .await
                .map_err(|e| {
                    MediaError::WorkerError(format!(
                        "Failed to create WebRtcServer for replacement worker {new_worker_id}: {e}"
                    ))
                })?;

            // Update WebRtcServer map
            let mut servers = self.webrtc_servers.write().await;
            servers.remove(&dead_worker_id);
            servers.insert(new_worker_id, webrtc_server);

            // Update load tracking
            let mut load = self.worker_load.write().await;
            load.remove(&dead_worker_id);
            load.insert(new_worker_id, 0);

            // Update consumer counter
            {
                let mut counts = self
                    .worker_consumer_counts
                    .write()
                    .unwrap_or_else(|e| e.into_inner());
                counts.remove(&dead_worker_id);
                counts.insert(new_worker_id, Arc::new(AtomicUsize::new(0)));
            }

            // Add the new worker
            workers.insert(pos, new_worker);

            info!(
                "Replacement worker {} fully initialized with WebRtcServer on port {}",
                new_worker_id, port
            );
        }

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
            Self::setup_worker_handlers(&worker, i);

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
        *self.worker_load.write().await = new_load;
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

        self.worker_load.write().await.clear();
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

#[cfg(test)]
mod tests {
    use super::*;

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
            select_worker_by_load([None, Some(8), Some(3), None]).unwrap(),
            (2, 3),
            "dead workers must not outrank live workers carrying calls"
        );
        assert_eq!(
            select_worker_by_load([None, Some(usize::MAX)]).unwrap(),
            (1, usize::MAX),
            "a live worker remains selectable at any load"
        );
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
            let worker = manager.get_least_loaded_worker().await;
            assert!(worker.is_ok());
            drop(worker);
            manager.shutdown().await.unwrap();
            assert_eq!(manager.live_worker_count().await, 0);
        }
    }
}
