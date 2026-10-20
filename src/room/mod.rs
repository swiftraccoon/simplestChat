#![forbid(unsafe_code)]

// Room module - Room state management and participant tracking
pub(crate) mod administration;
pub(crate) mod api;
pub(crate) mod community;
mod control;
pub(crate) mod history;
pub(crate) mod invites;
mod manager;
pub(crate) mod moderation;
mod privacy;
mod receive_replacement;
pub(crate) mod refusal;
pub(crate) mod roles;
pub(crate) mod settings;
pub use settings::{RoomSettings, SettingsPatch};
pub(crate) mod social;
mod window;

#[cfg(test)]
mod broadcaster_ceiling_tests;
#[cfg(test)]
mod control_tests;
#[cfg(test)]
mod departure_broadcast_tests;
#[cfg(test)]
mod dormant_join_tests;
#[cfg(test)]
mod security_tests;
#[cfg(test)]
mod settings_patch_tests;

use crate::diagnostics::{Stage, measure, measure_result};
use crate::ids::{ParticipantId, RoomId, UserId};
use crate::media::types::{MediaResult, TransportInfo};
use crate::media::{MediaConfig, MediaServer};
use crate::metrics::ServerMetrics;
use crate::shutdown::DrainSignal;
use crate::signaling::protocol::{
    AudioLevelEntry, ChatStyle, ParticipantInfo, ProducerMetadata, ServerMessage,
};
use crate::sync::{Mutex, RwLock};
use anyhow::Result;
use mediasoup::active_speaker_observer::{ActiveSpeakerObserver, ActiveSpeakerObserverOptions};
use mediasoup::audio_level_observer::{AudioLevelObserver, AudioLevelObserverOptions};
use mediasoup::prelude::*;
use mediasoup::producer::ProducerId;
use mediasoup::rtp_observer::{RtpObserver, RtpObserverAddProducerOptions};
use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::Hash;
use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use tokio::sync::RwLock as TokioRwLock;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

/// Events sent from observer callbacks (sync Fn) to async broadcast task
enum ObserverEvent {
    ActiveSpeaker {
        producer_id: ProducerId,
    },
    AudioLevels {
        volumes: Vec<(ProducerId, i8)>,
    },
    /// No producer exceeded the level threshold during the last interval.
    Silence,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RevokedProducer {
    participant_id: ParticipantId,
    media_session_id: uuid::Uuid,
    producer_id: ProducerId,
    kind: MediaKind,
}

/// A producer a participant publishes: its media kind and the source the client
/// declared for it (camera, screen, …) when it named one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProducerRecord {
    pub kind: MediaKind,
    pub source: Option<String>,
}

/// Participant in a room
#[derive(Clone)]
pub(crate) struct Participant {
    pub(crate) social: social::ParticipantSocial,
    pub id: ParticipantId,
    pub name: String,
    pub sender: mpsc::Sender<crate::OutboundJson>,
    /// Unique to this room membership, even when `id` is a stable account UUID.
    /// Media cleanup must use this value so an old socket cannot tear down a
    /// replacement session that joined with the same participant ID.
    pub media_session_id: uuid::Uuid,
    pub producers: HashMap<ProducerId, ProducerRecord>,
    pub role: roles::Role,
    pub punitive: moderation::PunitiveState,
    /// True when the connection carried a valid JWT (id == user uuid)
    pub authenticated: bool,
    /// Client IP (from ConnectInfo / X-Forwarded-For) — used for guest bans
    pub ip: Option<std::net::IpAddr>,
    /// How this participant's name and messages look to everyone.
    pub chat_style: ChatStyle,
}

/// Entry for a participant waiting in the lobby
pub(crate) struct LobbyEntry {
    pub participant_id: ParticipantId,
    pub name: String,
    pub sender: mpsc::Sender<crate::OutboundJson>,
    /// Carried into the room on admission.
    pub chat_style: ChatStyle,
    /// Preserved when the lobby entry is admitted into the room.
    pub media_session_id: uuid::Uuid,
    pub authenticated: bool,
    /// Reconnect token for grace period (set by connection handler after lobby entry)
    pub reconnect_token: String,
    /// Shared with the participant's connection task — cleared on admission so the
    /// connection stops rejecting media/moderation messages with the lobby guard.
    pub in_lobby_flag: Arc<AtomicBool>,
    /// Client IP — carried into Participant on admission
    pub ip: Option<IpAddr>,
    /// Role and sanctions are resolved before entering the lobby. Admission must
    /// not silently replace either value with a more permissive default.
    pub role: roles::Role,
    pub punitive: moderation::PunitiveState,
}

/// A sanction needs an identity that outlives an individual WebSocket. Account
/// UUIDs are stable for registered users, while guest sanctions are scoped to
/// the best durable identity available to the room (their IP address).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum SanctionKey {
    User(String),
    GuestIp(IpAddr),
    Connection(String),
}

impl SanctionKey {
    fn for_identity(
        participant_id: &ParticipantId,
        authenticated: bool,
        ip: Option<IpAddr>,
    ) -> Self {
        if authenticated {
            Self::User(participant_id.to_string())
        } else if let Some(ip) = ip {
            Self::GuestIp(crate::net::cohort(ip))
        } else {
            Self::Connection(participant_id.to_string())
        }
    }
}

/// Result of an add_participant attempt
pub(crate) enum JoinResult {
    /// Successfully joined the room — includes participant list, role, and room settings
    Joined {
        participants: Vec<ParticipantInfo>,
        role: roles::Role,
        /// Boxed so the lobby outcome does not pay for the settings it lacks.
        room_settings: Option<Box<settings::RoomSettings>>,
        /// The joiner's look: the account's saved one, or the guest's.
        chat_style: ChatStyle,
        /// The name the room knows the joiner by: a guest's may carry a number.
        name: String,
    },
    /// Placed in the lobby awaiting moderator approval
    Lobbied,
}

/// A public join challenge, kept distinct from internal room failures.
#[derive(Debug, thiserror::Error)]
#[error("This room requires a password")]
pub(crate) struct RoomPasswordRequired;

