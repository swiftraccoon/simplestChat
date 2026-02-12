#![forbid(unsafe_code)]

// Worker pool management for mediasoup

use crate::media::config::MediaConfig;
use crate::media::types::{MediaError, MediaResult};
use mediasoup::prelude::*;
use mediasoup::worker::{WorkerId, WorkerDump};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::RwLock;
use tracing::{info, warn, error, debug};
use anyhow::Result;

/// Manages a pool of mediasoup Workers
pub struct WorkerManager {
    workers: Arc<RwLock<Vec<Worker>>>,
    worker_load: Arc<RwLock<HashMap<WorkerId, usize>>>,
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

        // Create all workers
        for i in 0..num_workers {
            let worker = Self::create_worker_with_manager(&config.worker_config, &mediasoup_worker_manager).await?;
            let worker_id = worker.id();

            info!("Created worker {} with id: {}", i, worker_id);

            // Set up worker event handlers
            Self::setup_worker_handlers(&worker, i);

            worker_load.insert(worker_id, 0);
            workers.push(worker);
        }

        Ok(Self {
            workers: Arc::new(RwLock::new(workers)),
            worker_load: Arc::new(RwLock::new(worker_load)),
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
    
    /// Gets the least loaded worker using round-robin with load balancing
    ///
    /// # Errors
    /// Returns `MediaError::WorkerError` if no workers are available
    pub async fn get_least_loaded_worker(&self) -> MediaResult<Worker> {
        let workers = self.workers.read().await;

        if workers.is_empty() {
            return Err(MediaError::WorkerError("No workers available".to_string()));
        }

        // Simple round-robin for now, but could be enhanced with actual load metrics
        // Using Relaxed ordering is safe here - occasional duplicates in load balancing are acceptable
        let idx = self.next_worker_idx.fetch_add(1, Ordering::Relaxed) % workers.len();
        let worker = workers[idx].clone();
        
        // Increment load counter for this worker
        let mut load = self.worker_load.write().await;
        if let Some(count) = load.get_mut(&worker.id()) {
            *count += 1;
        }
        
        debug!("Selected worker {} (index {})", worker.id(), idx);
        Ok(worker)
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

            // Update load tracking
            let mut load = self.worker_load.write().await;
            load.remove(&dead_worker_id);
            load.insert(new_worker_id, 0);

            // Add the new worker
            workers.insert(pos, new_worker);
        }

        Ok(())
    }
    
    /// Updates worker settings (requires recreating workers)
    ///
    /// # Errors
    /// Returns an error if worker recreation fails
    pub async fn update_settings(&self, new_config: crate::media::config::WorkerConfig) -> MediaResult<()> {
        info!("Updating worker settings");

        // This is a simplified version - in production you'd want graceful migration
        let mut workers = self.workers.write().await;
        let mut new_workers = Vec::new();
        let mut new_load = HashMap::new();

        // Close old workers - they close automatically when dropped

        // Create new workers with updated settings
        for i in 0..new_config.num_workers {
            let worker = Self::create_worker_with_manager(&new_config, &self.mediasoup_worker_manager).await
                .map_err(|e| MediaError::WorkerError(format!("Failed to create worker: {e}")))?;

            let worker_id = worker.id();
            Self::setup_worker_handlers(&worker, i);
            new_load.insert(worker_id, 0);
            new_workers.push(worker);
        }

        *workers = new_workers;
        *self.worker_load.write().await = new_load;

        info!("Worker settings updated successfully");
        Ok(())
    }
    
    /// Gracefully shuts down all workers
    pub async fn shutdown(&self) -> Result<()> {
        info!("Shutting down all workers");

        let mut workers = self.workers.write().await;

        // Workers are automatically closed when dropped
        workers.clear();

        self.worker_load.write().await.clear();

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
