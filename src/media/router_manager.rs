#![forbid(unsafe_code)]

// Router management for rooms

use crate::media::config::RouterConfig;
use crate::media::types::{MediaError, MediaResult};
use crate::media::worker_manager::WorkerManager;
use mediasoup::prelude::*;
use mediasoup::worker::WorkerId;
use mediasoup::router::RouterDump;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{info, debug, warn};
use anyhow::Result;
use std::net::IpAddr;

/// Information about a router and its associated worker
#[derive(Clone)]
struct RouterInfo {
    router: Router,
    worker_id: WorkerId,
}

/// Manages routers for different rooms
pub struct RouterManager {
    routers: Arc<RwLock<HashMap<String, RouterInfo>>>,
    worker_manager: Arc<WorkerManager>,
}

impl RouterManager {
    /// Creates a new RouterManager
    pub fn new(worker_manager: Arc<WorkerManager>) -> Self {
        Self {
            routers: Arc::new(RwLock::new(HashMap::new())),
            worker_manager,
        }
    }
    
    /// Creates a new router for a room
    pub async fn create_router(&self, room_id: String, config: RouterConfig) -> MediaResult<String> {
        // Check if router already exists
        {
            let routers = self.routers.read().await;
            if routers.contains_key(&room_id) {
                return Err(MediaError::RouterError(
                    format!("Router already exists for room: {room_id}")
                ));
            }
        }
        
        // Get the least loaded worker
        let (worker, worker_id) = self.worker_manager.get_least_loaded_worker().await?;
        
        // Create the router
        let router_options = config.to_router_options();
        let router = worker
            .create_router(router_options)
            .await
            .map_err(|e| MediaError::RouterError(format!("Failed to create router: {e}")))?;
        
        let router_id = router.id().to_string();
        info!("Created router {} for room {} on worker {}", router_id, room_id, worker_id);
        
        // Set up router event handlers
        self.setup_router_handlers(&router, &room_id);
        
        // Store router info
        let router_info = RouterInfo {
            router,
            worker_id,
        };
        
        self.routers.write().await.insert(room_id.clone(), router_info);
        
        Ok(router_id)
    }
    
    /// Gets a router for a room
    pub async fn get_router(&self, room_id: &str) -> MediaResult<Router> {
        let routers = self.routers.read().await;

        routers
            .get(room_id)
            .map(|info| info.router.clone())
            .ok_or_else(|| MediaError::RouterError(format!("Router not found for room: {room_id}")))
    }

    /// Gets the worker ID for a room's router
    pub async fn get_worker_id(&self, room_id: &str) -> MediaResult<WorkerId> {
        let routers = self.routers.read().await;
        routers
            .get(room_id)
            .map(|info| info.worker_id)
            .ok_or_else(|| MediaError::RouterError(format!("Router not found for room: {room_id}")))
    }
    
    /// Removes a router for a room
    pub async fn remove_router(&self, room_id: &str) -> MediaResult<()> {
        let mut routers = self.routers.write().await;

        if let Some(router_info) = routers.remove(room_id) {
            // Decrement worker load
            self.worker_manager.decrement_worker_load(router_info.worker_id).await?;

            // Router is automatically closed when dropped

            info!("Removed router for room {} from worker {}", room_id, router_info.worker_id);
            Ok(())
        } else {
            Err(MediaError::RouterError(format!("Router not found for room: {room_id}")))
        }
    }
    
    /// Checks if a router exists for a room
    pub async fn has_router(&self, room_id: &str) -> bool {
        self.routers.read().await.contains_key(room_id)
    }
    
    /// Gets the number of active routers
    pub async fn router_count(&self) -> usize {
        self.routers.read().await.len()
    }
    
    /// Gets router statistics for a room
    pub async fn get_router_stats(&self, room_id: &str) -> MediaResult<RouterDump> {
        let router = self.get_router(room_id).await?;
        
        router
            .dump()
            .await
            .map_err(|e| MediaError::RouterError(format!("Failed to get router stats: {e}")))
    }
    