pub(crate) enum DeleteRoomResult {
    Deleted,
    NotFound,
    Forbidden,
}

const ROOM_DELETE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
const ROOM_CREATION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
const ROOM_PASSWORD_HASH_COOLDOWN: std::time::Duration = std::time::Duration::from_secs(30);
const PASSWORD_VERIFY_QUEUE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);
const ROOM_CHAT_WINDOW: std::time::Duration = std::time::Duration::from_secs(1);
const MAX_ROOM_CHAT_MESSAGES_PER_WINDOW: usize = 10;
const ROOM_MEDIA_MUTATION_WINDOW: std::time::Duration = std::time::Duration::from_secs(1);
const MAX_ROOM_MEDIA_MUTATIONS_PER_WINDOW: usize = 20;
// Consumer and transport requests are cheap to send but cross the mediasoup
// worker boundary. Per-session limits alone still scale with room membership,
// so retain a room-wide ceiling independent of the configured per-viewer cap.
const ROOM_MEDIA_CONTROL_IPC_WINDOW: std::time::Duration = std::time::Duration::from_secs(1);
const MAX_ROOM_MEDIA_CONTROL_IPC_PER_WINDOW: usize = 2_048;
const ROOM_VOICE_REQUEST_WINDOW: std::time::Duration = std::time::Duration::from_secs(10);
const MAX_ROOM_VOICE_REQUESTS_PER_WINDOW: usize = 5;
const ROOM_ADMIN_MUTATION_WINDOW: std::time::Duration = std::time::Duration::from_secs(1);
const MAX_ROOM_ADMIN_MUTATIONS_PER_WINDOW: usize = 20;
const JOIN_RATE_WINDOW: std::time::Duration = std::time::Duration::from_secs(60);
const MAX_TRACKED_JOIN_KEYS: usize = 10_000;

/// The room manager's operator settings, taken from [`crate::config::Config`].
/// The defaults are those of an empty environment; fixtures start from them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ManagerSettings {
    /// `ALLOW_AD_HOC_ROOMS`: a loopback listener can still be internet-facing
    /// through a reverse proxy, so ephemeral room creation is an explicit opt-in.
    pub allow_ad_hoc_rooms: bool,
    /// `MAX_PARTICIPANTS_PER_ROOM`.
    pub max_participants_per_room: Option<usize>,
    /// `MAX_BROADCASTERS_PER_ROOM`.
    pub max_broadcasters_per_room: Option<usize>,
    /// Concurrent Argon2 verifications for joins; an unset
    /// `MAX_PASSWORD_WORKERS` keeps two.
    pub password_verify_workers: usize,
    /// `MAX_ROOMS`.
    pub max_rooms: usize,
    /// `MAX_PERSISTED_ROOMS`.
    pub max_persisted_rooms: i64,
    /// `JOIN_ATTEMPTS_PER_IP_PER_MINUTE`: a school or an office behind one
    /// address raises it.
    pub join_attempts_per_ip_per_minute: u32,
    /// `JOIN_ATTEMPTS_PER_ROOM_IP_PER_MINUTE`.
    pub join_attempts_per_room_ip_per_minute: u32,
    /// Account update subscribers, bounded like connections (`MAX_CONNECTIONS`).
    pub account_update_capacity: usize,
}

impl Default for ManagerSettings {
    fn default() -> Self {
        Self {
            allow_ad_hoc_rooms: false,
            max_participants_per_room: None,
            max_broadcasters_per_room: None,
            password_verify_workers: 2,
            max_rooms: 1_000,
            max_persisted_rooms: 10_000,
            join_attempts_per_ip_per_minute: 30,
            join_attempts_per_room_ip_per_minute: 10,
            account_update_capacity: 10_000,
        }
    }
}

impl From<&crate::config::Config> for ManagerSettings {
    fn from(config: &crate::config::Config) -> Self {
        Self {
            allow_ad_hoc_rooms: config.rooms.allow_ad_hoc,
            max_participants_per_room: config.limits.max_participants_per_room,
            max_broadcasters_per_room: config.limits.max_broadcasters_per_room,
            password_verify_workers: config.limits.max_password_workers.unwrap_or(2),
            max_rooms: config.limits.max_rooms,
            max_persisted_rooms: config.limits.max_persisted_rooms,
            join_attempts_per_ip_per_minute: config.rates.join_attempts_per_ip_per_minute,
            join_attempts_per_room_ip_per_minute: config.rates.join_attempts_per_room_ip_per_minute,
            account_update_capacity: config.limits.max_connections,
        }
    }
}

#[derive(Debug)]
struct RateWindow {
    started_at: std::time::Instant,
    attempts: u32,
}

/// Windows plus their start stamps in start order. Reclaiming expired windows
/// from the front is amortized O(1); a window reset in place pushes a fresh
/// stamp, and the stale front item is skipped when its stamp no longer
/// matches, so a hot key never blocks reclamation behind it.
struct RateTable<K> {
    entries: HashMap<K, RateWindow>,
    order: std::collections::VecDeque<(K, std::time::Instant)>,
    overflow: Option<RateWindow>,
}

impl<K> RateTable<K>
where
    K: Eq + Hash + Clone,
{
    fn reclaim_expired(&mut self, now: std::time::Instant) {
        while let Some((key, stamp)) = self.order.front().cloned() {
            match self.entries.get(&key) {
                Some(window) if window.started_at != stamp => {
                    self.order.pop_front();
                }
                Some(window) if now.duration_since(window.started_at) >= JOIN_RATE_WINDOW => {
                    self.order.pop_front();
                    self.entries.remove(&key);
                }
                Some(_) => break,
                None => {
                    self.order.pop_front();
                }
            }
        }
    }
}

#[derive(Clone)]
struct SharedRateLimiter<K> {
    table: Arc<Mutex<RateTable<K>>>,
    max_attempts: u32,
}

