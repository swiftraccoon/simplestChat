#![forbid(unsafe_code)]

// Room module - Room state management and participant tracking

use crate::media::{MediaServer, MediaConfig};
use crate::media::types::{MediaResult, TransportInfo};
use crate::metrics::ServerMetrics;
use crate::signaling::protocol::{ParticipantInfo, ProducerMetadata, ServerMessage};
use mediasoup::prelude::*;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::RwLock as StdRwLock;
use tokio::sync::RwLock as TokioRwLock;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};
use anyhow::Result;

/// Participant in a room
#[derive(Clone)]
pub struct Participant {
    pub id: String,
    pub name: String,
    pub sender: mpsc::Sender<Arc<String>>,
    pub producers: HashMap<String, MediaKind>,
}

/// Room state
pub struct Room {
    pub id: String,
    pub router_id: String,
    pub participants: HashMap<String, Participant>,
}

impl Room {
    fn new(id: String, router_id: String) -> Self {
        Self {
            id,
            router_id,
            participants: HashMap::new(),
        }
    }

    /// Broadcast a message to all participants except the sender
    fn broadcast_except(&self, sender_id: &str, message: &ServerMessage) {
        let json = match serde_json::to_string(message) {
            Ok(j) => Arc::new(j),
            Err(e) => {
                warn!("Failed to serialize broadcast message: {}", e);
                return;
            }
        };
        for (id, participant) in &self.participants {
            if id != sender_id {
                match participant.sender.try_send(json.clone()) {
                    Ok(()) => {}
                    Err(mpsc::error::TrySendError::Full(_)) => {
                        warn!("Channel full for participant {} in room {}, dropping message", id, self.id);
                    }
                    Err(mpsc::error::TrySendError::Closed(_)) => {
                        debug!("Channel closed for participant {} in room {} (disconnected)", id, self.id);
                    }
                }
            }
        }
    }

    /// Broadcast a message to all participants
    fn broadcast_all(&self, message: &ServerMessage) {
        let json = match serde_json::to_string(message) {
            Ok(j) => Arc::new(j),
            Err(e) => {
                warn!("Failed to serialize broadcast message: {}", e);
                return;
            }
        };
        for (id, participant) in &self.participants {
            match participant.sender.try_send(json.clone()) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(_)) => {
                    warn!("Channel full for participant {} in room {}, dropping message", id, self.id);
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    debug!("Channel closed for participant {} in room {} (disconnected)", id, self.id);
                }
            }
        }
    }
}

/// Manages all rooms and coordinates media + signaling.
///
/// Uses per-room locking: the outer HashMap is protected by a std::sync::RwLock
/// (held only for brief lookups/inserts, never across await points), while each
/// room is protected by its own tokio::sync::RwLock (held across async operations
/// but only blocking participants in that specific room).
pub struct RoomManager {
    rooms: Arc<StdRwLock<HashMap<String, Arc<TokioRwLock<Room>>>>>,
    media_server: Arc<MediaServer>,
    metrics: ServerMetrics,
}

impl RoomManager {
    /// Creates a new room manager
    ///
    /// # Errors
    /// Returns an error if media server initialization fails
    pub async fn new(media_config: MediaConfig, metrics: ServerMetrics) -> Result<Self> {
        let media_server = Arc::new(MediaServer::new(media_config).await?);

        Ok(Self {
            rooms: Arc::new(StdRwLock::new(HashMap::new())),
            media_server,
            metrics,
        })
    }

