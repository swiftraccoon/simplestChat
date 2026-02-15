#![forbid(unsafe_code)]

// Media module - mediasoup worker and router management
// Handles mediasoup workers, routers, transports, producers, and consumers

pub mod worker_manager;
pub mod router_manager;
pub mod transport_manager;
pub mod config;
pub mod types;

pub use worker_manager::WorkerManager;
pub use router_manager::RouterManager;
pub use transport_manager::TransportManager;
pub use config::{MediaConfig, WorkerConfig, RouterConfig};
pub use types::{MediaError, MediaResult, TransportInfo, ProducerInfo, ConsumerInfo};

use std::sync::Arc;
use anyhow::Result;
use mediasoup::webrtc_server::WebRtcServer;
use tracing::{info, debug};

/// Main MediaServer struct that coordinates all mediasoup operations
pub struct MediaServer {
    worker_manager: Arc<WorkerManager>,
    router_manager: Arc<RouterManager>,
    transport_manager: Arc<TransportManager>,
    config: Arc<MediaConfig>,
}

impl MediaServer {
    /// Creates a new MediaServer instance with the given configuration
    pub async fn new(config: MediaConfig) -> Result<Self> {
        info!("Initializing MediaServer with {} workers", config.worker_config.num_workers);
        
        let config = Arc::new(config);
        let worker_manager = Arc::new(WorkerManager::new(config.clone()).await?);
        let router_manager = Arc::new(RouterManager::new(worker_manager.clone()));
        let transport_manager = Arc::new(TransportManager::new());
        
        Ok(Self {
            worker_manager,
            router_manager,
            transport_manager,
            config,
        })
    }
    
    /// Creates a new router for a room
    pub async fn create_router(&self, room_id: String) -> Result<String> {
        debug!("Creating router for room: {}", room_id);
        self.router_manager.create_router(room_id, self.config.router_config.clone())
            .await
            .map_err(|e| anyhow::anyhow!(e))
    }

    /// Removes a router when a room is closed
    pub async fn remove_router(&self, room_id: &str) -> Result<()> {
        debug!("Removing router for room: {}", room_id);
        self.router_manager.remove_router(room_id)
            .await
            .map_err(|e| anyhow::anyhow!(e))
    }
    
    /// Gets the worker manager for direct access if needed
    pub fn worker_manager(&self) -> Arc<WorkerManager> {
        self.worker_manager.clone()
    }
    
    /// Gets the router manager for direct access if needed
    pub fn router_manager(&self) -> Arc<RouterManager> {
        self.router_manager.clone()
    }
    
    /// Gets the transport manager for direct access if needed
    pub fn transport_manager(&self) -> Arc<TransportManager> {
        self.transport_manager.clone()
    }

    /// Gets the configuration
    pub fn config(&self) -> Arc<MediaConfig> {
        self.config.clone()
    }

    /// Gets the consumer counter for the worker hosting a room's router
    pub async fn get_consumer_counter_for_room(&self, room_id: &str) -> Result<Option<Arc<std::sync::atomic::AtomicUsize>>> {
        self.router_manager.get_consumer_counter_for_room(room_id).await
            .map_err(|e| anyhow::anyhow!(e))
    }

    /// Gets the WebRtcServer for the worker that hosts a room's router
    pub async fn get_webrtc_server_for_room(&self, room_id: &str) -> Result<WebRtcServer> {
        let worker_id = self.router_manager.get_worker_id(room_id).await
            .map_err(|e| anyhow::anyhow!(e))?;
        self.worker_manager.get_webrtc_server(worker_id).await
            .map_err(|e| anyhow::anyhow!(e))
    }

    /// Gracefully shuts down all workers and cleans up resources
    pub async fn shutdown(&self) -> Result<()> {
        info!("Shutting down MediaServer");
        
        // First close all transports
        self.transport_manager.close_all().await?;
        
        // Then close all routers
        self.router_manager.close_all().await?;
        
        // Finally close all workers
        self.worker_manager.shutdown().await?;
        
        info!("MediaServer shutdown complete");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[tokio::test]
    async fn test_media_server_creation() {
        let config = MediaConfig::default();
        let server = MediaServer::new(config).await;
        assert!(server.is_ok());
    }
}