impl<K> SharedRateLimiter<K>
where
    K: Eq + Hash + Clone,
{
    fn new(max_attempts: u32) -> Self {
        Self {
            table: Arc::new(Mutex::new(RateTable {
                entries: HashMap::new(),
                order: std::collections::VecDeque::new(),
                overflow: None,
            })),
            max_attempts,
        }
    }

    fn allow(&self, key: K, now: std::time::Instant) -> bool {
        let mut table = self.table.lock();
        table.reclaim_expired(now);
        if table.entries.len() >= MAX_TRACKED_JOIN_KEYS && !table.entries.contains_key(&key) {
            // Share a bounded overflow allowance; never renew a live identity's
            // allowance by evicting its state to accommodate a new identity.
            let overflow = table.overflow.get_or_insert(RateWindow {
                started_at: now,
                attempts: 0,
            });
            if now.duration_since(overflow.started_at) >= JOIN_RATE_WINDOW {
                overflow.started_at = now;
                overflow.attempts = 0;
            }
            if overflow.attempts >= self.max_attempts {
                return false;
            }
            overflow.attempts += 1;
            return true;
        }

        let fresh_window = match table.entries.get_mut(&key) {
            Some(entry) if now.duration_since(entry.started_at) >= JOIN_RATE_WINDOW => {
                entry.started_at = now;
                entry.attempts = 0;
                true
            }
            Some(_) => false,
            None => {
                table.entries.insert(
                    key.clone(),
                    RateWindow {
                        started_at: now,
                        attempts: 0,
                    },
                );
                true
            }
        };
        if fresh_window {
            table.order.push_back((key.clone(), now));
        }
        let entry = table.entries.get_mut(&key).expect("window inserted above");
        if entry.attempts >= self.max_attempts {
            return false;
        }
        entry.attempts += 1;
        true
    }

    #[cfg(test)]
    fn tracked_keys(&self) -> usize {
        self.table.lock().entries.len()
    }
}

fn room_delete_timeout(operation: &str) -> refusal::RoomError {
    sqlx::Error::Protocol(format!("room deletion timed out while {operation}")).into()
}

fn room_creation_timeout(operation: &str) -> refusal::RoomError {
    sqlx::Error::Protocol(format!("room creation timed out while {operation}")).into()
}

pub(crate) struct IdentityUpdateGuard {
    updates: Arc<RwLock<HashSet<RoomId>>>,
    room_id: RoomId,
}

impl Drop for IdentityUpdateGuard {
    fn drop(&mut self) {
        self.updates.write().remove(&self.room_id);
    }
}

fn release_deletion_reservation(
    deleting_rooms: &RwLock<HashMap<RoomId, uuid::Uuid>>,
    room_id: &RoomId,
    token: uuid::Uuid,
) {
    let mut deleting = deleting_rooms.write();
    if deleting.get(room_id) == Some(&token) {
        deleting.remove(room_id);
    }
}

fn release_deletion_reservation_after_router_teardown(
    deleting_rooms: &RwLock<HashMap<RoomId, uuid::Uuid>>,
    room_id: &RoomId,
    token: uuid::Uuid,
    router_teardown_succeeded: bool,
) {
    if router_teardown_succeeded {
        release_deletion_reservation(deleting_rooms, room_id, token);
    }
}

async fn restore_failed_deletion(
    runtime_room: Option<Arc<TokioRwLock<Room>>>,
    deleting_rooms: Arc<RwLock<HashMap<RoomId, uuid::Uuid>>>,
    room_id: RoomId,
    token: uuid::Uuid,
) {
    if let Some(runtime_room) = runtime_room {
        let delayed_room = runtime_room.clone();
        match tokio::time::timeout(ROOM_DELETE_TIMEOUT, runtime_room.write()).await {
            Ok(mut room) => {
                room.deleting = false;
                room.policy_revision = room.policy_revision.wrapping_add(1);
            }
            Err(_) => {
                // Keep the ID reserved until rollback completes, but do not
                // hold the global creation mutex or the HTTP task indefinitely.
                tokio::spawn(async move {
                    let mut room = delayed_room.write().await;
                    room.deleting = false;
                    room.policy_revision = room.policy_revision.wrapping_add(1);
                    drop(room);
                    release_deletion_reservation(&deleting_rooms, &room_id, token);
                });
                return;
            }
        }
    }
    release_deletion_reservation(&deleting_rooms, &room_id, token);
}

fn select_join_role(
    persisted: bool,
    is_first: bool,
    resolved_role: Option<roles::Role>,
    authenticated: bool,
) -> Option<roles::Role> {
    if persisted {
        resolved_role
    } else if is_first {
        Some(roles::Role::Owner)
    } else if authenticated {
        Some(roles::Role::User)
    } else {
        Some(roles::Role::Guest)
    }
}

fn merge_punitive_state(
    mut resolved: moderation::PunitiveState,
    in_memory: Option<&moderation::PunitiveState>,
) -> moderation::PunitiveState {
    if let Some(in_memory) = in_memory {
        resolved.cam_banned |= in_memory.cam_banned;
        resolved.text_muted |= in_memory.text_muted;
    }
    resolved
}

fn policy_snapshot_matches(room: &Room, revision: u64) -> bool {
    !room.deleting && room.policy_revision == revision
}

/// Record essential enqueue rejection attempts without changing retry semantics.
/// Successful enqueue is not a successful socket write; the writer owns that count.
fn try_send_essential(
    metrics: &ServerMetrics,
    sender: &mpsc::Sender<crate::OutboundJson>,
    json: crate::OutboundJson,
) -> std::result::Result<(), mpsc::error::TrySendError<crate::OutboundJson>> {
    let result = sender.try_send(json);
    match &result {
        Ok(()) => {}
        Err(mpsc::error::TrySendError::Full(_)) => {
            metrics.inc_outbound_queue_full();
            crate::signaling::outbound::request_resync(sender);
        }
        Err(mpsc::error::TrySendError::Closed(_)) => metrics.inc_outbound_queue_closed(),
    }
    result
}

