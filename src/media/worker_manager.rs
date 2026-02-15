#![forbid(unsafe_code)]

// Worker pool management for mediasoup

use crate::media::config::MediaConfig;
use crate::media::types::{MediaError, MediaResult};
use mediasoup::prelude::*;
use mediasoup::worker::{WorkerId, WorkerDump};
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::sync::RwLock as StdRwLock;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::RwLock;
use tracing::{info, warn, error, debug};
use anyhow::Result;

/// Manages a pool of mediasoup Workers
pub struct WorkerManager {
    workers: Arc<RwLock<Vec<Worker>>>,
    worker_load: Arc<RwLock<HashMap<WorkerId, usize>>>,
    webrtc_servers: Arc<RwLock<HashMap<WorkerId, WebRtcServer>>>,
    worker_consumer_counts: Arc<StdRwLock<HashMap<WorkerId, Arc<AtomicUsize>>>>,
    config: Arc<MediaConfig>,
    next_worker_idx: AtomicUsize,
    mediasoup_worker_manager: Arc<mediasoup::worker_manager::WorkerManager>,
}

impl WorkerManager {
    /// Creates a new `WorkerManager` with the specified configuration
    ///
    /// # Errors
    /// Returns an error if worker creation fails
    pub async fn new(config: Arc<MediaConfig>) -> Result<Self> {
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
            let worker = Self::create_worker_with_manager(&config.worker_config, &mediasoup_worker_manager).await?;
            let worker_id = worker.id();

            info!("Created worker {} with id: {}", i, worker_id);

            // Set up worker event handlers
            Self::setup_worker_handlers(&worker, i);

            // Create a WebRtcServer for this worker on a dedicated port
            let port = config.webrtc_server_port_base + i as u16;
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
            let server_options = WebRtcServerOptions::new(
                WebRtcServerListenInfos::new(listen_info),
            );
            let webrtc_server = worker
                .create_webrtc_server(server_options)
                .await
                .map_err(|e| MediaError::WorkerError(
                    format!("Failed to create WebRtcServer on port {port} for worker {worker_id}: {e}")
                ))?;
            info!("Created WebRtcServer on UDP port {} for worker {} (index {})", port, worker_id, i);
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
            next_worker_idx: AtomicUsize::new(0),
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
        worker.on_dead({
            move |_reason| {
                error!("Worker {} (index {}) died!", worker_id, worker_index);
                // In production, you'd want to recreate the worker here
            }
        });

        // Handle new WebRTC server (for TCP/TLS connections)
        worker.on_new_webrtc_server({
            move |webrtc_server| {
                debug!("New WebRTC server created on worker {}", worker_index);

                webrtc_server.on_close({
                    move || {
                        debug!("WebRTC server closed on worker {}", worker_index);
                    }
                });
            }
        });
    }
    
    /// Gets the least loaded worker based on real-time consumer counts.
    /// Returns both the Worker and its WorkerId (needed to look up the WebRtcServer).
    ///
    /// # Errors
    /// Returns `MediaError::WorkerError` if no workers are available
    pub async fn get_least_loaded_worker(&self) -> MediaResult<(Worker, WorkerId)> {
        let workers = self.workers.read().await;

        if workers.is_empty() {
            return Err(MediaError::WorkerError("No workers available".to_string()));
        }

        // Select worker with the lowest consumer count
        let consumer_counts = self.worker_consumer_counts.read().unwrap_or_else(|e| e.into_inner());
        let mut best_idx = 0;
        let mut best_count = usize::MAX;
        for (idx, worker) in workers.iter().enumerate() {
            let count = consumer_counts.get(&worker.id())
                .map(|c| c.load(Ordering::Relaxed))
                .unwrap_or(0);
            if count < best_count {
                best_count = count;
                best_idx = idx;
            }
        }

        let worker = workers[best_idx].clone();
        let worker_id = worker.id();

        // Increment router load counter for this worker
        let mut load = self.worker_load.write().await;
        if let Some(count) = load.get_mut(&worker_id) {
            *count += 1;
        }

        debug!("Selected worker {} (index {}, {} consumers)", worker_id, best_idx, best_count);
        Ok((worker, worker_id))
    }
    
