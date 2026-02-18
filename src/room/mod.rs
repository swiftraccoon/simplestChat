#![forbid(unsafe_code)]

// Room module - Room state management and participant tracking
pub mod api;
pub mod moderation;
pub mod roles;
pub mod settings;

use crate::media::{MediaServer, MediaConfig};
use crate::media::types::{MediaResult, TransportInfo};
use crate::metrics::ServerMetrics;
use crate::signaling::protocol::{AudioLevelEntry, ParticipantInfo, ProducerMetadata, ServerMessage};
use mediasoup::active_speaker_observer::{ActiveSpeakerObserver, ActiveSpeakerObserverOptions};
use mediasoup::audio_level_observer::{AudioLevelObserver, AudioLevelObserverOptions};
use mediasoup::prelude::*;
use mediasoup::producer::ProducerId;
use mediasoup::rtp_observer::{RtpObserver, RtpObserverAddProducerOptions};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Weak};
use std::sync::RwLock as StdRwLock;
use tokio::sync::RwLock as TokioRwLock;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};
use anyhow::Result;

/// Events sent from observer callbacks (sync Fn) to async broadcast task
enum ObserverEvent {
    ActiveSpeaker { producer_id: ProducerId },
    AudioLevels { volumes: Vec<(ProducerId, i8)> },
}

/// Participant in a room
#[derive(Clone)]
pub struct Participant {
    pub id: String,
    pub name: String,
    pub sender: mpsc::Sender<Arc<String>>,
    pub producers: HashMap<String, (MediaKind, Option<String>)>,
    pub role: roles::Role,
    pub punitive: moderation::PunitiveState,
}

/// Entry for a participant waiting in the lobby
pub struct LobbyEntry {
    pub participant_id: String,
    pub name: String,
    pub sender: mpsc::Sender<Arc<String>>,
    pub authenticated: bool,
    /// Reconnect token for grace period (set by connection handler after lobby entry)
    pub reconnect_token: String,
}

/// Result of an add_participant attempt
pub enum JoinResult {
    /// Successfully joined the room
    Joined(Vec<ParticipantInfo>),
    /// Placed in the lobby awaiting moderator approval
    Lobbied,
}

/// Room state
pub struct Room {
    pub id: String,
    pub router_id: String,
    pub participants: HashMap<String, Participant>,
    pub settings: Option<settings::RoomSettings>,
    active_speaker_observer: Option<ActiveSpeakerObserver>,
    audio_level_observer: Option<AudioLevelObserver>,
    /// Map producer_id -> participant_id for observer lookups
    producer_to_participant: HashMap<String, String>,
    /// In-memory ban list — prevents banned participants from rejoining
    banned_participants: HashSet<String>,
    /// Lobby: participants waiting for moderator approval
    pub lobby: HashMap<String, LobbyEntry>,
}

impl Room {
    fn new(id: String, router_id: String, settings: Option<settings::RoomSettings>) -> Self {
        Self {
            id,
            router_id,
            participants: HashMap::new(),
            settings,
            active_speaker_observer: None,
            audio_level_observer: None,
            producer_to_participant: HashMap::new(),
            banned_participants: HashSet::new(),
            lobby: HashMap::new(),
        }
    }

    fn new_with_observers(
        id: String,
        router_id: String,
        settings: Option<settings::RoomSettings>,
        active_speaker_observer: Option<ActiveSpeakerObserver>,
        audio_level_observer: Option<AudioLevelObserver>,
    ) -> Self {
        Self {
            id,
            router_id,
            participants: HashMap::new(),
            settings,
            active_speaker_observer,
            audio_level_observer,
            producer_to_participant: HashMap::new(),
            banned_participants: HashSet::new(),
            lobby: HashMap::new(),
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

    /// Send a message to a specific participant
    fn send_to(&self, participant_id: &str, message: &ServerMessage) {
        let json = match serde_json::to_string(message) {
            Ok(j) => Arc::new(j),
            Err(e) => {
                warn!("Failed to serialize message: {}", e);
                return;
            }
        };
        if let Some(participant) = self.participants.get(participant_id) {
            match participant.sender.try_send(json) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(_)) => {
                    warn!("Channel full for participant {} in room {}, dropping message", participant_id, self.id);
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    debug!("Channel closed for participant {} in room {} (disconnected)", participant_id, self.id);
                }
            }
        }
    }