/// Departure notifications are best-effort roster updates, not direct replies.
/// Keep their payload lazy so an entirely disconnected cohort needs no message
/// allocation. Selected recipients still use normal enqueue failure accounting.
fn broadcast_departure<'a>(
    metrics: &ServerMetrics,
    senders: impl Iterator<Item = &'a mpsc::Sender<crate::OutboundJson>>,
    payload: impl FnOnce() -> serde_json::Result<String>,
) {
    // Grace memberships can outlive their signaling receiver. A closed channel
    // cannot reopen; a reconnect replaces the sender under the same room lock.
    // Check lazily before constructing the shared payload or cloning it for a
    // recipient. A later close race still reaches try_send_essential below.
    let mut senders = senders.filter(|sender| !sender.is_closed()).peekable();
    if senders.peek().is_none() {
        return;
    }
    let json = match payload() {
        Ok(json) => crate::OutboundJson::from(json),
        Err(error) => {
            warn!("Failed to serialize participant departure: {}", error);
            return;
        }
    };
    for sender in senders {
        let _ = try_send_essential(metrics, sender, json.clone());
    }
}

/// Room state
pub(crate) struct Room {
    /// Serializes policy and membership commits independently of chat/media
    /// state access. Always acquire this before the main room write lock.
    control: Arc<tokio::sync::Mutex<()>>,
    /// Shared process counters; runtime rooms must not keep isolated counters.
    metrics: ServerMetrics,
    pub(crate) social: social::RoomSocial,
    pub id: RoomId,
    pub participants: HashMap<ParticipantId, Participant>,
    pub settings: Option<settings::RoomSettings>,
    /// The deployment's broadcaster ceiling, applied beneath the room's own limit.
    pub(crate) broadcaster_ceiling: Option<usize>,
    /// Whether this room is backed by a database row. Runtime settings on an
    /// ad-hoc room must never make it look persisted (and grant the first
    /// connection ownership of an existing room).
    persisted: bool,
    /// Cached Argon2 hash. Keeping this with the loaded settings makes the
    /// password decision atomic from the room's point of view and also lets
    /// ad-hoc rooms enforce passwords set at runtime.
    password_hash: Option<String>,
    /// Reserve expensive password changes before hashing so reconnecting or
    /// parallel administrators cannot repeatedly consume the hash pool.
    last_password_hash_at: Option<std::time::Instant>,
    /// Chat fan-out is O(room size). Bound aggregate broadcasts in addition to
    /// the per-connection limit enforced by the signaling task.
    chat_broadcasts: window::SlidingWindow,
    /// Producer mutations cross mediasoup IPC and fan out room-wide. A shared
    /// room budget prevents many individually compliant sockets from restoring
    /// the original amplification attack.
    media_mutations: window::SlidingWindow,
    /// Consumer and transport operations cross the mediasoup worker boundary.
    /// Bound their aggregate so a large open room cannot multiply each
    /// participant's compliant per-session refill into unbounded worker IPC.
    media_control_ipc: window::SlidingWindow,
    /// Voice requests create persistent action UI for moderators, so bound the
    /// aggregate as well as applying the per-connection cooldown.
    voice_requests: window::SlidingWindow,
    /// Authorized moderation and settings changes can write the database and
    /// fan out room-wide. Bound their aggregate across moderator sockets.
    admin_mutations: window::SlidingWindow,
    active_speaker_observer: Option<ActiveSpeakerObserver>,
    audio_level_observer: Option<AudioLevelObserver>,
    /// Map producer_id -> participant_id for observer lookups
    producer_to_participant: HashMap<ProducerId, ParticipantId>,
    /// In-memory ban list — prevents banned participants from rejoining
    banned_participants: HashMap<ParticipantId, Option<std::time::Instant>>,
    /// Guest identities are connection-scoped, so retain their IP as the
    /// in-memory ban key (registered users are always keyed by user UUID).
    banned_guest_ips: HashMap<std::net::IpAddr, Option<std::time::Instant>>,
    /// Participant IDs that were removed as part of a guest ban. Guest IDs are
    /// UUID-shaped but are not account IDs, so this marker keeps the user-ID
    /// unban path from silently targeting the wrong durable key.
    banned_guest_participants: HashSet<ParticipantId>,
    /// Security policy generation captured by joiners before slow DB/password
    /// work. A mismatch at commit time forces a safe retry rather than allowing
    /// a stale snapshot of lobby/invite/password/role/sanction policy.
    policy_revision: u64,
    /// Joins reserve the room before performing database and password work.
    /// Empty-room eviction must wait for every reservation, otherwise one
    /// failed join can tear the router down underneath another valid join.
    pending_joins: usize,
    /// Sanctions intentionally outlive Participant/LobbyEntry records so a
    /// leave/rejoin cannot clear them while the runtime room remains active.
    sanctions: HashMap<SanctionKey, moderation::PunitiveState>,
    /// Set before deleting the backing row. Operations that already cloned the
    /// room Arc must reject their commit after this tombstone becomes visible.
    deleting: bool,
    /// Lobby: participants waiting for moderator approval
    pub lobby: HashMap<ParticipantId, LobbyEntry>,
}

struct MediaMutationReservation {
    room: Arc<TokioRwLock<Room>>,
    reserved_at: std::time::Instant,
}

struct MediaControlIpcReservation {
    room: Arc<TokioRwLock<Room>>,
    reserved_at: std::time::Instant,
}

impl Room {
    /// Whether a participant or lobby entry other than `except` already uses
    /// `name`, using the shared Unicode identity comparison.
    pub(crate) fn name_in_use(&self, name: &str, except: Option<&ParticipantId>) -> bool {
        let wanted = crate::labels::comparison_key(name);
        self.participants
            .values()
            .filter(|participant| Some(&participant.id) != except)
            .map(|participant| participant.name.as_str())
            .chain(self.lobby.values().map(|entry| entry.name.as_str()))
            .any(|taken| crate::labels::comparison_key(taken) == wanted)
    }