    /// Gets the WebRtcServer associated with a worker.
    ///
    /// # Errors
    /// Returns `MediaError::WorkerError` if no WebRtcServer exists for the given worker_id
    pub async fn get_webrtc_server(&self, worker_id: WorkerId) -> MediaResult<WebRtcServer> {
        let servers = self.webrtc_servers.read().await;
        servers
            .get(&worker_id)
            .cloned()
            .ok_or_else(|| MediaError::WorkerError(
                format!("No WebRtcServer found for worker: {worker_id}")
            ))
    }

    /// Gets the consumer counter for a specific worker (for real-time tracking)
    pub fn get_consumer_counter(&self, worker_id: WorkerId) -> Option<Arc<AtomicUsize>> {
        let counts = self.worker_consumer_counts.read().unwrap_or_else(|e| e.into_inner());
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
            workers.remove(pos);

            // Create a new worker
            let new_worker = Self::create_worker_with_manager(&self.config.worker_config, &self.mediasoup_worker_manager)
                .await
                .map_err(|e| MediaError::WorkerError(format!("Failed to recreate worker: {e}")))?;

            let new_worker_id = new_worker.id();
            info!("Created replacement worker with id: {}", new_worker_id);

            // Set up handlers
            Self::setup_worker_handlers(&new_worker, pos);

            // Create WebRtcServer for replacement worker (reuse same port)
            let announced_address = self.config
                .webrtc_transport_config
                .listen_ips
                .first()
                .and_then(|li| li.announced_address.clone());
            let port = self.config.webrtc_server_port_base + pos as u16;
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
            let server_options = WebRtcServerOptions::new(
                WebRtcServerListenInfos::new(listen_info),
            );
            let webrtc_server = new_worker
                .create_webrtc_server(server_options)
                .await
                .map_err(|e| MediaError::WorkerError(
                    format!("Failed to create WebRtcServer for replacement worker {new_worker_id}: {e}")
                ))?;

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
                let mut counts = self.worker_consumer_counts.write().unwrap_or_else(|e| e.into_inner());
                counts.remove(&dead_worker_id);
                counts.insert(new_worker_id, Arc::new(AtomicUsize::new(0)));
            }

            // Add the new worker
            workers.insert(pos, new_worker);

            info!("Replacement worker {} fully initialized with WebRtcServer on port {}", new_worker_id, port);
        }

        Ok(())
    }
    
    /// Updates worker settings (requires recreating workers)
    ///
    /// # Errors
    /// Returns an error if worker recreation fails
    pub async fn update_settings(&self, new_config: crate::media::config::WorkerConfig) -> MediaResult<()> {
        info!("Updating worker settings");

        let announced_address = self.config
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
            let worker = Self::create_worker_with_manager(&new_config, &self.mediasoup_worker_manager).await
                .map_err(|e| MediaError::WorkerError(format!("Failed to create worker: {e}")))?;

            let worker_id = worker.id();
            Self::setup_worker_handlers(&worker, i);

            // Create WebRtcServer for each new worker
            let port = self.config.webrtc_server_port_base + i as u16;
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
            let server_options = WebRtcServerOptions::new(
                WebRtcServerListenInfos::new(listen_info),
            );
            let webrtc_server = worker
                .create_webrtc_server(server_options)
                .await
                .map_err(|e| MediaError::WorkerError(
                    format!("Failed to create WebRtcServer on port {port} for worker {worker_id}: {e}")
                ))?;

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
            let mut counts = self.worker_consumer_counts.write().unwrap_or_else(|e| e.into_inner());
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
        self.worker_consumer_counts.write().unwrap_or_else(|e| e.into_inner()).clear();

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
    
    #[tokio::test]
    async fn test_worker_creation() {
        let config = Arc::new(MediaConfig::default());
        let manager = WorkerManager::new(config).await;
        assert!(manager.is_ok());
        
        if let Ok(manager) = manager {
            let worker = manager.get_least_loaded_worker().await;
            assert!(worker.is_ok());
        }
    }
}