    /// Creates a pipe transport to connect two routers
    pub async fn create_pipe_transport(
        &self,
        room_id: &str,
        remote_ip: IpAddr,
        remote_port: u16,
    ) -> MediaResult<PipeTransport> {
        let router = self.get_router(room_id).await?;

        let options = PipeTransportOptions::new(ListenInfo {
            protocol: Protocol::Udp,
            ip: IpAddr::V4(std::net::Ipv4Addr::new(0, 0, 0, 0)),
            announced_address: None,
            port: None,
            port_range: None,
            flags: None,
            send_buffer_size: None,
            recv_buffer_size: None,
            expose_internal_ip: false,
        });

        let transport = router
            .create_pipe_transport(options)
            .await
            .map_err(|e| MediaError::TransportError(format!("Failed to create pipe transport: {e}")))?;

        // Connect to remote router
        let remote_parameters = PipeTransportRemoteParameters {
            ip: remote_ip,
            port: remote_port,
            srtp_parameters: None, // No SRTP for internal pipe transports
        };

        transport
            .connect(remote_parameters)
            .await
            .map_err(|e| MediaError::TransportError(format!("Failed to connect pipe transport: {e}")))?;

        info!("Created pipe transport for room {} to {}:{}", room_id, remote_ip, remote_port);

        Ok(transport)
    }
    
    /// Pipes a producer to another router (for server-side routing)
    pub async fn pipe_producer_to_router(
        &self,
        from_room_id: &str,
        to_room_id: &str,
        producer_id: ProducerId,
    ) -> MediaResult<Producer> {
        let from_router = self.get_router(from_room_id).await?;
        let to_router = self.get_router(to_room_id).await?;

        // Use the built-in pipe_producer_to_router method
        let pipe_result = from_router
            .pipe_producer_to_router(producer_id, PipeToRouterOptions::new(to_router))
            .await
            .map_err(|e| MediaError::RouterError(format!("Failed to pipe producer: {e}")))?;

        info!(
            "Piped producer {} from room {} to room {}",
            producer_id, from_room_id, to_room_id
        );

        Ok(pipe_result.pipe_producer.into_inner())
    }
    
    /// Sets up event handlers for a router
    fn setup_router_handlers(&self, router: &Router, room_id: &str) {
        let room_id = room_id.to_string();
        
        router.on_close({
            let room_id = room_id.clone();
            move || {
                warn!("Router closed for room: {}", room_id);
            }
        });
        
        router.on_worker_close({
            let room_id = room_id;
            move || {
                warn!("Worker closed for router in room: {}", room_id);
            }
        });
    }
    
    /// Gets all active room IDs
    pub async fn get_active_rooms(&self) -> Vec<String> {
        self.routers.read().await.keys().cloned().collect()
    }
    
    /// Closes all routers
    pub async fn close_all(&self) -> Result<()> {
        info!("Closing all routers");

        let mut routers = self.routers.write().await;

        // Routers are automatically closed when dropped
        for (room_id, _router_info) in routers.drain() {
            debug!("Closed router for room: {}", room_id);
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::config::MediaConfig;
    
    #[tokio::test]
    async fn test_router_lifecycle() {
        let config = Arc::new(MediaConfig::default());
        let worker_manager = Arc::new(WorkerManager::new(config.clone()).await.unwrap());
        let router_manager = RouterManager::new(worker_manager);
        
        // Create router
        let room_id = "test-room".to_string();
        let result = router_manager.create_router(room_id.clone(), config.router_config.clone()).await;
        assert!(result.is_ok());
        
        // Check if router exists
        assert!(router_manager.has_router(&room_id).await);
        
        // Remove router
        let result = router_manager.remove_router(&room_id).await;
        assert!(result.is_ok());
        
        // Check if router was removed
        assert!(!router_manager.has_router(&room_id).await);
    }
}