    /// The requested participant name, or "name (n)" with the smallest free n from 2,
    /// kept within the participant name limit.
    pub(crate) fn unique_participant_name(&self, name: String) -> String {
        // Normalize each occupied label once. Repeatedly scanning and folding
        // the roster for every suffix would make many equal names quadratic.
        let occupied: HashSet<String> = self
            .participants
            .values()
            .map(|participant| participant.name.as_str())
            .chain(self.lobby.values().map(|entry| entry.name.as_str()))
            .map(crate::labels::comparison_key)
            .collect();
        if !occupied.contains(&crate::labels::comparison_key(&name)) {
            return name;
        }
        let base: String = name
            .trim()
            .chars()
            .take(crate::signaling::connection::MAX_PARTICIPANT_NAME_LEN - 6)
            .collect();
        (2..)
            .map(|n| format!("{} ({n})", base.trim_end()))
            .find(|candidate| !occupied.contains(&crate::labels::comparison_key(candidate)))
            .expect("some suffix is free")
    }

    fn lobby_counts(&self) -> (u32, u32) {
        let connected = self
            .participants
            .values()
            .filter(|participant| participant.social.connected && !participant.sender.is_closed());
        let mut participants = 0_u32;
        let mut moderators = 0_u32;
        for participant in connected {
            participants = participants.saturating_add(1);
            if participant.role.can_admit_lobby() {
                moderators = moderators.saturating_add(1);
            }
        }
        (participants, moderators)
    }

    fn notify_lobby_status(&self) {
        if self.lobby.is_empty() {
            return;
        }
        let (participant_count, moderator_count) = self.lobby_counts();
        let Ok(json) = serde_json::to_string(&ServerMessage::LobbyStatus {
            participant_count,
            moderator_count,
        }) else {
            return;
        };
        let json = crate::OutboundJson::from(json);
        for entry in self
            .lobby
            .values()
            .filter(|entry| !entry.sender.is_closed())
        {
            let _ = try_send_essential(&self.metrics, &entry.sender, json.clone());
        }
    }

    fn mark_disconnected(
        &mut self,
        participant_id: &ParticipantId,
        expected_sender: &mpsc::Sender<crate::OutboundJson>,
    ) {
        let Some(participant) = self.participants.get_mut(participant_id) else {
            return;
        };
        if !participant.sender.same_channel(expected_sender) || !participant.social.connected {
            return;
        }
        participant.social.connected = false;
        self.notify_lobby_status();
    }

    #[cfg(test)]
    fn new(
        id: RoomId,
        settings: Option<settings::RoomSettings>,
        persisted: bool,
        password_hash: Option<String>,
    ) -> Self {
        Self {
            metrics: ServerMetrics::new(),
            control: Arc::new(tokio::sync::Mutex::new(())),
            id,
            social: social::RoomSocial::default(),
            broadcaster_ceiling: None,
            participants: HashMap::new(),
            settings,
            persisted,
            password_hash,
            last_password_hash_at: None,
            chat_broadcasts: window::SlidingWindow::new(
                ROOM_CHAT_WINDOW,
                MAX_ROOM_CHAT_MESSAGES_PER_WINDOW,
            ),
            media_mutations: window::SlidingWindow::new(
                ROOM_MEDIA_MUTATION_WINDOW,
                MAX_ROOM_MEDIA_MUTATIONS_PER_WINDOW,
            ),
            media_control_ipc: window::SlidingWindow::new(
                ROOM_MEDIA_CONTROL_IPC_WINDOW,
                MAX_ROOM_MEDIA_CONTROL_IPC_PER_WINDOW,
            ),
            voice_requests: window::SlidingWindow::new(
                ROOM_VOICE_REQUEST_WINDOW,
                MAX_ROOM_VOICE_REQUESTS_PER_WINDOW,
            ),
            admin_mutations: window::SlidingWindow::new(
                ROOM_ADMIN_MUTATION_WINDOW,
                MAX_ROOM_ADMIN_MUTATIONS_PER_WINDOW,
            ),
            active_speaker_observer: None,
            audio_level_observer: None,
            producer_to_participant: HashMap::new(),
            banned_participants: HashMap::new(),
            banned_guest_ips: HashMap::new(),
            banned_guest_participants: HashSet::new(),
            policy_revision: 0,
            pending_joins: 0,
            sanctions: HashMap::new(),
            deleting: false,
            lobby: HashMap::new(),
        }
    }

    fn new_with_observers(
        id: RoomId,
        settings: Option<settings::RoomSettings>,
        persisted: bool,
        password_hash: Option<String>,
        active_speaker_observer: Option<ActiveSpeakerObserver>,
        audio_level_observer: Option<AudioLevelObserver>,
        metrics: ServerMetrics,
    ) -> Self {
        Self {
            metrics,
            control: Arc::new(tokio::sync::Mutex::new(())),
            id,
            social: social::RoomSocial::default(),
            broadcaster_ceiling: None,
            participants: HashMap::new(),
            settings,
            persisted,
            password_hash,
            last_password_hash_at: None,
            chat_broadcasts: window::SlidingWindow::new(
                ROOM_CHAT_WINDOW,
                MAX_ROOM_CHAT_MESSAGES_PER_WINDOW,
            ),
            media_mutations: window::SlidingWindow::new(
                ROOM_MEDIA_MUTATION_WINDOW,
                MAX_ROOM_MEDIA_MUTATIONS_PER_WINDOW,
            ),
            media_control_ipc: window::SlidingWindow::new(
                ROOM_MEDIA_CONTROL_IPC_WINDOW,
                MAX_ROOM_MEDIA_CONTROL_IPC_PER_WINDOW,
            ),
            voice_requests: window::SlidingWindow::new(
                ROOM_VOICE_REQUEST_WINDOW,
                MAX_ROOM_VOICE_REQUESTS_PER_WINDOW,
            ),
            admin_mutations: window::SlidingWindow::new(
                ROOM_ADMIN_MUTATION_WINDOW,
                MAX_ROOM_ADMIN_MUTATIONS_PER_WINDOW,
            ),
            active_speaker_observer,
            audio_level_observer,
            producer_to_participant: HashMap::new(),
            banned_participants: HashMap::new(),
            banned_guest_ips: HashMap::new(),
            banned_guest_participants: HashSet::new(),
            policy_revision: 0,
            // `new_with_observers` is used only by the join creation path. The
            // creating join owns the first reservation before the room becomes
            // visible in the shared map.
            pending_joins: 1,
            sanctions: HashMap::new(),
            deleting: false,
            lobby: HashMap::new(),
        }
    }