    /// Broadcast a message to all participants with role >= min_role
    fn broadcast_to_role(&self, min_role: roles::Role, message: &ServerMessage) {
        let json = match serde_json::to_string(message) {
            Ok(j) => Arc::new(j),
            Err(e) => {
                warn!("Failed to serialize broadcast message: {}", e);
                return;
            }
        };
        for (id, participant) in &self.participants {
            if participant.role >= min_role {
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
    db_pool: Option<sqlx::PgPool>,
}

impl RoomManager {
    /// Creates a new room manager
    ///
    /// # Errors
    /// Returns an error if media server initialization fails
    pub async fn new(media_config: MediaConfig, metrics: ServerMetrics, db_pool: Option<sqlx::PgPool>) -> Result<Self> {
        let media_server = Arc::new(MediaServer::new(media_config).await?);

        Ok(Self {
            rooms: Arc::new(StdRwLock::new(HashMap::new())),
            media_server,
            metrics,
            db_pool,
        })
    }

    /// Gets the media server for direct access (e.g., find_producer_paused)
    pub fn media_server(&self) -> &MediaServer {
        &self.media_server
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

        // Load settings from DB if available
        let room_settings = if let Some(pool) = &self.db_pool {
            settings::load_room(pool, room_id).await.ok().flatten()
        } else {
            None
        };

        // Slow path: create router (no lock held during async operation)
        info!("Creating new room: {}", room_id);
        let router_id = self.media_server.create_router(room_id.to_string()).await?;
        self.metrics.inc_rooms_created();

        // Re-check under write lock before setting up observers (handles concurrent creation)
        {
            let rooms = self.rooms.read().unwrap_or_else(|e| e.into_inner());
            if let Some(existing) = rooms.get(room_id) {
                return Ok(existing.clone());
            }
        }

        // Get the Router object to create observers on
        let router = self.media_server.router_manager().get_router(room_id).await
            .map_err(|e| anyhow::anyhow!(e))?;

        // Create bounded channel for observer events (observer events are ephemeral UI hints)
        let (observer_tx, observer_rx) = tokio::sync::mpsc::channel::<ObserverEvent>(16);

        // Create active speaker observer (default interval=300ms)
        let active_speaker_observer = match router
            .create_active_speaker_observer(ActiveSpeakerObserverOptions::default())
            .await
        {
            Ok(obs) => Some(obs),
            Err(e) => {
                warn!("Failed to create active speaker observer for room {}: {}", room_id, e);
                None
            }
        };

        // Create audio level observer
        let audio_level_observer = {
            let mut opts = AudioLevelObserverOptions::default();
            opts.max_entries = std::num::NonZeroU16::new(10).unwrap();
            opts.threshold = -50;
            opts.interval = 800; // ms - don't send too frequently
            match router.create_audio_level_observer(opts).await {
                Ok(obs) => Some(obs),
                Err(e) => {
                    warn!("Failed to create audio level observer for room {}: {}", room_id, e);
                    None
                }
            }
        };

        // Set up callbacks (use try_send — dropping stale events is fine)
        if let Some(obs) = &active_speaker_observer {
            let tx = observer_tx.clone();
            obs.on_dominant_speaker(move |speaker| {
                let _ = tx.try_send(ObserverEvent::ActiveSpeaker {
                    producer_id: speaker.producer.id(),
                });
            });
        }

        if let Some(obs) = &audio_level_observer {
            let tx = observer_tx.clone();
            obs.on_volumes(move |volumes| {
                let entries: Vec<_> = volumes
                    .iter()
                    .map(|v| (v.producer.id(), v.volume))
                    .collect();
                let _ = tx.try_send(ObserverEvent::AudioLevels { volumes: entries });
            });
        }
        drop(observer_tx); // Only clones in callbacks remain

        // Insert under write lock (re-check for concurrent creation)
        let room_arc = {
            let mut rooms = self.rooms.write().unwrap_or_else(|e| e.into_inner());
            if let Some(existing) = rooms.get(room_id) {
                return Ok(existing.clone());
            }
            let new_room = Arc::new(TokioRwLock::new(Room::new_with_observers(
                room_id.to_string(),
                router_id,
                room_settings,
                active_speaker_observer,
                audio_level_observer,
            )));
            rooms.insert(room_id.to_string(), new_room.clone());
            new_room
        };

        // Spawn background task with Weak reference (only for newly created rooms)
        let weak_room = Arc::downgrade(&room_arc);
        tokio::spawn(Self::observer_broadcast_task(observer_rx, weak_room));

        Ok(room_arc)
    }

    /// Background task that reads observer events and broadcasts to room participants.
    /// Uses a Weak reference so the task exits when the room is dropped.
    async fn observer_broadcast_task(
        mut rx: tokio::sync::mpsc::Receiver<ObserverEvent>,
        weak_room: Weak<TokioRwLock<Room>>,
    ) {
        while let Some(event) = rx.recv().await {
            let room_arc = match weak_room.upgrade() {
                Some(r) => r,
                None => break, // Room is gone
            };

            let room = room_arc.read().await;
            match event {
                ObserverEvent::ActiveSpeaker { producer_id } => {
                    if let Some(participant_id) = room.producer_to_participant.get(&producer_id.to_string()) {
                        room.broadcast_all(&ServerMessage::ActiveSpeaker {
                            participant_id: participant_id.clone(),
                        });
                    }
                }
                ObserverEvent::AudioLevels { volumes } => {
                    let levels: Vec<AudioLevelEntry> = volumes
                        .iter()
                        .filter_map(|(pid, vol)| {
                            room.producer_to_participant
                                .get(&pid.to_string())
                                .map(|participant_id| AudioLevelEntry {
                                    participant_id: participant_id.clone(),
                                    volume: *vol,
                                })
                        })
                        .collect();
                    if !levels.is_empty() {
                        room.broadcast_all(&ServerMessage::AudioLevels { levels });
                    }
                }
            }
        }
    }

    /// Adds a participant to a room (creates room if needed)
    ///
    /// If the room has `lobby_enabled` and the participant's role would be < Moderator
    /// (and they are not the first participant), they are placed in the lobby instead.
    ///
    /// # Errors
    /// Returns an error if media server operations fail
    pub async fn add_participant(
        &self,
        room_id: &str,
        participant_id: String,
        participant_name: String,
        sender: mpsc::Sender<Arc<String>>,
        authenticated: bool,
    ) -> Result<JoinResult> {
        let room_lock = self.get_or_create_room(room_id).await?;

        // Brief read lock to check conditions for role resolution
        let (lobby_enabled, settings_owner_id) = {
            let room = room_lock.read().await;
            let lobby = room.settings.as_ref().map_or(false, |s| s.lobby_enabled);
            let owner = room.settings.as_ref().map(|s| s.owner_id);
            (lobby, owner)
        }; // read lock released

        // Resolve role outside any lock when lobby is enabled (DB call may await)
        let resolved_role = if lobby_enabled {
            if let (Some(pool), Some(owner_id)) = (&self.db_pool, settings_owner_id) {
                if let Ok(uid) = participant_id.parse::<uuid::Uuid>() {
                    Some(roles::resolve_role(pool, room_id, Some(&uid), &owner_id, authenticated).await)
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        };

        // Now acquire write lock for mutation
        let mut room = room_lock.write().await;

        // Check ban list before allowing join
        if room.banned_participants.contains(&participant_id) {
            anyhow::bail!("You are banned from this room");
        }

        // Enforce room modes
        if let Some(settings) = &room.settings {
            if settings.require_registration && !authenticated {
                anyhow::bail!("This room requires registration");
            }
            if !settings.guests_allowed && !authenticated {
                anyhow::bail!("Guests are not allowed in this room");
            }
            if let Some(max) = settings.max_participants {
                if room.participants.len() >= max as usize {
                    anyhow::bail!("Room is full");
                }
            }
        }

        // Re-check is_first under write lock (another join could have raced)
        let is_first = room.participants.is_empty() && room.lobby.is_empty();

        // Determine final role
        let role = if is_first {
            roles::Role::Owner
        } else if let Some(db_role) = resolved_role {
            db_role
        } else if authenticated {
            roles::Role::User
        } else {
            roles::Role::Guest
        };

        // Check lobby: if lobby_enabled AND not the first participant AND role < Moderator
        if lobby_enabled && !is_first && role < roles::Role::Moderator {
            // Place in lobby
            let room_name = room.settings.as_ref()
                .map_or_else(|| room.id.clone(), |s| s.display_name.clone());
            let topic = room.settings.as_ref().and_then(|s| s.topic.clone());
            let participant_count = room.participants.len() as u32;

            // Send LobbyWaiting to the participant
            let lobby_waiting = ServerMessage::LobbyWaiting {
                room_name,
                topic,
                participant_count,
            };
            if let Ok(json) = serde_json::to_string(&lobby_waiting) {
                let _ = sender.try_send(Arc::new(json));
            }

            // Broadcast LobbyJoin to Moderator+ participants
            room.broadcast_to_role(roles::Role::Moderator, &ServerMessage::LobbyJoin {
                participant_id: participant_id.clone(),
                display_name: participant_name.clone(),
                authenticated,
            });

            // Add to lobby map (reconnect_token set later by connection handler)
            room.lobby.insert(participant_id.clone(), LobbyEntry {
                participant_id: participant_id.clone(),
                name: participant_name.clone(),
                sender,
                authenticated,
                reconnect_token: String::new(),
            });

            info!("Participant {} ({}) entered lobby for room {}", participant_id, participant_name, room_id);
            return Ok(JoinResult::Lobbied);
        }

        let participant = Participant {
            id: participant_id.clone(),
            name: participant_name.clone(),
            sender,
            producers: HashMap::new(),
            role,
            punitive: moderation::PunitiveState::default(),
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
                producers: p.producers.iter().map(|(id, (kind, source))| ProducerMetadata {
                    id: id.clone(),
                    kind: *kind,
                    source: source.clone(),
                }).collect(),
            })
            .collect();

        Ok(JoinResult::Joined(participants))
    }

    /// Removes a participant from a room
    ///
    /// # Errors
    /// Returns an error if cleanup operations fail
    pub async fn remove_participant(&self, room_id: &str, participant_id: &str) -> Result<()> {
        let mut removed = false;
        let mut room_empty = false;
        let mut audio_producer_ids: Vec<String> = Vec::new();
        let mut active_obs = None;
        let mut audio_obs = None;

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

            // Also remove from lobby if present (lobby participants have no media to clean up)
            if room.lobby.remove(participant_id).is_some() {
                info!("Lobby participant {} removed from room {}", participant_id, room_id);
                room_empty = room.participants.is_empty() && room.lobby.is_empty();
                // No media cleanup needed for lobby participants
            }

            if let Some(participant) = room.participants.remove(participant_id) {
                removed = true;
                info!("Participant {} left room {}", participant_id, room_id);

                // Collect audio producer IDs for observer cleanup
                for (pid, (kind, _)) in &participant.producers {
                    if *kind == MediaKind::Audio {
                        audio_producer_ids.push(pid.clone());
                    }
                }

                // Remove from producer_to_participant map
                for pid in &audio_producer_ids {
                    room.producer_to_participant.remove(pid);
                }

                // Clone observers before releasing lock
                active_obs = room.active_speaker_observer.clone();
                audio_obs = room.audio_level_observer.clone();

                room.broadcast_all(&ServerMessage::ParticipantLeft {
                    participant_id: participant_id.to_string(),
                });

                room_empty = room.participants.is_empty() && room.lobby.is_empty();
            }
        } // Release per-room lock before outer write

        // Remove audio producers from observers OUTSIDE lock
        for pid_str in &audio_producer_ids {
            if let Ok(pid) = pid_str.parse::<ProducerId>() {
                if let Some(obs) = &active_obs {
                    let _ = obs.remove_producer(pid).await;
                }
                if let Some(obs) = &audio_obs {
                    let _ = obs.remove_producer(pid).await;
                }
            }
        }

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
        let webrtc_server = self.media_server.get_webrtc_server_for_room(room_id).await?;
        self.media_server
            .transport_manager()
            .create_send_transport(
                participant_id.to_string(),
                &router,
                webrtc_server,
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
        let webrtc_server = self.media_server.get_webrtc_server_for_room(room_id).await?;
        self.media_server
            .transport_manager()
            .create_recv_transport(
                participant_id.to_string(),
                &router,
                webrtc_server,
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
        source: Option<String>,
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
        let producer_id_typed = producer.id();

        // Store producer info, broadcast, and clone observers (per-room write lock)
        let (active_obs, audio_obs) = {
            let room_lock = self.get_room(room_id)?;
            let mut room = room_lock.write().await;
            if let Some(participant) = room.participants.get_mut(participant_id) {
                participant.producers.insert(producer_id.clone(), (kind, source.clone()));
            }
            if kind == MediaKind::Audio {
                room.producer_to_participant
                    .insert(producer_id.clone(), participant_id.to_string());
            }

            room.broadcast_except(participant_id, &ServerMessage::NewProducer {
                participant_id: participant_id.to_string(),
                producer_id: producer_id.clone(),
                kind,
                source,
            });

            // Clone observers before releasing lock
            (room.active_speaker_observer.clone(), room.audio_level_observer.clone())
        }; // room lock released

        // Add to observers OUTSIDE lock (async IPC)
        if kind == MediaKind::Audio {
            if let Some(obs) = &active_obs {
                let _ = obs
                    .add_producer(RtpObserverAddProducerOptions::new(producer_id_typed))
                    .await;
            }
            if let Some(obs) = &audio_obs {
                let _ = obs
                    .add_producer(RtpObserverAddProducerOptions::new(producer_id_typed))
                    .await;
            }
        }

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
        paused: bool,
    ) -> MediaResult<crate::media::types::ConsumerInfo> {
        // Look up the consumer counter for this room's worker (for load-aware tracking)
        let consumer_counter = self.media_server
            .get_consumer_counter_for_room(room_id).await
            .ok()
            .flatten();

        // No room lock needed — purely transport_manager operation
        let consumer = self.media_server
            .transport_manager()
            .create_consumer(
                participant_id,
                producer_id,
                rtp_capabilities,
                AppData::default(),
                sender,
                paused,
                consumer_counter,
            )
            .await?;

        let consumer_info = crate::media::types::ConsumerInfo::from_consumer(&consumer, paused);

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

        // Remove from room's participant tracking, observer maps, and notify others (per-room write lock)
        let (active_obs, audio_obs, was_audio) = {
            let room_lock = self.get_room(room_id)?;
            let mut room = room_lock.write().await;
            if let Some(participant) = room.participants.get_mut(participant_id) {
                participant.producers.remove(producer_id);
            }
            let was_audio = room.producer_to_participant.remove(producer_id).is_some();

            room.broadcast_except(participant_id, &ServerMessage::ProducerClosed {
                producer_id: producer_id.to_string(),
            });

            // Clone observers before releasing lock
            (room.active_speaker_observer.clone(), room.audio_level_observer.clone(), was_audio)
        }; // room lock released

        // Remove from observers OUTSIDE lock
        if was_audio {
            if let Ok(pid) = producer_id.parse::<ProducerId>() {
                if let Some(obs) = &active_obs {
                    let _ = obs.remove_producer(pid).await;
                }
                if let Some(obs) = &audio_obs {
                    let _ = obs.remove_producer(pid).await;
                }
            }
        }

        info!("Closed producer {} for participant {} in room {}", producer_id, participant_id, room_id);
        Ok(())
    }

    /// Pauses a producer and cascades pause to all consumers of that producer
    pub async fn pause_producer(
        &self,
        room_id: &str,
        participant_id: &str,
        producer_id: &str,
    ) -> Result<()> {
        // Pause the producer itself
        self.media_server
            .transport_manager()
            .pause_producer(participant_id, producer_id)
            .await
            .map_err(|e| anyhow::anyhow!(e))?;

        // Cascade: pause all consumers of this producer
        let count = self.media_server
            .transport_manager()
            .pause_consumers_of_producer(producer_id)
            .await
            .map_err(|e| anyhow::anyhow!(e))?;

        // Broadcast ProducerPaused to other participants in the room
        let room_lock = self.get_room(room_id)?;
        let room = room_lock.read().await;
        room.broadcast_except(participant_id, &ServerMessage::ProducerPaused {
            producer_id: producer_id.to_string(),
        });

        info!("Paused producer {} for participant {} in room {} (cascaded to {} consumers)",
              producer_id, participant_id, room_id, count);
        Ok(())
    }

    /// Resumes a producer and cascades resume to all consumers of that producer
    pub async fn resume_producer(
        &self,
        room_id: &str,
        participant_id: &str,
        producer_id: &str,
    ) -> Result<()> {
        // Resume the producer itself
        self.media_server
            .transport_manager()
            .resume_producer(participant_id, producer_id)
            .await
            .map_err(|e| anyhow::anyhow!(e))?;

        // Cascade: resume all consumers of this producer
        let count = self.media_server
            .transport_manager()
            .resume_consumers_of_producer(producer_id)
            .await
            .map_err(|e| anyhow::anyhow!(e))?;

        // Broadcast ProducerResumed to other participants in the room
        let room_lock = self.get_room(room_id)?;
        let room = room_lock.read().await;
        room.broadcast_except(participant_id, &ServerMessage::ProducerResumed {
            producer_id: producer_id.to_string(),
        });

        info!("Resumed producer {} for participant {} in room {} (cascaded to {} consumers)",
              producer_id, participant_id, room_id, count);
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

    /// Subscribes to BWE events for a participant's recv transport
    pub async fn subscribe_bwe_events(
        &self,
        participant_id: &str,
        bwe_sender: mpsc::Sender<u32>,
    ) -> Result<()> {
        self.media_server
            .transport_manager()
            .subscribe_bwe_events(participant_id, bwe_sender)
            .await
            .map_err(|e| anyhow::anyhow!(e))
    }

    /// Gets consumer IDs for a participant (no IPC — in-memory only)
    pub async fn get_consumer_ids(&self, participant_id: &str) -> Result<Vec<String>> {
        self.media_server
            .transport_manager()
            .get_consumer_ids(participant_id)
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

    // === Room settings methods ===

    /// Update room settings at runtime (Admin+ only).
    ///
    /// Applies partial updates to the in-memory RoomSettings, broadcasts the change
    /// to all participants, and optionally persists to DB.
    #[allow(clippy::too_many_arguments)]
    pub async fn update_room_settings(
        &self,
        room_id: &str,
        admin_id: &str,
        moderated: Option<bool>,
        lobby_enabled: Option<bool>,
        guests_allowed: Option<bool>,
        guests_can_broadcast: Option<bool>,
        max_broadcasters: Option<Option<i32>>,
        allow_screen_sharing: Option<bool>,
        allow_chat: Option<bool>,
        push_to_talk: Option<bool>,
        secret: Option<bool>,
        password: Option<Option<String>>,
    ) -> Result<()> {
        // Snapshot settings for broadcast + DB persistence (release lock before DB call)
        let settings_snapshot = {
            let room_lock = self.get_room(room_id)?;
            let mut room = room_lock.write().await;

            // Verify admin exists and has permission
            let admin = room.participants.get(admin_id)
                .ok_or_else(|| anyhow::anyhow!("Participant not found"))?;
            if !admin.role.can_change_settings() {
                anyhow::bail!("Insufficient permissions to change room settings (requires Admin+)");
            }

            // Create default settings if none exist yet (ephemeral rooms without DB)
            if room.settings.is_none() {
                room.settings = Some(settings::RoomSettings {
                    id: room_id.to_string(),
                    owner_id: uuid::Uuid::nil(),
                    display_name: room_id.to_string(),
                    password_protected: false,
                    require_registration: false,
                    max_participants: None,
                    max_broadcasters: None,
                    allow_screen_sharing: true,
                    allow_chat: true,
                    allow_video: true,
                    moderated: false,
                    invite_only: false,
                    secret: false,
                    lobby_enabled: false,
                    push_to_talk: false,
                    guests_allowed: true,
                    guests_can_broadcast: true,
                    topic: None,
                });
            }

            // Apply partial updates
            let s = room.settings.as_mut().unwrap();
            settings::apply_settings_update(
                s,
                moderated,
                lobby_enabled,
                guests_allowed,
                guests_can_broadcast,
                max_broadcasters,
                allow_screen_sharing,
                allow_chat,
                push_to_talk,
                secret,
                password.clone(),
            );

            // Serialize settings for broadcast
            let settings_value = serde_json::to_value(s)
                .unwrap_or_else(|_| serde_json::Value::Null);

            // Broadcast to all participants
            room.broadcast_all(&ServerMessage::RoomSettingsChanged {
                settings: settings_value.clone(),
            });

            info!("Room settings updated for room {} by {}", room_id, admin_id);
            settings_value
        }; // room lock released

        // Persist to DB outside lock (rare operation, OK to be async)
        if let Some(pool) = &self.db_pool {
            // Convert password Option<Option<String>> to password_hash Option<Option<String>>
            // For now, store plaintext — hashing is the API layer's responsibility
            let password_hash = password;
            if let Err(e) = settings::update_room_settings(
                pool,
                room_id,
                moderated,
                lobby_enabled,
                guests_allowed,
                guests_can_broadcast,
                max_broadcasters,
                allow_screen_sharing,
                allow_chat,
                push_to_talk,
                secret,
                password_hash,
            ).await {
                warn!("Failed to persist room settings to DB for room {}: {}", room_id, e);
            }
        }

        let _ = settings_snapshot; // suppress unused warning
        Ok(())
    }

    /// Set the room topic (Admin+ only).
    ///
    /// Updates the in-memory topic and broadcasts TopicChanged to all participants.
    pub async fn set_topic(&self, room_id: &str, admin_id: &str, topic: String) -> Result<()> {
        {
            let room_lock = self.get_room(room_id)?;
            let mut room = room_lock.write().await;

            // Verify admin exists and has permission
            let admin = room.participants.get(admin_id)
                .ok_or_else(|| anyhow::anyhow!("Participant not found"))?;
            if !admin.role.can_change_settings() {
                anyhow::bail!("Insufficient permissions to change room topic (requires Admin+)");
            }

            // Update topic in settings (create default settings if none exist)
            if let Some(s) = room.settings.as_mut() {
                s.topic = Some(topic.clone());
            } else {
                let mut default_settings = settings::RoomSettings {
                    id: room_id.to_string(),
                    owner_id: uuid::Uuid::nil(),
                    display_name: room_id.to_string(),
                    password_protected: false,
                    require_registration: false,
                    max_participants: None,
                    max_broadcasters: None,
                    allow_screen_sharing: true,
                    allow_chat: true,
                    allow_video: true,
                    moderated: false,
                    invite_only: false,
                    secret: false,
                    lobby_enabled: false,
                    push_to_talk: false,
                    guests_allowed: true,
                    guests_can_broadcast: true,
                    topic: Some(topic.clone()),
                };
                default_settings.topic = Some(topic.clone());
                room.settings = Some(default_settings);
            }

            // Broadcast topic change
            room.broadcast_all(&ServerMessage::TopicChanged {
                topic: topic.clone(),
                changed_by: admin_id.to_string(),
            });

            info!("Topic set for room {} by {}: {}", room_id, admin_id, topic);
        } // room lock released

        // Persist topic to DB
        if let Some(pool) = &self.db_pool {
            if let Err(e) = sqlx::query("UPDATE rooms SET topic = $1 WHERE id = $2")
                .bind(&topic)
                .bind(room_id)
                .execute(pool)
                .await
            {
                warn!("Failed to persist topic to DB for room {}: {}", room_id, e);
            }
        }

        Ok(())
    }

    // === Moderation methods ===

    /// Force-close all video/screen producers for a target participant
    pub async fn close_cam(
        &self,
        room_id: &str,
        moderator_id: &str,
        target_participant_id: &str,
    ) -> Result<()> {
        // Collect producer IDs to close (under room lock)
        let producers_to_close: Vec<String> = {
            let room_lock = self.get_room(room_id)?;
            let room = room_lock.read().await;

            let moderator = room.participants.get(moderator_id)
                .ok_or_else(|| anyhow::anyhow!("Moderator not found"))?;
            let target = room.participants.get(target_participant_id)
                .ok_or_else(|| anyhow::anyhow!("Target participant not found"))?;

            if !moderator.role.can_moderate(target.role) {
                anyhow::bail!("Insufficient permissions to moderate this participant");
            }

            // Collect video producers (camera and screen)
            target.producers.iter()
                .filter(|(_, (kind, _))| *kind == MediaKind::Video)
                .map(|(id, _)| id.clone())
                .collect()
        };

        // Close producers outside room lock (async mediasoup IPC)
        for producer_id in &producers_to_close {
            if let Err(e) = self.media_server
                .transport_manager()
                .close_producer(target_participant_id, producer_id)
                .await
            {
                warn!("Failed to close producer {} for close_cam: {}", producer_id, e);
            }
        }

        // Remove from room tracking and broadcast
        {
            let room_lock = self.get_room(room_id)?;
            let mut room = room_lock.write().await;
            if let Some(target) = room.participants.get_mut(target_participant_id) {
                for pid in &producers_to_close {
                    target.producers.remove(pid);
                }
            }
            for pid in &producers_to_close {
                room.producer_to_participant.remove(pid);
                room.broadcast_all(&ServerMessage::ForceClosedProducer {
                    producer_id: pid.clone(),
                    reason: "Closed by moderator".to_string(),
                });
                room.broadcast_all(&ServerMessage::ProducerClosed {
                    producer_id: pid.clone(),
                });
            }
        }

        info!("close_cam: {} closed {} video producers for {} in room {}",
              moderator_id, producers_to_close.len(), target_participant_id, room_id);
        Ok(())
    }

    /// Ban a participant from producing video/screen (close existing + prevent future)
    pub async fn cam_ban(
        &self,
        room_id: &str,
        moderator_id: &str,
        target_participant_id: &str,
        _reason: Option<&str>,
    ) -> Result<()> {
        // First close existing cam producers (permission check inside)
        self.close_cam(room_id, moderator_id, target_participant_id).await?;

        // Re-check permissions and set flag atomically (guards against TOCTOU role demotion)
        let room_lock = self.get_room(room_id)?;
        let mut room = room_lock.write().await;

        let moderator = room.participants.get(moderator_id)
            .ok_or_else(|| anyhow::anyhow!("Moderator not found"))?;
        let target = room.participants.get(target_participant_id)
            .ok_or_else(|| anyhow::anyhow!("Target participant not found"))?;

        if !moderator.role.can_moderate(target.role) {
            anyhow::bail!("Insufficient permissions (role changed during operation)");
        }

        let target = room.participants.get_mut(target_participant_id).unwrap();
        target.punitive.cam_banned = true;

        room.broadcast_all(&ServerMessage::CamBanned {
            participant_id: target_participant_id.to_string(),
        });

        info!("cam_ban: {} cam-banned {} in room {}", moderator_id, target_participant_id, room_id);
        Ok(())
    }

    /// Remove cam ban from a participant
    pub async fn cam_unban(
        &self,
        room_id: &str,
        moderator_id: &str,
        target_participant_id: &str,
    ) -> Result<()> {
        let room_lock = self.get_room(room_id)?;
        let mut room = room_lock.write().await;

        let moderator = room.participants.get(moderator_id)
            .ok_or_else(|| anyhow::anyhow!("Moderator not found"))?;
        let target = room.participants.get(target_participant_id)
            .ok_or_else(|| anyhow::anyhow!("Target participant not found"))?;

        if !moderator.role.can_moderate(target.role) {
            anyhow::bail!("Insufficient permissions to moderate this participant");
        }

        // Need to re-borrow mutably after immutable borrows
        let target = room.participants.get_mut(target_participant_id).unwrap();
        target.punitive.cam_banned = false;

        room.broadcast_all(&ServerMessage::CamUnbanned {
            participant_id: target_participant_id.to_string(),
        });

        info!("cam_unban: {} cam-unbanned {} in room {}", moderator_id, target_participant_id, room_id);
        Ok(())
    }

    /// Mute a participant's text chat
    pub async fn text_mute(
        &self,
        room_id: &str,
        moderator_id: &str,
        target_participant_id: &str,
    ) -> Result<()> {
        let room_lock = self.get_room(room_id)?;
        let mut room = room_lock.write().await;

        let moderator = room.participants.get(moderator_id)
            .ok_or_else(|| anyhow::anyhow!("Moderator not found"))?;
        let target = room.participants.get(target_participant_id)
            .ok_or_else(|| anyhow::anyhow!("Target participant not found"))?;

        if !moderator.role.can_moderate(target.role) {
            anyhow::bail!("Insufficient permissions to moderate this participant");
        }

        let target = room.participants.get_mut(target_participant_id).unwrap();
        target.punitive.text_muted = true;

        room.broadcast_all(&ServerMessage::TextMuted {
            participant_id: target_participant_id.to_string(),
        });

        info!("text_mute: {} text-muted {} in room {}", moderator_id, target_participant_id, room_id);
        Ok(())
    }

    /// Unmute a participant's text chat
    pub async fn text_unmute(
        &self,
        room_id: &str,
        moderator_id: &str,
        target_participant_id: &str,
    ) -> Result<()> {
        let room_lock = self.get_room(room_id)?;
        let mut room = room_lock.write().await;

        let moderator = room.participants.get(moderator_id)
            .ok_or_else(|| anyhow::anyhow!("Moderator not found"))?;
        let target = room.participants.get(target_participant_id)
            .ok_or_else(|| anyhow::anyhow!("Target participant not found"))?;

        if !moderator.role.can_moderate(target.role) {
            anyhow::bail!("Insufficient permissions to moderate this participant");
        }

        let target = room.participants.get_mut(target_participant_id).unwrap();
        target.punitive.text_muted = false;

        room.broadcast_all(&ServerMessage::TextUnmuted {
            participant_id: target_participant_id.to_string(),
        });

        info!("text_unmute: {} text-unmuted {} in room {}", moderator_id, target_participant_id, room_id);
        Ok(())
    }

    /// Kick a participant from the room
    pub async fn kick_participant(
        &self,
        room_id: &str,
        moderator_id: &str,
        target_participant_id: &str,
        reason: Option<&str>,
    ) -> Result<()> {
        // Check permissions and remove under room lock
        {
            let room_lock = self.get_room(room_id)?;
            let mut room = room_lock.write().await;

            let moderator = room.participants.get(moderator_id)
                .ok_or_else(|| anyhow::anyhow!("Moderator not found"))?;
            let target = room.participants.get(target_participant_id)
                .ok_or_else(|| anyhow::anyhow!("Target participant not found"))?;

            if !moderator.role.can_moderate(target.role) {
                anyhow::bail!("Insufficient permissions to kick this participant");
            }

            // Broadcast kick to everyone (including the target) before removing
            room.broadcast_all(&ServerMessage::ParticipantKicked {
                participant_id: target_participant_id.to_string(),
                reason: reason.map(String::from),
            });

            room.participants.remove(target_participant_id);

            // Broadcast leave to remaining participants
            room.broadcast_all(&ServerMessage::ParticipantLeft {
                participant_id: target_participant_id.to_string(),
            });
        }

        // Clean up media outside lock
        if let Err(e) = self.media_server.transport_manager().remove_participant(target_participant_id).await {
            warn!("Failed to clean up media for kicked participant {}: {}", target_participant_id, e);
        }

        info!("kick: {} kicked {} from room {}", moderator_id, target_participant_id, room_id);
        Ok(())
    }

    /// Ban a participant from the room (kick + add to ban list, in-memory only for now)
    pub async fn ban_participant(
        &self,
        room_id: &str,
        moderator_id: &str,
        target_participant_id: &str,
        reason: Option<&str>,
        _duration: Option<u64>,
    ) -> Result<()> {
        // Check permissions, add to ban list, and remove — all under one write lock
        {
            let room_lock = self.get_room(room_id)?;
            let mut room = room_lock.write().await;

            let moderator = room.participants.get(moderator_id)
                .ok_or_else(|| anyhow::anyhow!("Moderator not found"))?;
            let target = room.participants.get(target_participant_id)
                .ok_or_else(|| anyhow::anyhow!("Target participant not found"))?;

            if !moderator.role.can_moderate(target.role) {
                anyhow::bail!("Insufficient permissions to ban this participant");
            }

            // Add to ban list — prevents rejoining
            room.banned_participants.insert(target_participant_id.to_string());

            // Broadcast ban to everyone (including the target) before removing
            room.broadcast_all(&ServerMessage::ParticipantBanned {
                participant_id: target_participant_id.to_string(),
                reason: reason.map(String::from),
            });

            room.participants.remove(target_participant_id);

            // Broadcast leave to remaining participants
            room.broadcast_all(&ServerMessage::ParticipantLeft {
                participant_id: target_participant_id.to_string(),
            });
        }

        // Clean up media outside lock
        if let Err(e) = self.media_server.transport_manager().remove_participant(target_participant_id).await {
            warn!("Failed to clean up media for banned participant {}: {}", target_participant_id, e);
        }

        info!("ban: {} banned {} from room {}", moderator_id, target_participant_id, room_id);
        Ok(())
    }

    /// Remove a participant from the room's ban list
    pub async fn unban_participant(
        &self,
        room_id: &str,
        moderator_id: &str,
        target_participant_id: &str,
    ) -> Result<()> {
        let room_lock = self.get_room(room_id)?;
        let mut room = room_lock.write().await;

        let moderator = room.participants.get(moderator_id)
            .ok_or_else(|| anyhow::anyhow!("Moderator not found"))?;

        if moderator.role < roles::Role::Admin {
            anyhow::bail!("Insufficient permissions to unban (requires Admin+)");
        }

        if !room.banned_participants.remove(target_participant_id) {
            anyhow::bail!("Participant is not banned");
        }

        info!("unban: {} unbanned {} from room {}", moderator_id, target_participant_id, room_id);
        Ok(())
    }

    /// Set a participant's role
    pub async fn set_participant_role(
        &self,
        room_id: &str,
        setter_id: &str,
        target_participant_id: &str,
        new_role: roles::Role,
    ) -> Result<()> {
        let room_lock = self.get_room(room_id)?;
        let mut room = room_lock.write().await;

        let setter = room.participants.get(setter_id)
            .ok_or_else(|| anyhow::anyhow!("Setter not found"))?;
        let target = room.participants.get(target_participant_id)
            .ok_or_else(|| anyhow::anyhow!("Target participant not found"))?;

        if !setter.role.can_set_role(target.role, new_role) {
            anyhow::bail!("Insufficient permissions to set this role");
        }

        let setter_name = setter.id.clone();

        let target = room.participants.get_mut(target_participant_id).unwrap();
        target.role = new_role;

        room.broadcast_all(&ServerMessage::RoleChanged {
            participant_id: target_participant_id.to_string(),
            new_role: new_role.name().to_string(),
            granted_by: setter_name,
        });

        info!("set_role: {} set {} to {:?} in room {}", setter_id, target_participant_id, new_role, room_id);
        Ok(())
    }

    /// Request voice in a moderated room (broadcasts to Moderator+ participants)
    pub async fn request_voice(
        &self,
        room_id: &str,
        participant_id: &str,
    ) -> Result<()> {
        let room_lock = self.get_room(room_id)?;
        let room = room_lock.read().await;

        let participant = room.participants.get(participant_id)
            .ok_or_else(|| anyhow::anyhow!("Participant not found"))?;

        let display_name = participant.name.clone();

        room.broadcast_to_role(roles::Role::Moderator, &ServerMessage::VoiceRequested {
            participant_id: participant_id.to_string(),
            display_name,
        });

        info!("request_voice: {} requested voice in room {}", participant_id, room_id);
        Ok(())
    }

    // === Lobby methods ===

    /// Admit a participant from the lobby into the room.
    ///
    /// Moves the target from `room.lobby` to `room.participants`, sends
    /// `LobbyAdmitted` + `RoomJoined` to the admitted participant via their
    /// stored sender, and broadcasts `ParticipantJoined` to existing participants.
    pub async fn admit_from_lobby(
        &self,
        room_id: &str,
        moderator_id: &str,
        target_id: &str,
    ) -> Result<()> {
        let room_lock = self.get_room(room_id)?;
        let mut room = room_lock.write().await;

        // Verify moderator exists and has permission
        let moderator = room.participants.get(moderator_id)
            .ok_or_else(|| anyhow::anyhow!("Moderator not found"))?;
        if !moderator.role.can_admit_lobby() {
            anyhow::bail!("Insufficient permissions to admit from lobby");
        }

        // Remove from lobby
        let entry = room.lobby.remove(target_id)
            .ok_or_else(|| anyhow::anyhow!("Participant not found in lobby"))?;

        // Build the participants list (before inserting the new one)
        let participants: Vec<ParticipantInfo> = room
            .participants
            .values()
            .map(|p| ParticipantInfo {
                id: p.id.clone(),
                name: p.name.clone(),
                producers: p.producers.iter().map(|(id, (kind, source))| ProducerMetadata {
                    id: id.clone(),
                    kind: *kind,
                    source: source.clone(),
                }).collect(),
            })
            .collect();

        // Insert into participants with Role::User
        let participant = Participant {
            id: entry.participant_id.clone(),
            name: entry.name.clone(),
            sender: entry.sender.clone(),
            producers: HashMap::new(),
            role: roles::Role::User,
            punitive: moderation::PunitiveState::default(),
        };
        room.participants.insert(entry.participant_id.clone(), participant);

        // Check if the lobby participant's connection is still alive
        if entry.sender.is_closed() {
            warn!("Lobby participant {} already disconnected, cannot admit", target_id);
            anyhow::bail!("Participant has already disconnected");
        }

        // Send LobbyAdmitted to the admitted participant
        if let Ok(json) = serde_json::to_string(&ServerMessage::LobbyAdmitted) {
            let _ = entry.sender.try_send(Arc::new(json));
        }

        // Send RoomJoined to the admitted participant (use stored reconnect token)
        let reconnect_token = if entry.reconnect_token.is_empty() {
            uuid::Uuid::new_v4().to_string()
        } else {
            entry.reconnect_token.clone()
        };
        if let Ok(json) = serde_json::to_string(&ServerMessage::RoomJoined {
            participant_id: entry.participant_id.clone(),
            participants,
            reconnect_token,
        }) {
            let _ = entry.sender.try_send(Arc::new(json));
        }

        // Broadcast ParticipantJoined to existing participants (excluding the admitted one)
        room.broadcast_except(&entry.participant_id, &ServerMessage::ParticipantJoined {
            participant_id: entry.participant_id.clone(),
            participant_name: entry.name.clone(),
        });

        info!("admit_from_lobby: {} admitted {} to room {}",
              moderator_id, target_id, room_id);
        Ok(())
    }

    /// Deny a participant from the lobby.
    ///
    /// Removes the target from `room.lobby` and sends `LobbyDenied` to them.
    pub async fn deny_from_lobby(
        &self,
        room_id: &str,
        moderator_id: &str,
        target_id: &str,
        reason: Option<String>,
    ) -> Result<()> {
        let room_lock = self.get_room(room_id)?;
        let mut room = room_lock.write().await;

        // Verify moderator exists and has permission
        let moderator = room.participants.get(moderator_id)
            .ok_or_else(|| anyhow::anyhow!("Moderator not found"))?;
        if !moderator.role.can_admit_lobby() {
            anyhow::bail!("Insufficient permissions to deny from lobby");
        }

        // Remove from lobby
        let entry = room.lobby.remove(target_id)
            .ok_or_else(|| anyhow::anyhow!("Participant not found in lobby"))?;

        // Send LobbyDenied to the denied participant
        if let Ok(json) = serde_json::to_string(&ServerMessage::LobbyDenied { reason }) {
            let _ = entry.sender.try_send(Arc::new(json));
        }

        info!("deny_from_lobby: {} denied {} from room {}",
              moderator_id, target_id, room_id);
        Ok(())
    }

    /// Store a reconnect token in a lobby entry (called by connection handler after lobby join)
    pub async fn store_lobby_reconnect_token(&self, room_id: &str, participant_id: &str, token: &str) -> Result<()> {
        let room_lock = self.get_room(room_id)?;
        let mut room = room_lock.write().await;
        if let Some(entry) = room.lobby.get_mut(participant_id) {
            entry.reconnect_token = token.to_string();
        }
        Ok(())
    }

    /// Check if a participant can produce a given source type
    pub async fn can_participant_produce(&self, room_id: &str, participant_id: &str, source: &str) -> Result<bool> {
        let room_lock = self.get_room(room_id)?;
        let room = room_lock.read().await;
        let participant = room.participants.get(participant_id)
            .ok_or_else(|| anyhow::anyhow!("Participant not found"))?;

        // Settings-based produce restrictions
        if let Some(settings) = &room.settings {
            // Check allow_screen_sharing
            if !settings.allow_screen_sharing && (source == "screen" || source == "screen-audio") {
                return Ok(false);
            }
            // Check allow_video
            if !settings.allow_video && source == "camera" {
                return Ok(false);
            }
            // Check max_broadcasters
            if let Some(max) = settings.max_broadcasters {
                let broadcaster_count = room.participants.values()
                    .filter(|p| !p.producers.is_empty())
                    .count();
                // Allow if this participant already has producers
                let already_broadcasting = room.participants.get(participant_id)
                    .map_or(false, |p| !p.producers.is_empty());
                if !already_broadcasting && broadcaster_count >= max as usize {
                    return Ok(false);
                }
            }
        }

        let moderated = room.settings.as_ref().map_or(false, |s| s.moderated);
        Ok(moderation::can_produce(&participant.punitive, participant.role, moderated, source))
    }

    /// Check if a participant can chat
    pub async fn can_participant_chat(&self, room_id: &str, participant_id: &str) -> Result<bool> {
        let room_lock = self.get_room(room_id)?;
        let room = room_lock.read().await;
        let participant = room.participants.get(participant_id)
            .ok_or_else(|| anyhow::anyhow!("Participant not found"))?;
        let moderated = room.settings.as_ref().map_or(false, |s| s.moderated);
        Ok(moderation::can_chat(&participant.punitive, participant.role, moderated))
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

    /// Gets participant count for a specific room (non-async, brief read lock)
    pub fn participant_count_for_room(&self, room_id: &str) -> usize {
        let rooms = self.rooms.read().unwrap_or_else(|e| e.into_inner());
        rooms.get(room_id)
            .and_then(|lock| lock.try_read().ok())
            .map(|room| room.participants.len())
            .unwrap_or(0)
    }
}