    /// Gets a room lock by ID (brief outer read lock, no await)
    fn get_room(&self, room_id: &str) -> Result<Arc<TokioRwLock<Room>>> {
        let rooms = self.rooms.read().unwrap_or_else(|e| e.into_inner());
        rooms.get(room_id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Room not found: {room_id}"))
    }

    /// Gets or creates a room, creating a router if needed
    async fn get_or_create_room(&self, room_id: &str) -> Result<Arc<TokioRwLock<Room>>> {
        // Fast path: room exists (brief outer read lock)
        {
            let rooms = self.rooms.read().unwrap_or_else(|e| e.into_inner());
            if let Some(room) = rooms.get(room_id) {
                return Ok(room.clone());
            }
        }

        // Slow path: create router (no lock held during async operation)
        info!("Creating new room: {}", room_id);
        let router_id = self.media_server.create_router(room_id.to_string()).await?;
        self.metrics.inc_rooms_created();

        // Insert under write lock (re-check for concurrent creation)
        let mut rooms = self.rooms.write().unwrap_or_else(|e| e.into_inner());
        Ok(rooms
            .entry(room_id.to_string())
            .or_insert_with(|| Arc::new(TokioRwLock::new(Room::new(room_id.to_string(), router_id))))
            .clone())
    }

    /// Adds a participant to a room (creates room if needed)
    ///
    /// # Errors
    /// Returns an error if media server operations fail
    pub async fn add_participant(
        &self,
        room_id: &str,
        participant_id: String,
        participant_name: String,
        sender: mpsc::Sender<Arc<String>>,
    ) -> Result<Vec<ParticipantInfo>> {
        let room_lock = self.get_or_create_room(room_id).await?;
        let mut room = room_lock.write().await;

        let participant = Participant {
            id: participant_id.clone(),
            name: participant_name.clone(),
            sender,
            producers: HashMap::new(),
        };

        room.participants.insert(participant_id.clone(), participant);

        info!("Participant {} ({}) joined room {}", participant_id, participant_name, room_id);

        // Notify other participants
        room.broadcast_except(&participant_id, &ServerMessage::ParticipantJoined {
            participant_id: participant_id.clone(),
            participant_name,
        });

        // Return list of existing participants
        let participants: Vec<ParticipantInfo> = room
            .participants
            .values()
            .filter(|p| p.id != participant_id)
            .map(|p| ParticipantInfo {
                id: p.id.clone(),
                name: p.name.clone(),
                producers: p.producers.iter().map(|(id, kind)| ProducerMetadata {
                    id: id.clone(),
                    kind: *kind,
                }).collect(),
            })
            .collect();

        Ok(participants)
    }

    /// Removes a participant from a room
    ///
    /// # Errors
    /// Returns an error if cleanup operations fail
    pub async fn remove_participant(&self, room_id: &str, participant_id: &str) -> Result<()> {
        let mut removed = false;
        let mut room_empty = false;

        // Get room lock (brief outer read)
        let room_lock = {
            let rooms = self.rooms.read().unwrap_or_else(|e| e.into_inner());
            match rooms.get(room_id) {
                Some(r) => r.clone(),
                None => return Ok(()),
            }
        };

        // Lock only this room
        {
            let mut room = room_lock.write().await;
            if room.participants.remove(participant_id).is_some() {
                removed = true;
                info!("Participant {} left room {}", participant_id, room_id);

                room.broadcast_all(&ServerMessage::ParticipantLeft {
                    participant_id: participant_id.to_string(),
                });

                room_empty = room.participants.is_empty();
            }
        } // Release per-room lock before outer write

        // If room is empty, remove from map
        if room_empty {
            let mut rooms = self.rooms.write().unwrap_or_else(|e| e.into_inner());
            // Re-check under write lock using try_write to avoid blocking
            if let Some(room_lock) = rooms.get(room_id) {
                if room_lock.try_write().map_or(false, |room| room.participants.is_empty()) {
                    rooms.remove(room_id);
                    info!("Room {} is empty, cleaning up", room_id);
                }
            }
        }

        if removed {
            // Close transports, producers, consumers — releases FDs
            if let Err(e) = self.media_server.transport_manager().remove_participant(participant_id).await {
                warn!("Failed to clean up media for participant {}: {}", participant_id, e);
            }

            if room_empty {
                self.media_server.remove_router(room_id).await?;
                debug!("Removed router for room {}", room_id);
            }
        }

        Ok(())
    }

    /// Gets router RTP capabilities for a room
    ///
    /// # Errors
    /// Returns an error if the room doesn't exist
    pub async fn get_router_rtp_capabilities(&self, room_id: &str) -> Result<RtpCapabilitiesFinalized> {
        let router = self.get_router(room_id).await?;
        Ok(router.rtp_capabilities().clone())
    }

    /// Creates a send transport for a participant
    ///
    /// # Errors
    /// Returns an error if transport creation fails
    pub async fn create_send_transport(
        &self,
        room_id: &str,
        participant_id: &str,
    ) -> Result<TransportInfo> {
        // No room lock needed — router lookup goes through router_manager
        let router = self.get_router(room_id).await?;
        self.media_server
            .transport_manager()
            .create_send_transport(
                participant_id.to_string(),
                &router,
                &self.media_server.config().webrtc_transport_config,
            )
            .await
            .map_err(|e| anyhow::anyhow!(e))
    }

    /// Creates a receive transport for a participant
    ///
    /// # Errors
    /// Returns an error if transport creation fails
    pub async fn create_recv_transport(
        &self,
        room_id: &str,
        participant_id: &str,
    ) -> Result<TransportInfo> {
        // No room lock needed — router lookup goes through router_manager
        let router = self.get_router(room_id).await?;
        self.media_server
            .transport_manager()
            .create_recv_transport(
                participant_id.to_string(),
                &router,
                &self.media_server.config().webrtc_transport_config,
            )
            .await
            .map_err(|e| anyhow::anyhow!(e))
    }

    /// Connects a transport with DTLS parameters
    ///
    /// # Errors
    /// Returns an error if connection fails
    pub async fn connect_transport(
        &self,
        _room_id: &str,
        participant_id: &str,
        transport_id: &str,
        dtls_parameters: DtlsParameters,
    ) -> Result<()> {
        // No room lock needed — TransportManager determines send vs recv by transport ID
        self.media_server
            .transport_manager()
            .connect_transport(participant_id, transport_id, dtls_parameters)
            .await
            .map_err(|e| anyhow::anyhow!(e))?;

        debug!("Connected transport {} for participant {}", transport_id, participant_id);
        Ok(())
    }

    /// Creates a producer for a participant
    ///
    /// # Errors
    /// Returns an error if producer creation fails
    pub async fn create_producer(
        &self,
        room_id: &str,
        participant_id: &str,
        kind: MediaKind,
        rtp_parameters: RtpParameters,
    ) -> Result<String> {
        // Create producer WITHOUT room lock
        let producer = self.media_server
            .transport_manager()
            .create_producer(
                participant_id,
                kind,
                rtp_parameters,
                AppData::default(),
            )
            .await
            .map_err(|e| anyhow::anyhow!(e))?;

        let producer_id = producer.id().to_string();

        // Store producer info and broadcast (per-room write lock)
        let room_lock = self.get_room(room_id)?;
        let mut room = room_lock.write().await;
        if let Some(participant) = room.participants.get_mut(participant_id) {
            participant.producers.insert(producer_id.clone(), kind);
        }

        room.broadcast_except(participant_id, &ServerMessage::NewProducer {
            participant_id: participant_id.to_string(),
            producer_id: producer_id.clone(),
            kind,
        });

        info!("Created {:?} producer {} for participant {} in room {}",
              kind, producer_id, participant_id, room_id);

        Ok(producer_id)
    }

    /// Creates a consumer for a participant to receive media
    ///
    /// # Errors
    /// Returns an error if consumer creation fails
    pub async fn create_consumer(
        &self,
        room_id: &str,
        participant_id: &str,
        producer_id: ProducerId,
        rtp_capabilities: RtpCapabilities,
        sender: Option<mpsc::Sender<Arc<String>>>,
    ) -> MediaResult<crate::media::types::ConsumerInfo> {
        // No room lock needed — purely transport_manager operation
        let consumer = self.media_server
            .transport_manager()
            .create_consumer(
                participant_id,
                producer_id,
                rtp_capabilities,
                AppData::default(),
                sender,
            )
            .await?;

        let consumer_info = crate::media::types::ConsumerInfo::from_consumer(&consumer, false);

        debug!("Created consumer {} for participant {} in room {}",
               consumer_info.id, participant_id, room_id);

        Ok(consumer_info)
    }

    /// Resumes a consumer for a participant
    pub async fn resume_consumer(
        &self,
        _room_id: &str,
        participant_id: &str,
        consumer_id: &str,
    ) -> Result<()> {
        // No room lock needed — purely transport_manager operation
        self.media_server
            .transport_manager()
            .resume_consumer(participant_id, consumer_id)
            .await
            .map_err(|e| anyhow::anyhow!(e))?;

        debug!("Resumed consumer {} for participant {}", consumer_id, participant_id);
        Ok(())
    }

    /// Pauses a consumer for a participant
    pub async fn pause_consumer(
        &self,
        _room_id: &str,
        participant_id: &str,
        consumer_id: &str,
    ) -> Result<()> {
        // No room lock needed — purely transport_manager operation
        self.media_server
            .transport_manager()
            .pause_consumer(participant_id, consumer_id)
            .await
            .map_err(|e| anyhow::anyhow!(e))?;

        debug!("Paused consumer {} for participant {}", consumer_id, participant_id);
        Ok(())
    }

    /// Closes a producer for a participant
    pub async fn close_producer(
        &self,
        room_id: &str,
        participant_id: &str,
        producer_id: &str,
    ) -> Result<()> {
        // Close producer WITHOUT room lock
        self.media_server
            .transport_manager()
            .close_producer(participant_id, producer_id)
            .await
            .map_err(|e| anyhow::anyhow!(e))?;

        // Remove from room's participant tracking and notify others (per-room write lock)
        let room_lock = self.get_room(room_id)?;
        let mut room = room_lock.write().await;
        if let Some(participant) = room.participants.get_mut(participant_id) {
            participant.producers.remove(producer_id);
        }

        room.broadcast_except(participant_id, &ServerMessage::ProducerClosed {
            producer_id: producer_id.to_string(),
        });

        info!("Closed producer {} for participant {} in room {}", producer_id, participant_id, room_id);
        Ok(())
    }

    /// Sets preferred simulcast layers for a consumer
    pub async fn set_preferred_layers(
        &self,
        participant_id: &str,
        consumer_id: &str,
        spatial_layer: u8,
        temporal_layer: Option<u8>,
    ) -> Result<()> {
        self.media_server
            .transport_manager()
            .set_preferred_layers(participant_id, consumer_id, ConsumerLayers {
                spatial_layer,
                temporal_layer,
            })
            .await
            .map_err(|e| anyhow::anyhow!(e))
    }

    /// Restarts ICE on a transport, returning new ICE parameters
    pub async fn restart_ice(
        &self,
        participant_id: &str,
        transport_id: &str,
    ) -> Result<IceParameters> {
        self.media_server
            .transport_manager()
            .restart_ice(participant_id, transport_id)
            .await
            .map_err(|e| anyhow::anyhow!(e))
    }

    /// Gets recv transport stats for bandwidth estimation
    pub async fn get_recv_transport_stats(
        &self,
        participant_id: &str,
    ) -> Result<Option<(u32, Vec<String>)>> {
        self.media_server
            .transport_manager()
            .get_recv_transport_stats(participant_id)
            .await
            .map_err(|e| anyhow::anyhow!(e))
    }

    /// Gets send transport stats for connection quality reporting
    pub async fn get_send_transport_stats(
        &self,
        participant_id: &str,
    ) -> Result<Option<Option<u32>>> {
        self.media_server
            .transport_manager()
            .get_send_transport_stats(participant_id)
            .await
            .map_err(|e| anyhow::anyhow!(e))
    }

    /// Rebinds a participant's WS sender after reconnection
    pub async fn rebind_participant_sender(
        &self,
        room_id: &str,
        participant_id: &str,
        new_sender: mpsc::Sender<Arc<String>>,
    ) -> Result<bool> {
        let room_lock = self.get_room(room_id)?;
        let mut room = room_lock.write().await;
        if let Some(participant) = room.participants.get_mut(participant_id) {
            participant.sender = new_sender;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Broadcasts a chat message to all participants in a room except the sender
    pub async fn broadcast_chat(&self, room_id: &str, sender_id: &str, content: String) -> Result<()> {
        let room_lock = self.get_room(room_id)?;
        let room = room_lock.read().await;

        let sender_name = room.participants.get(sender_id)
            .map(|p| p.name.clone())
            .unwrap_or_else(|| "Unknown".to_string());

        room.broadcast_except(sender_id, &ServerMessage::ChatReceived {
            participant_id: sender_id.to_string(),
            participant_name: sender_name,
            content,
        });

        Ok(())
    }

    /// Gets the router for a room (from media server)
    async fn get_router(&self, room_id: &str) -> Result<Router> {
        self.media_server
            .router_manager()
            .get_router(room_id)
            .await
            .map_err(|e| anyhow::anyhow!(e))
    }

    /// Gracefully shuts down all rooms: removes all participants, closes transports, removes routers.
    pub async fn shutdown(&self) {
        info!("Shutting down all rooms...");

        // Drain all rooms from the map
        let all_rooms: Vec<(String, Arc<TokioRwLock<Room>>)> = {
            let mut rooms = self.rooms.write().unwrap_or_else(|e| e.into_inner());
            rooms.drain().collect()
        };

        for (room_id, room_lock) in &all_rooms {
            let participant_ids: Vec<String> = {
                let room = room_lock.read().await;
                room.participants.keys().cloned().collect()
            };

            for pid in &participant_ids {
                if let Err(e) = self.media_server.transport_manager().remove_participant(pid).await {
                    warn!("Failed to clean up media for participant {} during shutdown: {}", pid, e);
                }
            }

            self.media_server.remove_router(room_id).await.ok();
            info!("Shut down room {} ({} participants)", room_id, participant_ids.len());
        }

        info!("All rooms shut down ({} total)", all_rooms.len());
    }

    /// Gets current room count
    pub async fn room_count(&self) -> usize {
        self.rooms.read().unwrap_or_else(|e| e.into_inner()).len()
    }

    /// Gets total participant count across all rooms
    pub async fn total_participant_count(&self) -> usize {
        let room_locks: Vec<Arc<TokioRwLock<Room>>> = {
            let rooms = self.rooms.read().unwrap_or_else(|e| e.into_inner());
            rooms.values().cloned().collect()
        };

        let mut total = 0;
        for room_lock in room_locks {
            if let Ok(room) = room_lock.try_read() {
                total += room.participants.len();
            }
        }
        total
    }
}