    fn ensure_live(&self) -> Result<()> {
        if self.deleting {
            anyhow::bail!("Room is being deleted");
        }
        Ok(())
    }

    fn prune_expired_bans(&mut self, now: std::time::Instant) {
        self.banned_participants
            .retain(|_, expiry| expiry.is_none_or(|expiry| expiry > now));
        self.banned_guest_ips
            .retain(|_, expiry| expiry.is_none_or(|expiry| expiry > now));
        self.banned_guest_participants
            .retain(|participant_id| self.banned_participants.contains_key(participant_id));
    }

    fn identity_is_banned(
        &self,
        participant_id: &ParticipantId,
        authenticated: bool,
        ip: Option<IpAddr>,
    ) -> bool {
        self.banned_participants.contains_key(participant_id)
            || (!authenticated
                && ip.is_some_and(|address| {
                    self.banned_guest_ips
                        .contains_key(&crate::net::cohort(address))
                }))
    }

    /// Broadcast a message to all participants except the sender
    fn broadcast_except(&self, sender_id: &ParticipantId, message: &ServerMessage) {
        let json = match serde_json::to_string(message) {
            Ok(j) => crate::OutboundJson::from(j),
            Err(e) => {
                warn!("Failed to serialize broadcast message: {}", e);
                return;
            }
        };
        for (id, participant) in &self.participants {
            if id != sender_id {
                self.try_send_broadcast(&participant.sender, json.clone(), message);
            }
        }
    }

    /// Broadcast a message to all participants with role >= min_role
    fn broadcast_to_role(&self, min_role: roles::Role, message: &ServerMessage) {
        let json = match serde_json::to_string(message) {
            Ok(j) => crate::OutboundJson::from(j),
            Err(e) => {
                warn!("Failed to serialize broadcast message: {}", e);
                return;
            }
        };
        for participant in self.participants.values() {
            if participant.role >= min_role {
                self.try_send_broadcast(&participant.sender, json.clone(), message);
            }
        }
    }

    /// Broadcast a message to all participants
    fn broadcast_all(&self, message: &ServerMessage) {
        let json = match serde_json::to_string(message) {
            Ok(j) => crate::OutboundJson::from(j),
            Err(e) => {
                warn!("Failed to serialize broadcast message: {}", e);
                return;
            }
        };
        for participant in self.participants.values() {
            self.try_send_broadcast(&participant.sender, json.clone(), message);
        }
    }

    /// The caller has already removed the departed membership under this room's
    /// write lock. Retained grace memberships stay authoritative until their own
    /// removal/rebind; a future reconnect obtains the current roster snapshot.
    fn broadcast_participant_left(&self, participant_id: &ParticipantId) {
        broadcast_departure(
            &self.metrics,
            self.participants
                .values()
                .map(|participant| &participant.sender),
            || {
                serde_json::to_string(&ServerMessage::ParticipantLeft {
                    participant_id: *participant_id,
                })
            },
        );
    }

    fn try_send_broadcast(
        &self,
        sender: &mpsc::Sender<crate::OutboundJson>,
        json: crate::OutboundJson,
        message: &ServerMessage,
    ) {
        // These observations are deliberately lossy and refreshed frequently.
        // Counting them as failed control/chat delivery would hide real pressure.
        if matches!(
            message,
            ServerMessage::ActiveSpeaker { .. } | ServerMessage::AudioLevels { .. }
        ) {
            let _ = sender.try_send(json);
            return;
        }
        let _ = try_send_essential(&self.metrics, sender, json);
    }
}

fn lobby_admission_state(
    room: &Room,
    entry: &LobbyEntry,
) -> (roles::Role, moderation::PunitiveState) {
    let sanction_key =
        SanctionKey::for_identity(&entry.participant_id, entry.authenticated, entry.ip);
    (
        entry.role,
        merge_punitive_state(entry.punitive.clone(), room.sanctions.get(&sanction_key)),
    )
}

/// Resolve the full set affected by a moderation action. Guest sanctions and
/// bans are IP-scoped, so every live unauthenticated participant on the same
/// address must move together. Authorization is checked against the whole
/// cohort to prevent selecting a low-role guest as a proxy for punishing a
/// higher-role guest sharing that address.
/// A kick's history write failed: a missing report is the caller's mistake,
/// anything else is the database's.
fn moderation_history_error(error: refusal::RoomError) -> anyhow::Error {
    match error {
        refusal::RoomError::Refused(refused) => return social::refused(refused),
        refusal::RoomError::Database(sqlx::Error::RowNotFound) => {
            return anyhow::anyhow!("Report not found");
        }
        refusal::RoomError::Database(_) => {}
    }
    warn!(%error, "Failed to record a moderation history entry");
    anyhow::anyhow!("Moderation history could not be saved")
}

fn moderation_cohort(
    room: &Room,
    target_participant_id: &ParticipantId,
    moderator_role: roles::Role,
) -> Result<Vec<ParticipantId>> {
    let target = room
        .participants
        .get(target_participant_id)
        .ok_or_else(|| anyhow::anyhow!("Target participant not found"))?;

    let mut participant_ids = if !target.authenticated {
        if let Some(target_ip) = target.ip {
            let target_ip = crate::net::cohort(target_ip);
            room.participants
                .iter()
                .filter(|(_, participant)| {
                    !participant.authenticated
                        && participant.ip.map(crate::net::cohort) == Some(target_ip)
                })
                .map(|(participant_id, _)| *participant_id)
                .collect()
        } else {
            vec![*target_participant_id]
        }
    } else {
        vec![*target_participant_id]
    };

    if participant_ids.iter().any(|participant_id| {
        room.participants
            .get(participant_id)
            .is_none_or(|participant| !moderator_role.can_moderate(participant.role))
    }) {
        anyhow::bail!("Insufficient permissions to moderate this participant");
    }

    participant_ids.sort_unstable();
    Ok(participant_ids)
}

fn guest_ip_lobby_cohort(
    room: &Room,
    target_authenticated: bool,
    target_ip: Option<IpAddr>,
    moderator_role: roles::Role,
) -> Result<Vec<ParticipantId>> {
    if target_authenticated {
        return Ok(Vec::new());
    }
    let Some(target_ip) = target_ip else {
        return Ok(Vec::new());
    };
    let target_ip = crate::net::cohort(target_ip);
    let mut participant_ids: Vec<ParticipantId> = room
        .lobby
        .iter()
        .filter(|(_, entry)| {
            !entry.authenticated && entry.ip.map(crate::net::cohort) == Some(target_ip)
        })
        .map(|(participant_id, _)| *participant_id)
        .collect();
    if participant_ids.iter().any(|participant_id| {
        room.lobby
            .get(participant_id)
            .is_none_or(|entry| !moderator_role.can_moderate(entry.role))
    }) {
        anyhow::bail!("Insufficient permissions to moderate this participant");
    }
    participant_ids.sort_unstable();
    Ok(participant_ids)
}

fn punitive_notification(
    kind: moderation::PunitiveKind,
    enabled: bool,
    participant_id: ParticipantId,
) -> ServerMessage {
    match (kind, enabled) {
        (moderation::PunitiveKind::CamBanned, true) => ServerMessage::CamBanned { participant_id },
        (moderation::PunitiveKind::CamBanned, false) => {
            ServerMessage::CamUnbanned { participant_id }
        }
        (moderation::PunitiveKind::Muted, true) => ServerMessage::TextMuted { participant_id },
        (moderation::PunitiveKind::Muted, false) => ServerMessage::TextUnmuted { participant_id },
    }
}

fn punitive_value(punitive: &moderation::PunitiveState, kind: moderation::PunitiveKind) -> bool {
    match kind {
        moderation::PunitiveKind::CamBanned => punitive.cam_banned,
        moderation::PunitiveKind::Muted => punitive.text_muted,
    }
}

fn punitive_cohort_matches(
    room: &Room,
    participant_ids: &[ParticipantId],
    lobby_ids: &[ParticipantId],
    sanction_key: &SanctionKey,
    kind: moderation::PunitiveKind,
    enabled: bool,
) -> bool {
    let stored_enabled = room
        .sanctions
        .get(sanction_key)
        .is_some_and(|punitive| punitive_value(punitive, kind));
    stored_enabled == enabled
        && participant_ids.iter().all(|participant_id| {
            room.participants
                .get(participant_id)
                .is_some_and(|participant| punitive_value(&participant.punitive, kind) == enabled)
        })
        && lobby_ids.iter().all(|participant_id| {
            room.lobby
                .get(participant_id)
                .is_some_and(|entry| punitive_value(&entry.punitive, kind) == enabled)
        })
}

fn apply_punitive_to_cohort(
    room: &mut Room,
    participant_ids: &[ParticipantId],
    lobby_ids: &[ParticipantId],
    sanction_key: SanctionKey,
    kind: moderation::PunitiveKind,
    enabled: bool,
) -> moderation::PunitiveState {
    // Preserve the union of existing restrictions. Cohort members may differ
    // only because they joined before the IP-scoped state was installed; after
    // this mutation they must all reflect the same durable identity state.
    let mut punitive = room
        .sanctions
        .get(&sanction_key)
        .cloned()
        .unwrap_or_default();
    for participant_id in participant_ids {
        if let Some(participant) = room.participants.get(participant_id) {
            punitive = merge_punitive_state(punitive, Some(&participant.punitive));
        }
    }
    for participant_id in lobby_ids {
        if let Some(entry) = room.lobby.get(participant_id) {
            punitive = merge_punitive_state(punitive, Some(&entry.punitive));
        }
    }
    match kind {
        moderation::PunitiveKind::CamBanned => punitive.cam_banned = enabled,
        moderation::PunitiveKind::Muted => punitive.text_muted = enabled,
    }
    for participant_id in participant_ids {
        if let Some(participant) = room.participants.get_mut(participant_id) {
            participant.punitive = punitive.clone();
        }
    }
    for participant_id in lobby_ids {
        if let Some(entry) = room.lobby.get_mut(participant_id) {
            entry.punitive = punitive.clone();
        }
    }
    if punitive == moderation::PunitiveState::default() {
        room.sanctions.remove(&sanction_key);
    } else {
        room.sanctions.insert(sanction_key, punitive.clone());
    }
    punitive
}

fn record_banned_cohort(
    room: &mut Room,
    participant_ids: &[ParticipantId],
    target_authenticated: bool,
    target_ip: Option<IpAddr>,
    runtime_expiry: Option<std::time::Instant>,
) {
    for participant_id in participant_ids {
        room.banned_participants
            .insert(*participant_id, runtime_expiry);
    }
    if !target_authenticated {
        room.banned_guest_participants
            .extend(participant_ids.iter().cloned());
        if let Some(address) = target_ip {
            room.banned_guest_ips
                .insert(crate::net::cohort(address), runtime_expiry);
        }
    }
}

/// Manages all rooms and coordinates media + signaling.
///
/// Uses per-room locking: the outer HashMap is protected by a std::sync::RwLock
/// (held only for brief lookups/inserts, never across await points), while each
/// room has a short-lived state lock and a separate control-operation gate.
/// Persistence retains control without retaining the state lock, keeping chat
/// and routine media operations available. Clones share all limits and state.
///
/// Persistent control mutations own their work after admission: abandoning the
/// caller's future does not imply rollback or prevent committed state from being
/// published. A confirmed rejection leaves existing policy unchanged; uncertain
/// writes quarantine the exact runtime room until restart reloads durable state.
#[derive(Clone)]
pub(crate) struct RoomManager {
    rooms: Arc<RwLock<HashMap<RoomId, Arc<TokioRwLock<Room>>>>>,
    media_server: Arc<MediaServer>,
    drain: DrainSignal,
    metrics: ServerMetrics,
    db_pool: Option<sqlx::PgPool>,
    account_updates: crate::auth::updates::Hub,
    /// Serializes room creation — two clients joining a brand-new room
    /// concurrently would otherwise both call create_router and one would
    /// fail with "Router already exists". Creation is rare (once per room),
    /// so a single async mutex is cheap.
    room_creation_lock: Arc<tokio::sync::Mutex<()>>,
    /// Room IDs stay reserved while deletion tears down their old router and
    /// media state outside the creation lock.
    deleting_rooms: Arc<RwLock<HashMap<RoomId, uuid::Uuid>>>,
    /// Rooms whose durable identity is mid-update: creation of that one room
    /// waits, instead of every room waiting on the process-wide creation lock.
    identity_updates: Arc<RwLock<HashSet<RoomId>>>,
    max_rooms: usize,
    max_persisted_rooms: i64,
    allow_ad_hoc_rooms: bool,
    /// Server-wide participant ceiling per room, applied under any room's own
    /// limit; sized from the measured capacity of the deployment.
    max_participants_per_room: Option<usize>,
    max_broadcasters_per_room: Option<usize>,
    /// CPU saturation monitor; fresh joins are refused while it reports saturation.
    saturation: std::sync::OnceLock<crate::saturation::SaturationMonitor>,
    /// Verification must remain available even if an administrator repeatedly
    /// requests expensive password hashes.
    password_verify_work: Arc<tokio::sync::Semaphore>,
    password_hash_work: Arc<tokio::sync::Semaphore>,
    max_password_verify_work: usize,
    join_attempts_by_ip: SharedRateLimiter<IpAddr>,
    join_attempts_by_room_ip: SharedRateLimiter<(String, IpAddr)>,
}

#[derive(Clone)]
struct FailedJoinCleanup {
    rooms: Arc<RwLock<HashMap<RoomId, Arc<TokioRwLock<Room>>>>>,
    deleting_rooms: Arc<RwLock<HashMap<RoomId, uuid::Uuid>>>,
    room_creation_lock: Arc<tokio::sync::Mutex<()>>,
    media_server: Arc<MediaServer>,
}

/// Owns one pending-join count from the instant a runtime room becomes visible.
/// If the join future returns an error or is cancelled, `Drop` schedules the
/// decrement and empty-room teardown on the runtime so an unauthenticated join
/// cannot leave a router behind.
struct PendingRoomJoin {
    room_id: RoomId,
    room: Arc<TokioRwLock<Room>>,
    cleanup: FailedJoinCleanup,
    active: bool,
}

impl PendingRoomJoin {
    fn room(&self) -> Arc<TokioRwLock<Room>> {
        self.room.clone()
    }

    fn complete_locked(&mut self, room: &mut Room) {
        if room.pending_joins == 0 {
            warn!(room_id = %self.room_id, "Join reservation count underflow");
        } else {
            room.pending_joins -= 1;
        }
        self.active = false;
    }
}

impl Drop for PendingRoomJoin {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        self.active = false;
        let cleanup = self.cleanup.clone();
        let room_id = self.room_id.clone();
        let room = self.room.clone();
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn(async move {
                    cleanup.release_failed_join(room_id, room).await;
                });
            }
            Err(error) => {
                warn!(%error, room_id = %self.room_id, "Could not schedule failed-join cleanup");
            }
        }
    }
}

fn release_join_reservation(room: &mut Room) -> bool {
    if room.pending_joins == 0 {
        return false;
    }
    room.pending_joins -= 1;
    !room.deleting
        && room.pending_joins == 0
        && room.participants.is_empty()
        && room.lobby.is_empty()
}

impl FailedJoinCleanup {
    async fn release_failed_join(self, room_id: RoomId, room_lock: Arc<TokioRwLock<Room>>) {
        // Cleanup has no request deadline: every creator holding this mutex is
        // itself bounded, and abandoning this reservation would permanently
        // consume runtime room/router capacity.
        let creation_guard = self.room_creation_lock.lock().await;
        let control_guard = control::lock_room(&room_lock).await;
        let mut room = room_lock.write().await;
        let should_remove = release_join_reservation(&mut room)
            && self
                .rooms
                .read()
                .get(&room_id)
                .is_some_and(|candidate| Arc::ptr_eq(candidate, &room_lock));

        let eviction_token = if should_remove {
            room.deleting = true;
            room.policy_revision = room.policy_revision.wrapping_add(1);
            let deletion_token = uuid::Uuid::new_v4();
            self.deleting_rooms
                .write()
                .insert(room_id.clone(), deletion_token);
            let mut rooms = self.rooms.write();
            if rooms
                .get(&room_id)
                .is_some_and(|candidate| Arc::ptr_eq(candidate, &room_lock))
            {
                rooms.remove(&room_id);
                Some(deletion_token)
            } else {
                release_deletion_reservation(&self.deleting_rooms, &room_id, deletion_token);
                None
            }
        } else {
            None
        };
        drop(room);
        drop(control_guard);
        drop(creation_guard);

        if let Some(deletion_token) = eviction_token {
            let router_teardown_succeeded = match tokio::time::timeout(
                ROOM_DELETE_TIMEOUT,
                self.media_server.remove_router(room_id.as_str()),
            )
            .await
            {
                Ok(Ok(())) => {
                    debug!(%room_id, "Cleaned up router after failed join");
                    true
                }
                Ok(Err(error)) => {
                    warn!(%room_id, %error, "Failed to tear down failed-join router; retaining room-ID reservation");
                    false
                }
                Err(_) => {
                    warn!(
                        %room_id,
                        "Timed out tearing down failed-join router; retaining room-ID reservation"
                    );
                    false
                }
            };
            release_deletion_reservation_after_router_teardown(
                &self.deleting_rooms,
                &room_id,
                deletion_token,
                router_teardown_succeeded,
            );
        }
    }
}
