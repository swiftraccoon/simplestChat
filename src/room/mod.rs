#![forbid(unsafe_code)]

// Room module - Room state management and participant tracking
pub mod api;
pub mod moderation;
pub mod roles;
pub mod settings;

use crate::media::types::{MediaResult, TransportInfo};
use crate::media::{MediaConfig, MediaServer};
use crate::metrics::ServerMetrics;
use crate::signaling::protocol::{
    AudioLevelEntry, ParticipantInfo, ProducerMetadata, ServerMessage,
};
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
use std::sync::{Arc, Mutex as StdMutex, RwLock as StdRwLock, Weak};
use tokio::sync::RwLock as TokioRwLock;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

/// Events sent from observer callbacks (sync Fn) to async broadcast task
enum ObserverEvent {
    ActiveSpeaker { producer_id: ProducerId },
    AudioLevels { volumes: Vec<(ProducerId, i8)> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RevokedProducer {
    participant_id: String,
    media_session_id: uuid::Uuid,
    producer_id: String,
    kind: MediaKind,
}

/// Participant in a room
#[derive(Clone)]
pub struct Participant {
    pub id: String,
    pub name: String,
    pub sender: mpsc::Sender<Arc<String>>,
    /// Unique to this room membership, even when `id` is a stable account UUID.
    /// Media cleanup must use this value so an old socket cannot tear down a
    /// replacement session that joined with the same participant ID.
    pub media_session_id: uuid::Uuid,
    pub producers: HashMap<String, (MediaKind, Option<String>)>,
    pub role: roles::Role,
    pub punitive: moderation::PunitiveState,
    /// True when the connection carried a valid JWT (id == user uuid)
    pub authenticated: bool,
    /// Client IP (from ConnectInfo / X-Forwarded-For) — used for guest bans
    pub ip: Option<std::net::IpAddr>,
}

/// Entry for a participant waiting in the lobby
pub struct LobbyEntry {
    pub participant_id: String,
    pub name: String,
    pub sender: mpsc::Sender<Arc<String>>,
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
    fn for_identity(participant_id: &str, authenticated: bool, ip: Option<IpAddr>) -> Self {
        if authenticated {
            Self::User(participant_id.to_string())
        } else if let Some(ip) = ip {
            Self::GuestIp(moderation::canonical_guest_ip(ip))
        } else {
            Self::Connection(participant_id.to_string())
        }
    }
}

/// Result of an add_participant attempt
pub enum JoinResult {
    /// Successfully joined the room — includes participant list, role, and room settings
    Joined {
        participants: Vec<ParticipantInfo>,
        role: String,
        room_settings: Option<serde_json::Value>,
    },
    /// Placed in the lobby awaiting moderator approval
    Lobbied,
}

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
// so retain a deliberately generous room-wide ceiling: enough for roughly 38
// participants to perform a full 53-operation media startup burst per second.
const ROOM_MEDIA_CONTROL_IPC_WINDOW: std::time::Duration = std::time::Duration::from_secs(1);
const MAX_ROOM_MEDIA_CONTROL_IPC_PER_WINDOW: usize = 2_048;
const ROOM_VOICE_REQUEST_WINDOW: std::time::Duration = std::time::Duration::from_secs(10);
const MAX_ROOM_VOICE_REQUESTS_PER_WINDOW: usize = 5;
const ROOM_ADMIN_MUTATION_WINDOW: std::time::Duration = std::time::Duration::from_secs(1);
const MAX_ROOM_ADMIN_MUTATIONS_PER_WINDOW: usize = 20;
const ALLOW_AD_HOC_ROOMS_BY_DEFAULT: bool = false;
const JOIN_RATE_WINDOW: std::time::Duration = std::time::Duration::from_secs(60);
const MAX_JOIN_ATTEMPTS_PER_IP: u32 = 30;
const MAX_JOIN_ATTEMPTS_PER_ROOM_IP: u32 = 10;
const MAX_TRACKED_JOIN_KEYS: usize = 10_000;

#[derive(Debug)]
struct RateWindow {
    started_at: std::time::Instant,
    attempts: u32,
}

struct SharedRateLimiter<K> {
    entries: StdMutex<HashMap<K, RateWindow>>,
    max_attempts: u32,
}

impl<K> SharedRateLimiter<K>
where
    K: Eq + Hash + Clone,
{
    fn new(max_attempts: u32) -> Self {
        Self {
            entries: StdMutex::new(HashMap::new()),
            max_attempts,
        }
    }

    fn allow(&self, key: K, now: std::time::Instant) -> bool {
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if entries.len() >= MAX_TRACKED_JOIN_KEYS && !entries.contains_key(&key) {
            // Memory capacity must not become either a global deny switch or
            // an O(n) scan per attacker-selected room ID.
            if let Some(evicted) = entries.keys().next().cloned() {
                entries.remove(&evicted);
            }
        }

        let entry = entries.entry(key).or_insert(RateWindow {
            started_at: now,
            attempts: 0,
        });
        if now.duration_since(entry.started_at) >= JOIN_RATE_WINDOW {
            entry.started_at = now;
            entry.attempts = 0;
        }
        if entry.attempts >= self.max_attempts {
            return false;
        }
        entry.attempts += 1;
        true
    }
}

fn rate_limit_ip(address: IpAddr) -> IpAddr {
    match address {
        IpAddr::V4(_) => address,
        IpAddr::V6(address) => {
            if let Some(address) = address.to_ipv4_mapped() {
                return IpAddr::V4(address);
            }
            let segments = address.segments();
            IpAddr::V6(std::net::Ipv6Addr::new(
                segments[0],
                segments[1],
                segments[2],
                segments[3],
                0,
                0,
                0,
                0,
            ))
        }
    }
}

fn environment_flag(name: &str, default: bool) -> Result<bool> {
    match std::env::var(name) {
        Ok(value) => parse_environment_flag_value(name, Some(&value), default),
        Err(std::env::VarError::NotPresent) => parse_environment_flag_value(name, None, default),
        Err(std::env::VarError::NotUnicode(_)) => anyhow::bail!("{name} must be valid UTF-8"),
    }
}

fn parse_environment_flag_value(name: &str, value: Option<&str>, default: bool) -> Result<bool> {
    match value {
        Some(value) if value.eq_ignore_ascii_case("true") || value == "1" => Ok(true),
        Some(value) if value.eq_ignore_ascii_case("false") || value == "0" => Ok(false),
        Some(_) => anyhow::bail!("{name} must be true, false, 1, or 0"),
        None => Ok(default),
    }
}

fn room_delete_timeout(operation: &str) -> sqlx::Error {
    sqlx::Error::Protocol(format!("room deletion timed out while {operation}"))
}

fn room_creation_timeout(operation: &str) -> sqlx::Error {
    sqlx::Error::Protocol(format!("room creation timed out while {operation}"))
}

fn release_deletion_reservation(
    deleting_rooms: &StdRwLock<HashMap<String, uuid::Uuid>>,
    room_id: &str,
    token: uuid::Uuid,
) {
    let mut deleting = deleting_rooms
        .write()
        .unwrap_or_else(|error| error.into_inner());
    if deleting.get(room_id) == Some(&token) {
        deleting.remove(room_id);
    }
}

fn release_deletion_reservation_after_router_teardown(
    deleting_rooms: &StdRwLock<HashMap<String, uuid::Uuid>>,
    room_id: &str,
    token: uuid::Uuid,
    router_teardown_succeeded: bool,
) {
    if router_teardown_succeeded {
        release_deletion_reservation(deleting_rooms, room_id, token);
    }
}

async fn restore_failed_deletion(
    runtime_room: Option<Arc<TokioRwLock<Room>>>,
    deleting_rooms: Arc<StdRwLock<HashMap<String, uuid::Uuid>>>,
    room_id: String,
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

/// Room state
pub struct Room {
    pub id: String,
    pub router_id: String,
    pub participants: HashMap<String, Participant>,
    pub settings: Option<settings::RoomSettings>,
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
    recent_chat_broadcasts: VecDeque<std::time::Instant>,
    /// Producer mutations cross mediasoup IPC and fan out room-wide. A shared
    /// room budget prevents many individually compliant sockets from restoring
    /// the original amplification attack.
    recent_media_mutations: VecDeque<std::time::Instant>,
    /// Consumer and transport operations cross the mediasoup worker boundary.
    /// Bound their aggregate so a large open room cannot multiply each
    /// participant's compliant per-session refill into unbounded worker IPC.
    recent_media_control_ipc: VecDeque<std::time::Instant>,
    /// Voice requests create persistent action UI for moderators, so bound the
    /// aggregate as well as applying the per-connection cooldown.
    recent_voice_requests: VecDeque<std::time::Instant>,
    /// Authorized moderation and settings changes can write the database and
    /// fan out room-wide. Bound their aggregate across moderator sockets.
    recent_admin_mutations: VecDeque<std::time::Instant>,
    active_speaker_observer: Option<ActiveSpeakerObserver>,
    audio_level_observer: Option<AudioLevelObserver>,
    /// Map producer_id -> participant_id for observer lookups
    producer_to_participant: HashMap<String, String>,
    /// In-memory ban list — prevents banned participants from rejoining
    banned_participants: HashMap<String, Option<std::time::Instant>>,
    /// Guest identities are connection-scoped, so retain their IP as the
    /// in-memory ban key (registered users are always keyed by user UUID).
    banned_guest_ips: HashMap<std::net::IpAddr, Option<std::time::Instant>>,
    /// Participant IDs that were removed as part of a guest ban. Guest IDs are
    /// UUID-shaped but are not account IDs, so this marker keeps the user-ID
    /// unban path from silently targeting the wrong durable key.
    banned_guest_participants: HashSet<String>,
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
    pub lobby: HashMap<String, LobbyEntry>,
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
    #[cfg(test)]
    fn new(
        id: String,
        router_id: String,
        settings: Option<settings::RoomSettings>,
        persisted: bool,
        password_hash: Option<String>,
    ) -> Self {
        Self {
            id,
            router_id,
            participants: HashMap::new(),
            settings,
            persisted,
            password_hash,
            last_password_hash_at: None,
            recent_chat_broadcasts: VecDeque::new(),
            recent_media_mutations: VecDeque::new(),
            recent_media_control_ipc: VecDeque::new(),
            recent_voice_requests: VecDeque::new(),
            recent_admin_mutations: VecDeque::new(),
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
        id: String,
        router_id: String,
        settings: Option<settings::RoomSettings>,
        persisted: bool,
        password_hash: Option<String>,
        active_speaker_observer: Option<ActiveSpeakerObserver>,
        audio_level_observer: Option<AudioLevelObserver>,
    ) -> Self {
        Self {
            id,
            router_id,
            participants: HashMap::new(),
            settings,
            persisted,
            password_hash,
            last_password_hash_at: None,
            recent_chat_broadcasts: VecDeque::new(),
            recent_media_mutations: VecDeque::new(),
            recent_media_control_ipc: VecDeque::new(),
            recent_voice_requests: VecDeque::new(),
            recent_admin_mutations: VecDeque::new(),
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

    fn reserve_chat_broadcast(&mut self, now: std::time::Instant) -> bool {
        while self
            .recent_chat_broadcasts
            .front()
            .is_some_and(|sent_at| now.duration_since(*sent_at) >= ROOM_CHAT_WINDOW)
        {
            self.recent_chat_broadcasts.pop_front();
        }
        if self.recent_chat_broadcasts.len() >= MAX_ROOM_CHAT_MESSAGES_PER_WINDOW {
            return false;
        }
        self.recent_chat_broadcasts.push_back(now);
        true
    }

    fn reserve_media_mutation(&mut self, now: std::time::Instant) -> bool {
        while self
            .recent_media_mutations
            .front()
            .is_some_and(|sent_at| now.duration_since(*sent_at) >= ROOM_MEDIA_MUTATION_WINDOW)
        {
            self.recent_media_mutations.pop_front();
        }
        if self.recent_media_mutations.len() >= MAX_ROOM_MEDIA_MUTATIONS_PER_WINDOW {
            return false;
        }
        self.recent_media_mutations.push_back(now);
        true
    }

    fn refund_media_mutation(&mut self, reserved_at: std::time::Instant) -> bool {
        let Some(position) = self
            .recent_media_mutations
            .iter()
            .position(|entry| *entry == reserved_at)
        else {
            return false;
        };
        self.recent_media_mutations.remove(position);
        true
    }

    fn reserve_media_control_ipc(&mut self, now: std::time::Instant) -> bool {
        while self
            .recent_media_control_ipc
            .front()
            .is_some_and(|sent_at| now.duration_since(*sent_at) >= ROOM_MEDIA_CONTROL_IPC_WINDOW)
        {
            self.recent_media_control_ipc.pop_front();
        }
        if self.recent_media_control_ipc.len() >= MAX_ROOM_MEDIA_CONTROL_IPC_PER_WINDOW {
            return false;
        }
        self.recent_media_control_ipc.push_back(now);
        true
    }

    fn refund_media_control_ipc(&mut self, reserved_at: std::time::Instant) -> bool {
        let Some(position) = self
            .recent_media_control_ipc
            .iter()
            .position(|entry| *entry == reserved_at)
        else {
            return false;
        };
        self.recent_media_control_ipc.remove(position);
        true
    }

    fn reserve_voice_request(&mut self, now: std::time::Instant) -> bool {
        while self
            .recent_voice_requests
            .front()
            .is_some_and(|sent_at| now.duration_since(*sent_at) >= ROOM_VOICE_REQUEST_WINDOW)
        {
            self.recent_voice_requests.pop_front();
        }
        if self.recent_voice_requests.len() >= MAX_ROOM_VOICE_REQUESTS_PER_WINDOW {
            return false;
        }
        self.recent_voice_requests.push_back(now);
        true
    }

    fn reserve_admin_mutation(&mut self, now: std::time::Instant) -> bool {
        while self
            .recent_admin_mutations
            .front()
            .is_some_and(|sent_at| now.duration_since(*sent_at) >= ROOM_ADMIN_MUTATION_WINDOW)
        {
            self.recent_admin_mutations.pop_front();
        }
        if self.recent_admin_mutations.len() >= MAX_ROOM_ADMIN_MUTATIONS_PER_WINDOW {
            return false;
        }
        self.recent_admin_mutations.push_back(now);
        true
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
        participant_id: &str,
        authenticated: bool,
        ip: Option<IpAddr>,
    ) -> bool {
        self.banned_participants.contains_key(participant_id)
            || (!authenticated
                && ip.is_some_and(|address| {
                    self.banned_guest_ips
                        .contains_key(&moderation::canonical_guest_ip(address))
                }))
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
                        debug!(
                            "Channel full for participant {} in room {}, dropping message",
                            id, self.id
                        );
                    }
                    Err(mpsc::error::TrySendError::Closed(_)) => {
                        debug!(
                            "Channel closed for participant {} in room {} (disconnected)",
                            id, self.id
                        );
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
                    debug!(
                        "Channel full for participant {} in room {}, dropping message",
                        participant_id, self.id
                    );
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    debug!(
                        "Channel closed for participant {} in room {} (disconnected)",
                        participant_id, self.id
                    );
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
                        debug!(
                            "Channel full for participant {} in room {}, dropping message",
                            id, self.id
                        );
                    }
                    Err(mpsc::error::TrySendError::Closed(_)) => {
                        debug!(
                            "Channel closed for participant {} in room {} (disconnected)",
                            id, self.id
                        );
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
                    debug!(
                        "Channel full for participant {} in room {}, dropping message",
                        id, self.id
                    );
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    debug!(
                        "Channel closed for participant {} in room {} (disconnected)",
                        id, self.id
                    );
                }
            }
        }
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
fn moderation_cohort(
    room: &Room,
    target_participant_id: &str,
    moderator_role: roles::Role,
) -> Result<Vec<String>> {
    let target = room
        .participants
        .get(target_participant_id)
        .ok_or_else(|| anyhow::anyhow!("Target participant not found"))?;

    let mut participant_ids = if !target.authenticated {
        if let Some(target_ip) = target.ip {
            let target_ip = moderation::canonical_guest_ip(target_ip);
            room.participants
                .iter()
                .filter(|(_, participant)| {
                    !participant.authenticated
                        && participant.ip.map(moderation::canonical_guest_ip) == Some(target_ip)
                })
                .map(|(participant_id, _)| participant_id.clone())
                .collect()
        } else {
            vec![target_participant_id.to_string()]
        }
    } else {
        vec![target_participant_id.to_string()]
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
) -> Result<Vec<String>> {
    if target_authenticated {
        return Ok(Vec::new());
    }
    let Some(target_ip) = target_ip else {
        return Ok(Vec::new());
    };
    let target_ip = moderation::canonical_guest_ip(target_ip);
    let mut participant_ids: Vec<String> = room
        .lobby
        .iter()
        .filter(|(_, entry)| {
            !entry.authenticated && entry.ip.map(moderation::canonical_guest_ip) == Some(target_ip)
        })
        .map(|(participant_id, _)| participant_id.clone())
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
    participant_id: String,
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
    participant_ids: &[String],
    lobby_ids: &[String],
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
    participant_ids: &[String],
    lobby_ids: &[String],
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
    participant_ids: &[String],
    target_authenticated: bool,
    target_ip: Option<IpAddr>,
    runtime_expiry: Option<std::time::Instant>,
) {
    for participant_id in participant_ids {
        room.banned_participants
            .insert(participant_id.clone(), runtime_expiry);
    }
    if !target_authenticated {
        room.banned_guest_participants
            .extend(participant_ids.iter().cloned());
        if let Some(address) = target_ip {
            room.banned_guest_ips
                .insert(moderation::canonical_guest_ip(address), runtime_expiry);
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
    /// Serializes room creation — two clients joining a brand-new room
    /// concurrently would otherwise both call create_router and one would
    /// fail with "Router already exists". Creation is rare (once per room),
    /// so a single async mutex is cheap.
    room_creation_lock: Arc<tokio::sync::Mutex<()>>,
    /// Room IDs stay reserved while deletion tears down their old router and
    /// media state outside the creation lock.
    deleting_rooms: Arc<StdRwLock<HashMap<String, uuid::Uuid>>>,
    max_rooms: usize,
    max_persisted_rooms: i64,
    allow_ad_hoc_rooms: bool,
    /// Verification must remain available even if an administrator repeatedly
    /// requests expensive password hashes.
    password_verify_work: Arc<tokio::sync::Semaphore>,
    password_hash_work: Arc<tokio::sync::Semaphore>,
    join_attempts_by_ip: SharedRateLimiter<IpAddr>,
    join_attempts_by_room_ip: SharedRateLimiter<(String, IpAddr)>,
}

#[derive(Clone)]
struct FailedJoinCleanup {
    rooms: Arc<StdRwLock<HashMap<String, Arc<TokioRwLock<Room>>>>>,
    deleting_rooms: Arc<StdRwLock<HashMap<String, uuid::Uuid>>>,
    room_creation_lock: Arc<tokio::sync::Mutex<()>>,
    media_server: Arc<MediaServer>,
}

/// Owns one pending-join count from the instant a runtime room becomes visible.
/// If the join future returns an error or is cancelled, `Drop` schedules the
/// decrement and empty-room teardown on the runtime so an unauthenticated join
/// cannot leave a router behind.
struct PendingRoomJoin {
    room_id: String,
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
            warn!(room_id = self.room_id, "Join reservation count underflow");
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
                warn!(%error, room_id = self.room_id, "Could not schedule failed-join cleanup");
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
    async fn release_failed_join(self, room_id: String, room_lock: Arc<TokioRwLock<Room>>) {
        // Cleanup has no request deadline: every creator holding this mutex is
        // itself bounded, and abandoning this reservation would permanently
        // consume runtime room/router capacity.
        let creation_guard = self.room_creation_lock.lock().await;
        let mut room = room_lock.write().await;
        let should_remove = release_join_reservation(&mut room)
            && self
                .rooms
                .read()
                .unwrap_or_else(|error| error.into_inner())
                .get(&room_id)
                .is_some_and(|candidate| Arc::ptr_eq(candidate, &room_lock));

        let eviction_token = if should_remove {
            room.deleting = true;
            room.policy_revision = room.policy_revision.wrapping_add(1);
            let deletion_token = uuid::Uuid::new_v4();
            self.deleting_rooms
                .write()
                .unwrap_or_else(|error| error.into_inner())
                .insert(room_id.clone(), deletion_token);
            let mut rooms = self
                .rooms
                .write()
                .unwrap_or_else(|error| error.into_inner());
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
        drop(creation_guard);

        if let Some(deletion_token) = eviction_token {
            let router_teardown_succeeded = match tokio::time::timeout(
                ROOM_DELETE_TIMEOUT,
                self.media_server.remove_router(&room_id),
            )
            .await
            {
                Ok(Ok(())) => {
                    debug!(room_id, "Cleaned up router after failed join");
                    true
                }
                Ok(Err(error)) => {
                    warn!(room_id, %error, "Failed to tear down failed-join router; retaining room-ID reservation");
                    false
                }
                Err(_) => {
                    warn!(
                        room_id,
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

impl RoomManager {
    /// Creates a new room manager
    ///
    /// # Errors
    /// Returns an error if media server initialization fails
    pub async fn new(
        media_config: MediaConfig,
        metrics: ServerMetrics,
        db_pool: Option<sqlx::PgPool>,
    ) -> Result<Self> {
        // A loopback listener can still be internet-facing through a reverse
        // proxy, so ephemeral room creation is always an explicit opt-in.
        let allow_ad_hoc_rooms =
            environment_flag("ALLOW_AD_HOC_ROOMS", ALLOW_AD_HOC_ROOMS_BY_DEFAULT)?;
        let media_server = Arc::new(MediaServer::new(media_config).await?);

        let password_verify_workers = std::env::var("MAX_PASSWORD_WORKERS")
            .ok()
            .and_then(|value| value.parse().ok())
            .filter(|value| (1..=32).contains(value))
            .unwrap_or(2);

        Ok(Self {
            rooms: Arc::new(StdRwLock::new(HashMap::new())),
            media_server,
            metrics,
            db_pool,
            room_creation_lock: Arc::new(tokio::sync::Mutex::new(())),
            deleting_rooms: Arc::new(StdRwLock::new(HashMap::new())),
            max_rooms: std::env::var("MAX_ROOMS")
                .ok()
                .and_then(|value| value.parse().ok())
                .filter(|value| *value > 0)
                .unwrap_or(1_000),
            max_persisted_rooms: std::env::var("MAX_PERSISTED_ROOMS")
                .ok()
                .and_then(|value| value.parse().ok())
                .filter(|value| (1..=1_000_000).contains(value))
                .unwrap_or(10_000),
            allow_ad_hoc_rooms,
            password_verify_work: Arc::new(tokio::sync::Semaphore::new(password_verify_workers)),
            // One bounded hashing lane prevents room settings from consuming
            // every CPU while the separate verification lane serves joiners.
            password_hash_work: Arc::new(tokio::sync::Semaphore::new(1)),
            join_attempts_by_ip: SharedRateLimiter::new(MAX_JOIN_ATTEMPTS_PER_IP),
            join_attempts_by_room_ip: SharedRateLimiter::new(MAX_JOIN_ATTEMPTS_PER_ROOM_IP),
        })
    }

    /// Gets the media server for direct access (e.g., find_producer_paused)
    pub fn media_server(&self) -> &MediaServer {
        &self.media_server
    }

    pub(crate) async fn hash_room_password(&self, password: String) -> Result<String> {
        let permit = self
            .password_hash_work
            .clone()
            .try_acquire_owned()
            .map_err(|_| anyhow::anyhow!("Password service is busy; try again"))?;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            crate::auth::password::hash_password(&password)
        })
        .await
        .map_err(|error| {
            warn!(%error, "Password hash task failed");
            anyhow::anyhow!("Password update failed")
        })?
        .map_err(|error| {
            warn!(%error, "Password hashing failed");
            anyhow::anyhow!("Password update failed")
        })
    }

    /// Atomically reserve a database-backed room ID against ad-hoc runtime room
    /// creation. Without sharing the creation lock, an anonymous join could
    /// create an owner-controlled runtime room between the REST endpoint's
    /// existence check and the database insert.
    pub(crate) async fn create_persisted_room(
        &self,
        owner_id: &uuid::Uuid,
        request: &settings::CreateRoomRequest,
        password_hash: Option<&str>,
    ) -> std::result::Result<Option<settings::RoomSettings>, sqlx::Error> {
        let _creation_guard =
            tokio::time::timeout(ROOM_CREATION_TIMEOUT, self.room_creation_lock.lock())
                .await
                .map_err(|_| room_creation_timeout("waiting for room creation"))?;
        if self
            .deleting_rooms
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .contains_key(&request.id)
        {
            return Ok(None);
        }
        if self
            .rooms
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .contains_key(&request.id)
        {
            return Ok(None);
        }
        let pool = self.db_pool.as_ref().ok_or(sqlx::Error::PoolClosed)?;
        tokio::time::timeout(
            ROOM_CREATION_TIMEOUT,
            settings::create_room(
                pool,
                owner_id,
                request,
                password_hash,
                self.max_persisted_rooms,
            ),
        )
        .await
        .map_err(|_| room_creation_timeout("writing the database row"))?
        .map(Some)
    }

    /// Delete a persisted room without leaving an in-memory zombie. Ownership
    /// is checked before any runtime tombstone is installed, so unauthorized
    /// callers cannot temporarily disrupt a live room. The creation mutex
    /// serializes the reservation, tombstone, and conditional DB delete against
    /// creation; the reservation then protects map eviction and media teardown
    /// after the mutex is released.
    pub(crate) async fn delete_persisted_room(
        &self,
        room_id: &str,
        requester_id: &uuid::Uuid,
    ) -> std::result::Result<DeleteRoomResult, sqlx::Error> {
        let pool = self.db_pool.clone().ok_or(sqlx::Error::PoolClosed)?;
        let room_id = room_id.to_string();
        let requester_id = *requester_id;
        let rooms = self.rooms.clone();
        let deleting_rooms = self.deleting_rooms.clone();
        let creation_lock = self.room_creation_lock.clone();
        let media_server = self.media_server.clone();

        // Run the mutation in its own task so cancellation of the HTTP request
        // cannot strand a committed delete halfway through runtime teardown.
        let deletion = tokio::spawn(async move {
            // Capture an immutable row generation before taking the local
            // creation lock. The conditional DELETE below revalidates both
            // ownership and generation, so a delayed task cannot delete a
            // same-ID replacement created after this read.
            let generation = tokio::time::timeout(
                ROOM_DELETE_TIMEOUT,
                sqlx::query_as::<_, (uuid::Uuid, chrono::DateTime<chrono::Utc>)>(
                    "SELECT owner_id, created_at FROM rooms WHERE id = $1",
                )
                .bind(&room_id)
                .fetch_optional(&pool),
            )
            .await
            .map_err(|_| room_delete_timeout("checking ownership"))??;
            let Some((owner_id, created_at)) = generation else {
                return Ok(DeleteRoomResult::NotFound);
            };
            if owner_id != requester_id {
                return Ok(DeleteRoomResult::Forbidden);
            }

            let creation_guard = tokio::time::timeout(ROOM_DELETE_TIMEOUT, creation_lock.lock())
                .await
                .map_err(|_| room_delete_timeout("waiting for room creation"))?;
            let deletion_token = uuid::Uuid::new_v4();
            {
                let mut deleting = deleting_rooms
                    .write()
                    .unwrap_or_else(|error| error.into_inner());
                if deleting.contains_key(&room_id) {
                    return Ok(DeleteRoomResult::NotFound);
                }
                deleting.insert(room_id.clone(), deletion_token);
            }

            let runtime_room = rooms
                .read()
                .unwrap_or_else(|error| error.into_inner())
                .get(&room_id)
                .cloned();
            if let Some(runtime_room) = runtime_room.as_ref() {
                let mut room = match tokio::time::timeout(ROOM_DELETE_TIMEOUT, runtime_room.write())
                    .await
                {
                    Ok(room) => room,
                    Err(_) => {
                        release_deletion_reservation(&deleting_rooms, &room_id, deletion_token);
                        return Err(room_delete_timeout("waiting to tombstone the runtime room"));
                    }
                };
                room.deleting = true;
                room.policy_revision = room.policy_revision.wrapping_add(1);
            }

            let deleted = tokio::time::timeout(
                ROOM_DELETE_TIMEOUT,
                sqlx::query(
                    "DELETE FROM rooms \
                     WHERE id = $1 AND owner_id = $2 AND created_at = $3",
                )
                .bind(&room_id)
                .bind(requester_id)
                .bind(created_at)
                .execute(&pool),
            )
            .await;
            let deleted = match deleted {
                Ok(Ok(result)) => result.rows_affected() == 1,
                Ok(Err(error)) => {
                    drop(creation_guard);
                    if matches!(
                        &error,
                        sqlx::Error::Database(_)
                            | sqlx::Error::PoolTimedOut
                            | sqlx::Error::PoolClosed
                    ) {
                        // PostgreSQL rejected the statement (or no connection
                        // was acquired), so no commit is possible.
                        restore_failed_deletion(
                            runtime_room,
                            deleting_rooms,
                            room_id,
                            deletion_token,
                        )
                        .await;
                    } else {
                        // I/O/protocol failures can make an autocommit outcome
                        // unknowable. Keep the room reserved and tombstoned
                        // rather than reviving a runtime room whose DB row may
                        // already be gone.
                        warn!(room_id, %error, "Room delete outcome is indeterminate; retaining tombstone");
                    }
                    return Err(error);
                }
                Err(_) => {
                    drop(creation_guard);
                    // Dropping a timed-out query future cannot prove that
                    // PostgreSQL did not commit. Fail closed until process
                    // reconciliation/restart rather than creating a zombie.
                    warn!(
                        room_id,
                        "Room delete timed out; retaining tombstone because commit status is unknown"
                    );
                    return Err(room_delete_timeout("deleting the database row"));
                }
            };
            if !deleted {
                drop(creation_guard);
                restore_failed_deletion(runtime_room, deleting_rooms, room_id, deletion_token)
                    .await;
                return Ok(DeleteRoomResult::NotFound);
            }

            // The reservation blocks same-ID creation. Release the global
            // creation mutex before waiting for a busy room or mediasoup IPC.
            drop(creation_guard);

            let participant_sessions = if let Some(runtime_room) = runtime_room.as_ref() {
                let mut room =
                    match tokio::time::timeout(ROOM_DELETE_TIMEOUT, runtime_room.write()).await {
                        Ok(room) => room,
                        Err(_) => {
                            // The database row is already gone. Keep the runtime
                            // tombstoned and the ID reserved rather than leak a
                            // permanently waiting teardown task.
                            warn!(
                                room_id,
                                "Timed out draining deleted room; retaining room-ID reservation"
                            );
                            return Ok(DeleteRoomResult::Deleted);
                        }
                    };
                room.broadcast_all(&ServerMessage::Error {
                    message: "Room was deleted by its owner".to_string(),
                });
                if let Ok(message) = serde_json::to_string(&ServerMessage::Error {
                    message: "Room was deleted by its owner".to_string(),
                }) {
                    let message = Arc::new(message);
                    for entry in room.lobby.values() {
                        let _ = entry.sender.try_send(message.clone());
                    }
                }
                let participant_sessions: Vec<(String, uuid::Uuid)> = room
                    .participants
                    .values()
                    .map(|participant| (participant.id.clone(), participant.media_session_id))
                    .collect();
                room.participants.clear();
                room.lobby.clear();
                room.producer_to_participant.clear();
                participant_sessions
            } else {
                Vec::new()
            };

            if let Some(runtime_room) = runtime_room.as_ref() {
                let mut rooms = rooms.write().unwrap_or_else(|error| error.into_inner());
                if rooms
                    .get(&room_id)
                    .is_some_and(|candidate| Arc::ptr_eq(candidate, runtime_room))
                {
                    rooms.remove(&room_id);
                }
            }

            let participant_cleanup = participant_sessions.into_iter().map(
                |(participant_id, media_session_id)| {
                    let media_server = media_server.clone();
                    let room_id = room_id.clone();
                    async move {
                        let media_participant_id = RoomManager::media_participant_id(
                            &room_id,
                            &participant_id,
                            media_session_id,
                        );
                        if let Err(error) = media_server
                            .transport_manager()
                            .remove_participant(&media_participant_id)
                            .await
                        {
                            warn!(room_id, %participant_id, %error, "Failed to tear down deleted-room participant media");
                        }
                    }
                },
            );
            if tokio::time::timeout(
                ROOM_DELETE_TIMEOUT,
                futures_util::future::join_all(participant_cleanup),
            )
            .await
            .is_err()
            {
                warn!(
                    room_id,
                    "Timed out tearing down deleted-room participant media"
                );
            }
            let router_teardown_succeeded = if runtime_room.is_some() {
                match tokio::time::timeout(
                    ROOM_DELETE_TIMEOUT,
                    media_server.remove_router(&room_id),
                )
                .await
                {
                    Ok(Ok(())) => true,
                    Ok(Err(error)) => {
                        // The DB row is gone, but releasing the ID while the old
                        // mediasoup router remains could make a same-ID room
                        // recreation fail or attach to stale infrastructure.
                        // Fail closed until restart/reconciliation can prove the
                        // router is gone.
                        warn!(room_id, %error, "Failed to tear down deleted-room router; retaining room-ID reservation");
                        false
                    }
                    Err(_) => {
                        warn!(
                            room_id,
                            "Timed out tearing down deleted-room router; retaining room-ID reservation"
                        );
                        false
                    }
                }
            } else {
                true
            };

            release_deletion_reservation_after_router_teardown(
                &deleting_rooms,
                &room_id,
                deletion_token,
                router_teardown_succeeded,
            );
            Ok(DeleteRoomResult::Deleted)
        });

        deletion
            .await
            .map_err(|error| sqlx::Error::Protocol(format!("room deletion task failed: {error}")))?
    }

    fn media_participant_id(
        room_id: &str,
        participant_id: &str,
        media_session_id: uuid::Uuid,
    ) -> String {
        format!("{room_id}\u{1f}{media_session_id}\u{1f}{participant_id}")
    }

    /// Gets a room lock by ID (brief outer read lock, no await)
    fn get_room(&self, room_id: &str) -> Result<Arc<TokioRwLock<Room>>> {
        let rooms = self.rooms.read().unwrap_or_else(|e| e.into_inner());
        rooms
            .get(room_id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Room not found: {room_id}"))
    }

    fn failed_join_cleanup(&self) -> FailedJoinCleanup {
        FailedJoinCleanup {
            rooms: self.rooms.clone(),
            deleting_rooms: self.deleting_rooms.clone(),
            room_creation_lock: self.room_creation_lock.clone(),
            media_server: self.media_server.clone(),
        }
    }

    async fn reserve_existing_room_join(
        &self,
        room_id: &str,
        room_lock: Arc<TokioRwLock<Room>>,
    ) -> Result<PendingRoomJoin> {
        let mut room = tokio::time::timeout(ROOM_CREATION_TIMEOUT, room_lock.write())
            .await
            .map_err(|_| anyhow::anyhow!("Room is busy; try again"))?;
        room.ensure_live()?;
        room.pending_joins = room
            .pending_joins
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("Room join capacity has been reached"))?;
        drop(room);
        Ok(PendingRoomJoin {
            room_id: room_id.to_string(),
            room: room_lock,
            cleanup: self.failed_join_cleanup(),
            active: true,
        })
    }

    /// Gets or creates a room, creating a router if needed
    async fn get_or_create_room(&self, room_id: &str) -> Result<PendingRoomJoin> {
        if !settings::valid_room_id(room_id) {
            anyhow::bail!("Invalid room ID");
        }
        if self
            .deleting_rooms
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .contains_key(room_id)
        {
            anyhow::bail!("Room is being deleted");
        }
        // Fast path: room exists (brief outer read lock)
        let existing_room = {
            let rooms = self.rooms.read().unwrap_or_else(|e| e.into_inner());
            let existing = rooms.get(room_id).cloned();
            if existing.is_none() && rooms.len() >= self.max_rooms {
                anyhow::bail!("Room capacity has been reached");
            }
            existing
        };
        if let Some(room) = existing_room {
            return self.reserve_existing_room_join(room_id, room).await;
        }

        // Serialize creation: a concurrent creator may be mid-way between
        // create_router and inserting into the rooms map.
        let _creation_guard =
            tokio::time::timeout(ROOM_CREATION_TIMEOUT, self.room_creation_lock.lock())
                .await
                .map_err(|_| anyhow::anyhow!("Room creation is busy; try again"))?;

        if self
            .deleting_rooms
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .contains_key(room_id)
        {
            anyhow::bail!("Room is being deleted");
        }

        // Re-check now that we hold the creation lock
        let existing_room = {
            let rooms = self.rooms.read().unwrap_or_else(|e| e.into_inner());
            let existing = rooms.get(room_id).cloned();
            if existing.is_none() && rooms.len() >= self.max_rooms {
                anyhow::bail!("Room capacity has been reached");
            }
            existing
        };
        if let Some(room) = existing_room {
            drop(_creation_guard);
            return self.reserve_existing_room_join(room_id, room).await;
        }

        // Load security-relevant state before creating any runtime room. A DB
        // outage must not turn a protected persisted room into an ad-hoc room.
        let (room_settings, password_hash) = if let Some(pool) = &self.db_pool {
            let loaded =
                tokio::time::timeout(ROOM_CREATION_TIMEOUT, settings::load_room(pool, room_id))
                    .await
                    .map_err(|_| anyhow::anyhow!("Room data is temporarily unavailable"))?
                    .map_err(|error| {
                        warn!(room_id, %error, "Failed to load room");
                        anyhow::anyhow!("Room data is temporarily unavailable")
                    })?;
            loaded.map_or((None, None), |(settings, hash)| (Some(settings), hash))
        } else {
            (None, None)
        };
        let persisted = room_settings.is_some();
        if !persisted && !self.allow_ad_hoc_rooms {
            anyhow::bail!("Ad-hoc room creation is disabled");
        }

        // Slow path: router/observer setup remains serialized so duplicate
        // same-ID creators and the live-room cap stay atomic. Bound the entire
        // media phase; on timeout, best-effort removal prevents a half-created
        // router from poisoning the room ID after the lock is released.
        info!("Creating new room: {}", room_id);
        let media_setup = async {
            let router_id = self.media_server.create_router(room_id.to_string()).await?;
            let router = self
                .media_server
                .router_manager()
                .get_router(room_id)
                .await
                .map_err(|error| anyhow::anyhow!(error))?;

            let active_speaker_observer = match router
                .create_active_speaker_observer(ActiveSpeakerObserverOptions::default())
                .await
            {
                Ok(observer) => Some(observer),
                Err(error) => {
                    warn!(room_id, %error, "Failed to create active speaker observer");
                    None
                }
            };

            let mut options = AudioLevelObserverOptions::default();
            options.max_entries = std::num::NonZeroU16::new(10).unwrap();
            options.threshold = -50;
            options.interval = 800;
            let audio_level_observer = match router.create_audio_level_observer(options).await {
                Ok(observer) => Some(observer),
                Err(error) => {
                    warn!(room_id, %error, "Failed to create audio level observer");
                    None
                }
            };
            Ok::<_, anyhow::Error>((router_id, active_speaker_observer, audio_level_observer))
        };
        let (router_id, active_speaker_observer, audio_level_observer) =
            match tokio::time::timeout(ROOM_CREATION_TIMEOUT, media_setup).await {
                Ok(Ok(setup)) => setup,
                Ok(Err(error)) => {
                    let _ = tokio::time::timeout(
                        ROOM_DELETE_TIMEOUT,
                        self.media_server.remove_router(room_id),
                    )
                    .await;
                    return Err(error);
                }
                Err(_) => {
                    warn!(room_id, "Room media setup timed out; rolling back router");
                    let _ = tokio::time::timeout(
                        ROOM_DELETE_TIMEOUT,
                        self.media_server.remove_router(room_id),
                    )
                    .await;
                    anyhow::bail!("Room media service is temporarily unavailable");
                }
            };
        self.metrics.inc_rooms_created();

        // Create bounded channel for observer events (observer events are ephemeral UI hints)
        let (observer_tx, observer_rx) = tokio::sync::mpsc::channel::<ObserverEvent>(16);

        // Set up callbacks (use try_send — dropping stale events is fine).
        // .detach() is required: dropping the returned HandlerId unregisters the
        // callback, silently killing active-speaker/audio-level events.
        if let Some(obs) = &active_speaker_observer {
            let tx = observer_tx.clone();
            obs.on_dominant_speaker(move |speaker| {
                let _ = tx.try_send(ObserverEvent::ActiveSpeaker {
                    producer_id: speaker.producer.id(),
                });
            })
            .detach();
        }

        if let Some(obs) = &audio_level_observer {
            let tx = observer_tx.clone();
            obs.on_volumes(move |volumes| {
                let entries: Vec<_> = volumes
                    .iter()
                    .map(|v| (v.producer.id(), v.volume))
                    .collect();
                let _ = tx.try_send(ObserverEvent::AudioLevels { volumes: entries });
            })
            .detach();
        }
        drop(observer_tx); // Only clones in callbacks remain

        // Publish the initialized room while the creation mutex is still held.
        let room_arc = {
            let mut rooms = self.rooms.write().unwrap_or_else(|e| e.into_inner());
            // Every runtime-room insertion is serialized by
            // `room_creation_lock`, which is still held here.
            debug_assert!(!rooms.contains_key(room_id));
            let new_room = Arc::new(TokioRwLock::new(Room::new_with_observers(
                room_id.to_string(),
                router_id,
                room_settings,
                persisted,
                password_hash,
                active_speaker_observer,
                audio_level_observer,
            )));
            rooms.insert(room_id.to_string(), new_room.clone());
            new_room
        };

        // Spawn background task with Weak reference (only for newly created rooms)
        let weak_room = Arc::downgrade(&room_arc);
        tokio::spawn(Self::observer_broadcast_task(observer_rx, weak_room));

        Ok(PendingRoomJoin {
            room_id: room_id.to_string(),
            room: room_arc,
            cleanup: self.failed_join_cleanup(),
            active: true,
        })
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
                    if let Some(participant_id) =
                        room.producer_to_participant.get(&producer_id.to_string())
                    {
                        room.broadcast_all(&ServerMessage::ActiveSpeaker {
                            participant_id: participant_id.clone(),
                        });
                    }
                }
                ObserverEvent::AudioLevels { volumes } => {
                    let levels: Vec<AudioLevelEntry> = volumes
                        .iter()
                        .filter_map(|(pid, vol)| {
                            room.producer_to_participant.get(&pid.to_string()).map(
                                |participant_id| AudioLevelEntry {
                                    participant_id: participant_id.clone(),
                                    volume: *vol,
                                },
                            )
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
    #[allow(clippy::too_many_arguments)]
    pub async fn add_participant(
        &self,
        room_id: &str,
        participant_id: String,
        participant_name: String,
        sender: mpsc::Sender<Arc<String>>,
        authenticated: bool,
        in_lobby_flag: Arc<AtomicBool>,
        password: Option<&str>,
        reconnect_token: &str,
        client_ip: Option<std::net::IpAddr>,
    ) -> Result<JoinResult> {
        let client_ip = if authenticated {
            client_ip
        } else {
            client_ip.map(moderation::canonical_guest_ip)
        };
        if let Some(address) = client_ip {
            let address = rate_limit_ip(address);
            let now = std::time::Instant::now();
            if !self.join_attempts_by_ip.allow(address, now)
                || !self
                    .join_attempts_by_room_ip
                    .allow((room_id.to_string(), address), now)
            {
                anyhow::bail!("Room join attempts are rate limited");
            }
        }

        let mut pending_join = self.get_or_create_room(room_id).await?;
        let room_lock = pending_join.room();

        // Brief read lock to check conditions for role resolution
        let (
            lobby_enabled,
            invite_only,
            settings_owner_id,
            password_protected,
            password_hash,
            persisted,
            policy_revision,
        ) = {
            let room = room_lock.read().await;
            let lobby = room.settings.as_ref().map_or(false, |s| s.lobby_enabled);
            let invite_only = room.settings.as_ref().is_some_and(|s| s.invite_only);
            let owner = room.settings.as_ref().map(|s| s.owner_id);
            let pw = room
                .settings
                .as_ref()
                .map_or(false, |s| s.password_protected);
            (
                lobby,
                invite_only,
                owner,
                pw,
                room.password_hash.clone(),
                room.persisted,
                room.policy_revision,
            )
        }; // read lock released

        let user_uuid = if authenticated {
            Some(
                participant_id
                    .parse::<uuid::Uuid>()
                    .map_err(|_| anyhow::anyhow!("Invalid authenticated participant identity"))?,
            )
        } else {
            None
        };

        // Persistent ban check (registered users by id, guests by IP)
        if persisted {
            if let Some(pool) = &self.db_pool {
                match moderation::is_banned(pool, room_id, user_uuid, client_ip, authenticated)
                    .await
                {
                    Ok(true) => anyhow::bail!("You are banned from this room"),
                    Ok(false) => {}
                    Err(error) => {
                        warn!(room_id, %error, "Ban check failed");
                        anyhow::bail!("Room access could not be verified");
                    }
                }
            } else {
                anyhow::bail!("Room access could not be verified");
            }
        }

        // Every join to a persisted room resolves its role from the DB. In
        // particular, a temporarily empty runtime room does not make its first
        // connection the owner.
        let resolved_role = if persisted {
            let pool = self
                .db_pool
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("Room access could not be verified"))?;
            let owner_id = settings_owner_id
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("Room access could not be verified"))?;
            Some(
                roles::resolve_role(pool, room_id, user_uuid.as_ref(), owner_id, authenticated)
                    .await
                    .map_err(|error| {
                        warn!(room_id, %error, "Role lookup failed");
                        anyhow::anyhow!("Room access could not be verified")
                    })?,
            )
        } else {
            None
        };

        // Persisted sanctions are part of join authorization. Treat a missing
        // pool or a lookup failure as a failed join rather than silently
        // granting an unsanctioned session during a database outage.
        let persisted_punitive = if persisted {
            let pool = self
                .db_pool
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("Room access could not be verified"))?;
            moderation::load_punitive_state(pool, room_id, user_uuid, client_ip, authenticated)
                .await
                .map_err(|error| {
                    warn!(room_id, %error, "Sanction lookup failed");
                    anyhow::anyhow!("Room access could not be verified")
                })?
        } else {
            moderation::PunitiveState::default()
        };

        // Password gate: everyone below Admin must present the room password
        if password_protected && !resolved_role.map_or(false, |r| r >= roles::Role::Admin) {
            let hash =
                password_hash.ok_or_else(|| anyhow::anyhow!("Room password is unavailable"))?;
            let supplied = password
                .ok_or_else(|| anyhow::anyhow!("This room requires a password"))?
                .to_owned();
            let permit = tokio::time::timeout(
                PASSWORD_VERIFY_QUEUE_TIMEOUT,
                self.password_verify_work.clone().acquire_owned(),
            )
            .await
            .map_err(|_| anyhow::anyhow!("Password verification is busy; try again"))?
            .map_err(|_| anyhow::anyhow!("Password verification is unavailable"))?;
            let verified = tokio::task::spawn_blocking(move || {
                let _permit = permit;
                crate::auth::password::verify_password(&supplied, &hash).unwrap_or(false)
            })
            .await
            .map_err(|error| {
                warn!(room_id, %error, "Password verification task failed");
                anyhow::anyhow!("Password verification failed")
            })?;
            if !verified {
                anyhow::bail!("Incorrect room password");
            }
        }

        // A fresh media namespace is assigned to every successful room
        // membership. It deliberately survives lobby admission and reconnect,
        // but never a leave/kick followed by a new join with the same ID.
        let media_session_id = uuid::Uuid::new_v4();

        // Now acquire write lock for mutation
        let mut room = room_lock.write().await;

        if !policy_snapshot_matches(&room, policy_revision) {
            anyhow::bail!("Room policy changed while joining; retry");
        }

        // Check ban list before allowing join
        let now = std::time::Instant::now();
        room.prune_expired_bans(now);
        if room.identity_is_banned(&participant_id, authenticated, client_ip) {
            anyhow::bail!("You are banned from this room");
        }
        if room.participants.contains_key(&participant_id)
            || room.lobby.contains_key(&participant_id)
        {
            anyhow::bail!("This participant already has an active room session");
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
                let Ok(max) = usize::try_from(max) else {
                    anyhow::bail!("Room capacity is unavailable");
                };
                if max == 0 || room.participants.len() >= max {
                    anyhow::bail!("Room is full");
                }
            }
        }

        if room.lobby.len() >= 1_000 {
            anyhow::bail!("Room lobby is full");
        }

        // Re-check is_first under write lock (another join could have raced)
        let is_first = room.participants.is_empty() && room.lobby.is_empty();

        // Determine final role
        let role = select_join_role(persisted, is_first, resolved_role, authenticated)
            .ok_or_else(|| anyhow::anyhow!("Room access could not be verified"))?;
        let sanction_key = SanctionKey::for_identity(&participant_id, authenticated, client_ip);
        // A sanction may have been installed after the DB snapshot but before
        // this join acquired the room lock. Restrictions only merge in the safe
        // direction.
        let punitive = merge_punitive_state(persisted_punitive, room.sanctions.get(&sanction_key));
        if punitive != moderation::PunitiveState::default() {
            room.sanctions.insert(sanction_key, punitive.clone());
        }

        // Invite-only rooms treat a persisted Member+ role as the backwards-
        // compatible invitation. Unknown users wait for an explicit moderator
        // admission even when the ordinary lobby toggle is off.
        let must_wait_for_invite = invite_only && role < roles::Role::Member;
        let ordinary_lobby = lobby_enabled && !is_first && role < roles::Role::Moderator;
        if must_wait_for_invite || ordinary_lobby {
            // Place in lobby
            let room_name = room
                .settings
                .as_ref()
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
            room.broadcast_to_role(
                roles::Role::Moderator,
                &ServerMessage::LobbyJoin {
                    participant_id: participant_id.clone(),
                    display_name: participant_name.clone(),
                    authenticated,
                },
            );

            // Add to lobby map (reconnect_token set later by connection handler)
            room.lobby.insert(
                participant_id.clone(),
                LobbyEntry {
                    participant_id: participant_id.clone(),
                    name: participant_name.clone(),
                    sender,
                    media_session_id,
                    authenticated,
                    reconnect_token: reconnect_token.to_string(),
                    in_lobby_flag,
                    ip: client_ip,
                    role,
                    punitive,
                },
            );
            pending_join.complete_locked(&mut room);

            info!(
                "Participant {} ({}) entered lobby for room {}",
                participant_id, participant_name, room_id
            );
            return Ok(JoinResult::Lobbied);
        }

        let participant = Participant {
            id: participant_id.clone(),
            name: participant_name.clone(),
            sender,
            media_session_id,
            producers: HashMap::new(),
            role,
            punitive,
            authenticated,
            ip: client_ip,
        };

        room.participants
            .insert(participant_id.clone(), participant);
        pending_join.complete_locked(&mut room);

        info!(
            "Participant {} ({}) joined room {}",
            participant_id, participant_name, room_id
        );

        // Notify other participants
        room.broadcast_except(
            &participant_id,
            &ServerMessage::ParticipantJoined {
                participant_id: participant_id.clone(),
                participant_name,
                role: role.name().to_string(),
                authenticated,
            },
        );

        // Return list of existing participants
        let participants: Vec<ParticipantInfo> = room
            .participants
            .values()
            .filter(|p| p.id != participant_id)
            .map(|p| ParticipantInfo {
                id: p.id.clone(),
                name: p.name.clone(),
                producers: p
                    .producers
                    .iter()
                    .map(|(id, (kind, source))| ProducerMetadata {
                        id: id.clone(),
                        kind: *kind,
                        source: source.clone(),
                    })
                    .collect(),
                role: p.role.name().to_string(),
            })
            .collect();

        // Serialize room settings for the joining client
        let room_settings = room
            .settings
            .as_ref()
            .and_then(|s| serde_json::to_value(s).ok());

        Ok(JoinResult::Joined {
            participants,
            role: role.name().to_string(),
            room_settings,
        })
    }

    /// Removes a participant from a room
    ///
    /// # Errors
    /// Returns an error if cleanup operations fail
    pub async fn remove_participant(&self, room_id: &str, participant_id: &str) -> Result<()> {
        self.remove_participant_inner(room_id, participant_id, None)
            .await
            .map(|_| ())
    }

    /// Removes a participant only if this sender is still the authoritative
    /// connection for the room membership. Used by socket leave/disconnect and
    /// grace timers so an old socket cannot evict a same-ID replacement.
    pub async fn remove_participant_for_sender(
        &self,
        room_id: &str,
        participant_id: &str,
        expected_sender: &mpsc::Sender<Arc<String>>,
    ) -> Result<bool> {
        self.remove_participant_inner(room_id, participant_id, Some(expected_sender))
            .await
    }

    async fn remove_participant_inner(
        &self,
        room_id: &str,
        participant_id: &str,
        expected_sender: Option<&mpsc::Sender<Arc<String>>>,
    ) -> Result<bool> {
        let mut removed = false;
        let mut removed_any = false;
        let mut room_empty = false;
        let mut media_session_id = None;
        let mut audio_producer_ids: Vec<String> = Vec::new();
        let mut active_obs = None;
        let mut audio_obs = None;

        // Get room lock (brief outer read)
        let room_lock = {
            let rooms = self.rooms.read().unwrap_or_else(|e| e.into_inner());
            match rooms.get(room_id) {
                Some(r) => r.clone(),
                None => return Ok(false),
            }
        };

        // Lock only this room
        {
            let mut room = room_lock.write().await;

            if let Some(expected_sender) = expected_sender {
                let sender_matches = room
                    .participants
                    .get(participant_id)
                    .is_some_and(|participant| participant.sender.same_channel(expected_sender))
                    || room
                        .lobby
                        .get(participant_id)
                        .is_some_and(|entry| entry.sender.same_channel(expected_sender));
                if !sender_matches {
                    return Ok(false);
                }
            }

            // Also remove from lobby if present (lobby participants have no media to clean up)
            if room.lobby.remove(participant_id).is_some() {
                removed_any = true;
                info!(
                    "Lobby participant {} removed from room {}",
                    participant_id, room_id
                );
                room_empty = room.participants.is_empty() && room.lobby.is_empty();
                // No media cleanup needed for lobby participants
            }

            if let Some(participant) = room.participants.remove(participant_id) {
                removed = true;
                removed_any = true;
                media_session_id = Some(participant.media_session_id);
                info!("Participant {} left room {}", participant_id, room_id);

                // Collect audio producer IDs for observer cleanup
                for (pid, (kind, _)) in &participant.producers {
                    if *kind == MediaKind::Audio {
                        audio_producer_ids.push(pid.clone());
                    }
                }

                // Remove every producer from the room ownership index. Audio
                // producers are additionally removed from observers below.
                for pid in participant.producers.keys() {
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

        if removed {
            // Close transports, producers, consumers — releases FDs
            let media_participant_id = Self::media_participant_id(
                room_id,
                participant_id,
                media_session_id.expect("removed participants have a media session"),
            );
            if let Err(e) = self
                .media_server
                .transport_manager()
                .remove_participant(&media_participant_id)
                .await
            {
                warn!(
                    "Failed to clean up media for participant {}: {}",
                    participant_id, e
                );
            }
        }

        if room_empty {
            // Serialize the final empty-room check, map removal, and router
            // removal against room creation. This also cleans routers for
            // lobby-only rooms and prevents deleting a router just recreated
            // by a concurrent join.
            let creation_guard =
                match tokio::time::timeout(ROOM_CREATION_TIMEOUT, self.room_creation_lock.lock())
                    .await
                {
                    Ok(guard) => guard,
                    Err(_) => {
                        warn!(room_id, "Timed out waiting to evict an empty room");
                        return Ok(removed_any);
                    }
                };
            let mut room = match tokio::time::timeout(ROOM_DELETE_TIMEOUT, room_lock.write()).await
            {
                Ok(room) => room,
                Err(_) => {
                    warn!(
                        room_id,
                        "Timed out rechecking an empty room; leaving it active"
                    );
                    return Ok(removed_any);
                }
            };
            let still_mapped = self
                .rooms
                .read()
                .unwrap_or_else(|error| error.into_inner())
                .get(room_id)
                .is_some_and(|candidate| Arc::ptr_eq(candidate, &room_lock));
            let should_remove = still_mapped
                && !room.deleting
                && room.pending_joins == 0
                && room.participants.is_empty()
                && room.lobby.is_empty();
            let eviction_token = if should_remove {
                // Invalidate any join that cloned this Arc before we remove it.
                room.deleting = true;
                room.policy_revision = room.policy_revision.wrapping_add(1);
                let deletion_token = uuid::Uuid::new_v4();
                self.deleting_rooms
                    .write()
                    .unwrap_or_else(|error| error.into_inner())
                    .insert(room_id.to_string(), deletion_token);
                let mut rooms = self.rooms.write().unwrap_or_else(|e| e.into_inner());
                if rooms
                    .get(room_id)
                    .is_some_and(|candidate| Arc::ptr_eq(candidate, &room_lock))
                {
                    rooms.remove(room_id);
                    Some(deletion_token)
                } else {
                    release_deletion_reservation(&self.deleting_rooms, room_id, deletion_token);
                    None
                }
            } else {
                None
            };
            drop(room);
            drop(creation_guard);
            if let Some(deletion_token) = eviction_token {
                let router_teardown_succeeded = match tokio::time::timeout(
                    ROOM_DELETE_TIMEOUT,
                    self.media_server.remove_router(room_id),
                )
                .await
                {
                    Ok(Ok(())) => {
                        info!("Room {} is empty, cleaned up router", room_id);
                        true
                    }
                    Ok(Err(error)) => {
                        warn!(room_id, %error, "Failed to tear down empty-room router; retaining room-ID reservation");
                        false
                    }
                    Err(_) => {
                        warn!(
                            room_id,
                            "Timed out tearing down empty-room router; retaining room-ID reservation"
                        );
                        false
                    }
                };
                release_deletion_reservation_after_router_teardown(
                    &self.deleting_rooms,
                    room_id,
                    deletion_token,
                    router_teardown_succeeded,
                );
            }
        }

        Ok(removed_any)
    }

    /// Gets router RTP capabilities for a room
    ///
    /// # Errors
    /// Returns an error if the room doesn't exist
    pub async fn get_router_rtp_capabilities(
        &self,
        room_id: &str,
        participant_id: &str,
        expected_sender: &mpsc::Sender<Arc<String>>,
    ) -> Result<RtpCapabilitiesFinalized> {
        self.media_session_for_sender(room_id, participant_id, expected_sender)
            .await?;
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
        expected_sender: &mpsc::Sender<Arc<String>>,
    ) -> Result<TransportInfo> {
        let media_session_id = self
            .media_session_for_sender(room_id, participant_id, expected_sender)
            .await?;
        let router = self.get_router(room_id).await?;
        let webrtc_server = self
            .media_server
            .get_webrtc_server_for_room(room_id)
            .await?;
        let media_participant_id =
            Self::media_participant_id(room_id, participant_id, media_session_id);
        let media_config = self.media_server.config();
        let _ipc_reservation = self
            .reserve_media_control_ipc_for_sender(room_id, participant_id, expected_sender)
            .await?;
        let transport = self
            .media_server
            .transport_manager()
            .create_send_transport(
                media_participant_id.clone(),
                &router,
                webrtc_server,
                &media_config.webrtc_transport_config,
            )
            .await
            .map_err(|e| anyhow::anyhow!(e))?;
        if !self
            .is_current_media_session(room_id, participant_id, expected_sender, media_session_id)
            .await
        {
            let _ = self
                .media_server
                .transport_manager()
                .remove_participant(&media_participant_id)
                .await;
            anyhow::bail!("Participant is no longer in this room");
        }
        Ok(transport)
    }

    /// Creates a receive transport for a participant
    ///
    /// # Errors
    /// Returns an error if transport creation fails
    pub async fn create_recv_transport(
        &self,
        room_id: &str,
        participant_id: &str,
        expected_sender: &mpsc::Sender<Arc<String>>,
    ) -> Result<TransportInfo> {
        let media_session_id = self
            .media_session_for_sender(room_id, participant_id, expected_sender)
            .await?;
        let router = self.get_router(room_id).await?;
        let webrtc_server = self
            .media_server
            .get_webrtc_server_for_room(room_id)
            .await?;
        let media_participant_id =
            Self::media_participant_id(room_id, participant_id, media_session_id);
        let media_config = self.media_server.config();
        let _ipc_reservation = self
            .reserve_media_control_ipc_for_sender(room_id, participant_id, expected_sender)
            .await?;
        let transport = self
            .media_server
            .transport_manager()
            .create_recv_transport(
                media_participant_id.clone(),
                &router,
                webrtc_server,
                &media_config.webrtc_transport_config,
            )
            .await
            .map_err(|e| anyhow::anyhow!(e))?;
        if !self
            .is_current_media_session(room_id, participant_id, expected_sender, media_session_id)
            .await
        {
            let _ = self
                .media_server
                .transport_manager()
                .remove_participant(&media_participant_id)
                .await;
            anyhow::bail!("Participant is no longer in this room");
        }
        Ok(transport)
    }

    /// Connects a transport with DTLS parameters
    ///
    /// # Errors
    /// Returns an error if connection fails
    pub async fn connect_transport(
        &self,
        room_id: &str,
        participant_id: &str,
        expected_sender: &mpsc::Sender<Arc<String>>,
        transport_id: &str,
        dtls_parameters: DtlsParameters,
    ) -> Result<()> {
        let media_session_id = self
            .media_session_for_sender(room_id, participant_id, expected_sender)
            .await?;
        let media_participant_id =
            Self::media_participant_id(room_id, participant_id, media_session_id);
        let ipc_reservation = self
            .reserve_media_control_ipc_for_sender(room_id, participant_id, expected_sender)
            .await?;
        let connected = self
            .media_server
            .transport_manager()
            .connect_transport(&media_participant_id, transport_id, dtls_parameters)
            .await
            .map_err(|e| anyhow::anyhow!(e))?;

        if !connected {
            Self::refund_media_control_ipc(ipc_reservation).await;
        }

        if connected {
            debug!(
                "Connected transport {} for participant {}",
                transport_id, participant_id
            );
        }
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
        expected_sender: &mpsc::Sender<Arc<String>>,
        kind: MediaKind,
        rtp_parameters: RtpParameters,
        source: Option<String>,
    ) -> Result<String> {
        let source = source
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("Media source is required"))?;
        if !Self::valid_media_source(kind, source) {
            anyhow::bail!("Media kind does not match source");
        }
        if !self
            .can_participant_produce(room_id, participant_id, expected_sender, kind, source)
            .await?
        {
            anyhow::bail!("You are not allowed to produce this media type");
        }
        let _reservation = self
            .reserve_media_mutation_for_sender(room_id, participant_id, expected_sender, None)
            .await?;

        // Create producer WITHOUT room lock
        let media_session_id = self
            .media_session_for_sender(room_id, participant_id, expected_sender)
            .await?;
        let media_participant_id =
            Self::media_participant_id(room_id, participant_id, media_session_id);
        let producer = self
            .media_server
            .transport_manager()
            .create_producer(
                &media_participant_id,
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
            let room_lock = match self.get_room(room_id) {
                Ok(room) => room,
                Err(error) => {
                    let _ = self
                        .media_server
                        .transport_manager()
                        .close_producer(&media_participant_id, &producer_id)
                        .await;
                    return Err(error);
                }
            };
            let mut room = room_lock.write().await;
            if let Err(error) = room.ensure_live() {
                drop(room);
                let _ = self
                    .media_server
                    .transport_manager()
                    .close_producer(&media_participant_id, &producer_id)
                    .await;
                return Err(error);
            }
            let allowed = room
                .participants
                .get(participant_id)
                .is_some_and(|participant| {
                    participant.media_session_id == media_session_id
                        && participant.sender.same_channel(expected_sender)
                        && Self::participant_can_produce(&room, participant, kind, source)
                });
            if !allowed {
                drop(room);
                let _ = self
                    .media_server
                    .transport_manager()
                    .close_producer(&media_participant_id, &producer_id)
                    .await;
                anyhow::bail!("You are no longer allowed to produce this media type");
            }

            let participant = room.participants.get_mut(participant_id).unwrap();
            participant
                .producers
                .insert(producer_id.clone(), (kind, Some(source.to_string())));
            room.producer_to_participant
                .insert(producer_id.clone(), participant_id.to_string());

            room.broadcast_except(
                participant_id,
                &ServerMessage::NewProducer {
                    participant_id: participant_id.to_string(),
                    producer_id: producer_id.clone(),
                    kind,
                    source: Some(source.to_string()),
                },
            );

            // Clone observers before releasing lock
            (
                room.active_speaker_observer.clone(),
                room.audio_level_observer.clone(),
            )
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

        info!(
            "Created {:?} producer {} for participant {} in room {}",
            kind, producer_id, participant_id, room_id
        );

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
        expected_sender: &mpsc::Sender<Arc<String>>,
        producer_id: ProducerId,
        rtp_capabilities: RtpCapabilities,
        notification_sender: Option<mpsc::Sender<Arc<String>>>,
        paused: bool,
    ) -> MediaResult<crate::media::types::ConsumerInfo> {
        let media_session_id = self
            .media_session_for_sender(room_id, participant_id, expected_sender)
            .await
            .map_err(|error| crate::media::types::MediaError::ConsumerError(error.to_string()))?;
        {
            let room_lock = self.get_room(room_id).map_err(|error| {
                crate::media::types::MediaError::ConsumerError(error.to_string())
            })?;
            let room = room_lock.read().await;
            if !room
                .producer_to_participant
                .contains_key(&producer_id.to_string())
            {
                return Err(crate::media::types::MediaError::ConsumerError(
                    "Producer is not in this room".to_string(),
                ));
            }
        }
        let _ipc_reservation = self
            .reserve_media_control_ipc_for_sender(room_id, participant_id, expected_sender)
            .await
            .map_err(|error| crate::media::types::MediaError::ConsumerError(error.to_string()))?;
        // Look up the consumer counter for this room's worker (for load-aware tracking)
        let consumer_counter = self
            .media_server
            .get_consumer_counter_for_room(room_id)
            .await
            .ok()
            .flatten();

        // No room lock needed — purely transport_manager operation
        let media_participant_id =
            Self::media_participant_id(room_id, participant_id, media_session_id);
        let consumer = self
            .media_server
            .transport_manager()
            .create_consumer(
                &media_participant_id,
                producer_id,
                rtp_capabilities,
                AppData::default(),
                notification_sender,
                paused,
                consumer_counter,
            )
            .await?;

        if !self
            .is_current_media_session(room_id, participant_id, expected_sender, media_session_id)
            .await
        {
            let _ = self
                .media_server
                .transport_manager()
                .remove_participant(&media_participant_id)
                .await;
            return Err(crate::media::types::MediaError::ConsumerError(
                "Participant is no longer in this room".to_string(),
            ));
        }

        let consumer_info = crate::media::types::ConsumerInfo::from_consumer(&consumer, paused);

        debug!(
            "Created consumer {} for participant {} in room {}",
            consumer_info.id, participant_id, room_id
        );

        Ok(consumer_info)
    }

    /// Resumes a consumer for a participant
    pub async fn resume_consumer(
        &self,
        room_id: &str,
        participant_id: &str,
        expected_sender: &mpsc::Sender<Arc<String>>,
        consumer_id: &str,
    ) -> Result<()> {
        let media_session_id = self
            .media_session_for_sender(room_id, participant_id, expected_sender)
            .await?;
        let media_participant_id =
            Self::media_participant_id(room_id, participant_id, media_session_id);
        let ipc_reservation = self
            .reserve_media_control_ipc_for_sender(room_id, participant_id, expected_sender)
            .await?;
        let resumed = self
            .media_server
            .transport_manager()
            .resume_consumer(&media_participant_id, consumer_id)
            .await
            .map_err(|e| anyhow::anyhow!(e))?;

        if !resumed {
            Self::refund_media_control_ipc(ipc_reservation).await;
        }

        if resumed {
            debug!(
                "Resumed consumer {} for participant {}",
                consumer_id, participant_id
            );
        }
        Ok(())
    }

    /// Pauses a consumer for a participant
    pub async fn pause_consumer(
        &self,
        room_id: &str,
        participant_id: &str,
        expected_sender: &mpsc::Sender<Arc<String>>,
        consumer_id: &str,
    ) -> Result<()> {
        let media_session_id = self
            .media_session_for_sender(room_id, participant_id, expected_sender)
            .await?;
        let media_participant_id =
            Self::media_participant_id(room_id, participant_id, media_session_id);
        let ipc_reservation = self
            .reserve_media_control_ipc_for_sender(room_id, participant_id, expected_sender)
            .await?;
        let paused = self
            .media_server
            .transport_manager()
            .pause_consumer(&media_participant_id, consumer_id)
            .await
            .map_err(|e| anyhow::anyhow!(e))?;

        if !paused {
            Self::refund_media_control_ipc(ipc_reservation).await;
        }

        if paused {
            debug!(
                "Paused consumer {} for participant {}",
                consumer_id, participant_id
            );
        }
        Ok(())
    }

    /// Closes a producer for a participant
    pub async fn close_producer(
        &self,
        room_id: &str,
        participant_id: &str,
        expected_sender: &mpsc::Sender<Arc<String>>,
        producer_id: &str,
    ) -> Result<()> {
        let media_session_id = self
            .media_session_for_sender(room_id, participant_id, expected_sender)
            .await?;
        let media_participant_id =
            Self::media_participant_id(room_id, participant_id, media_session_id);
        let _reservation = self
            .reserve_media_mutation_for_sender(
                room_id,
                participant_id,
                expected_sender,
                Some(producer_id),
            )
            .await?;
        // Close producer WITHOUT room lock
        self.media_server
            .transport_manager()
            .close_producer(&media_participant_id, producer_id)
            .await
            .map_err(|e| anyhow::anyhow!(e))?;

        // Remove from room's participant tracking, observer maps, and notify others (per-room write lock)
        let (active_obs, audio_obs, was_audio) = {
            let room_lock = self.get_room(room_id)?;
            let mut room = room_lock.write().await;
            if !Self::participant_session_is_current(
                &room,
                participant_id,
                expected_sender,
                media_session_id,
            ) {
                anyhow::bail!("Participant is no longer in this room");
            }
            let was_audio = room
                .participants
                .get_mut(participant_id)
                .and_then(|participant| participant.producers.remove(producer_id))
                .is_some_and(|(kind, _)| kind == MediaKind::Audio);
            room.producer_to_participant.remove(producer_id);

            room.broadcast_except(
                participant_id,
                &ServerMessage::ProducerClosed {
                    producer_id: producer_id.to_string(),
                },
            );

            // Clone observers before releasing lock
            (
                room.active_speaker_observer.clone(),
                room.audio_level_observer.clone(),
                was_audio,
            )
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

        info!(
            "Closed producer {} for participant {} in room {}",
            producer_id, participant_id, room_id
        );
        Ok(())
    }

    /// Pauses a producer. Mediasoup propagates producer pause state to its
    /// consumers, so no server-wide consumer scan is needed.
    pub async fn pause_producer(
        &self,
        room_id: &str,
        participant_id: &str,
        expected_sender: &mpsc::Sender<Arc<String>>,
        producer_id: &str,
    ) -> Result<()> {
        let media_session_id = self
            .media_session_for_sender(room_id, participant_id, expected_sender)
            .await?;
        let media_participant_id =
            Self::media_participant_id(room_id, participant_id, media_session_id);
        let Some(reservation) = self
            .reserve_producer_pause_mutation_for_sender(
                room_id,
                participant_id,
                expected_sender,
                producer_id,
                true,
            )
            .await?
        else {
            return Ok(());
        };
        // Pause the producer itself
        let changed = self
            .media_server
            .transport_manager()
            .pause_producer(&media_participant_id, producer_id)
            .await
            .map_err(|e| anyhow::anyhow!(e))?;
        if !changed {
            Self::refund_media_mutation(reservation).await;
            return Ok(());
        }

        // Broadcast ProducerPaused to other participants in the room
        let room_lock = self.get_room(room_id)?;
        let room = room_lock.read().await;
        if !Self::participant_session_is_current(
            &room,
            participant_id,
            expected_sender,
            media_session_id,
        ) {
            anyhow::bail!("Participant is no longer in this room");
        }
        room.broadcast_except(
            participant_id,
            &ServerMessage::ProducerPaused {
                producer_id: producer_id.to_string(),
            },
        );

        info!(
            "Paused producer {} for participant {} in room {}",
            producer_id, participant_id, room_id
        );
        Ok(())
    }

    /// Resumes a producer. Mediasoup propagates producer resume state to its
    /// consumers, so no server-wide consumer scan is needed.
    pub async fn resume_producer(
        &self,
        room_id: &str,
        participant_id: &str,
        expected_sender: &mpsc::Sender<Arc<String>>,
        producer_id: &str,
    ) -> Result<()> {
        let media_session_id = self
            .media_session_for_sender(room_id, participant_id, expected_sender)
            .await?;
        let media_participant_id =
            Self::media_participant_id(room_id, participant_id, media_session_id);
        let Some(reservation) = self
            .reserve_producer_pause_mutation_for_sender(
                room_id,
                participant_id,
                expected_sender,
                producer_id,
                false,
            )
            .await?
        else {
            return Ok(());
        };
        // Resume the producer itself
        let changed = self
            .media_server
            .transport_manager()
            .resume_producer(&media_participant_id, producer_id)
            .await
            .map_err(|e| anyhow::anyhow!(e))?;
        if !changed {
            Self::refund_media_mutation(reservation).await;
            return Ok(());
        }

        // Broadcast ProducerResumed to other participants in the room
        let room_lock = self.get_room(room_id)?;
        let room = room_lock.read().await;
        if !Self::participant_session_is_current(
            &room,
            participant_id,
            expected_sender,
            media_session_id,
        ) {
            anyhow::bail!("Participant is no longer in this room");
        }
        room.broadcast_except(
            participant_id,
            &ServerMessage::ProducerResumed {
                producer_id: producer_id.to_string(),
            },
        );

        info!(
            "Resumed producer {} for participant {} in room {}",
            producer_id, participant_id, room_id
        );
        Ok(())
    }

    /// Sets preferred simulcast layers for a consumer
    pub async fn set_preferred_layers(
        &self,
        room_id: &str,
        participant_id: &str,
        expected_sender: &mpsc::Sender<Arc<String>>,
        consumer_id: &str,
        spatial_layer: u8,
        temporal_layer: Option<u8>,
    ) -> Result<()> {
        let media_session_id = self
            .media_session_for_sender(room_id, participant_id, expected_sender)
            .await?;
        let media_participant_id =
            Self::media_participant_id(room_id, participant_id, media_session_id);
        let ipc_reservation = self
            .reserve_media_control_ipc_for_sender(room_id, participant_id, expected_sender)
            .await?;
        let changed = self
            .media_server
            .transport_manager()
            .set_preferred_layers(
                &media_participant_id,
                consumer_id,
                ConsumerLayers {
                    spatial_layer,
                    temporal_layer,
                },
            )
            .await
            .map_err(|e| anyhow::anyhow!(e))?;
        if !changed {
            Self::refund_media_control_ipc(ipc_reservation).await;
        }
        Ok(())
    }

    /// Subscribes to BWE events for a participant's recv transport
    pub async fn subscribe_bwe_events(
        &self,
        room_id: &str,
        participant_id: &str,
        expected_sender: &mpsc::Sender<Arc<String>>,
        bwe_sender: mpsc::Sender<u32>,
    ) -> Result<()> {
        let media_session_id = self
            .media_session_for_sender(room_id, participant_id, expected_sender)
            .await?;
        let media_participant_id =
            Self::media_participant_id(room_id, participant_id, media_session_id);
        let _ipc_reservation = self
            .reserve_media_control_ipc_for_sender(room_id, participant_id, expected_sender)
            .await?;
        self.media_server
            .transport_manager()
            .subscribe_bwe_events(&media_participant_id, bwe_sender)
            .await
            .map_err(|e| anyhow::anyhow!(e))
    }

    /// Gets consumer IDs for a participant (no IPC — in-memory only)
    pub async fn get_consumer_ids(
        &self,
        room_id: &str,
        participant_id: &str,
        expected_sender: &mpsc::Sender<Arc<String>>,
    ) -> Result<Vec<String>> {
        let media_session_id = self
            .media_session_for_sender(room_id, participant_id, expected_sender)
            .await?;
        let media_participant_id =
            Self::media_participant_id(room_id, participant_id, media_session_id);
        self.media_server
            .transport_manager()
            .get_consumer_ids(&media_participant_id)
            .await
            .map_err(|e| anyhow::anyhow!(e))
    }

    /// Restarts ICE on a transport, returning new ICE parameters
    pub async fn restart_ice(
        &self,
        room_id: &str,
        participant_id: &str,
        expected_sender: &mpsc::Sender<Arc<String>>,
        transport_id: &str,
    ) -> Result<IceParameters> {
        let media_session_id = self
            .media_session_for_sender(room_id, participant_id, expected_sender)
            .await?;
        let media_participant_id =
            Self::media_participant_id(room_id, participant_id, media_session_id);
        let _ipc_reservation = self
            .reserve_media_control_ipc_for_sender(room_id, participant_id, expected_sender)
            .await?;
        self.media_server
            .transport_manager()
            .restart_ice(&media_participant_id, transport_id)
            .await
            .map_err(|e| anyhow::anyhow!(e))
    }

    /// Rebinds a participant's WS sender after reconnection
    pub async fn rebind_participant_sender(
        &self,
        room_id: &str,
        participant_id: &str,
        authenticated_subject: Option<&str>,
        expected_sender: &mpsc::Sender<Arc<String>>,
        new_sender: mpsc::Sender<Arc<String>>,
    ) -> Result<bool> {
        let room_lock = self.get_room(room_id)?;
        let mut room = room_lock.write().await;
        room.ensure_live()?;
        if let Some(participant) = room.participants.get_mut(participant_id) {
            if !participant.sender.same_channel(expected_sender) {
                warn!(
                    room_id,
                    participant_id, "Reconnect attempted against a superseded room session"
                );
                return Ok(false);
            }
            let identity_matches = match (participant.authenticated, authenticated_subject) {
                (true, Some(subject)) => subject == participant_id,
                (false, None) => true,
                _ => false,
            };
            if !identity_matches {
                warn!(
                    room_id,
                    participant_id, "Reconnect authentication identity mismatch"
                );
                return Ok(false);
            }
            participant.sender = new_sender;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    // === Room settings methods ===

    fn default_room_settings(room_id: &str) -> settings::RoomSettings {
        settings::RoomSettings {
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
        }
    }

    fn validate_capacity(label: &str, value: Option<Option<i32>>, maximum: i32) -> Result<()> {
        if let Some(Some(value)) = value {
            if !(1..=maximum).contains(&value) {
                anyhow::bail!("{label} must be between 1 and {maximum}");
            }
        }
        Ok(())
    }

    /// Update room settings at runtime (Admin+ only).
    ///
    /// Applies partial updates to the in-memory RoomSettings, broadcasts the change
    /// to all participants, and optionally persists to DB.
    #[allow(clippy::too_many_arguments)]
    pub async fn update_room_settings(
        &self,
        room_id: &str,
        admin_id: &str,
        expected_sender: &mpsc::Sender<Arc<String>>,
        moderated: Option<bool>,
        lobby_enabled: Option<bool>,
        guests_allowed: Option<bool>,
        guests_can_broadcast: Option<bool>,
        max_broadcasters: Option<Option<i32>>,
        max_participants: Option<Option<i32>>,
        allow_screen_sharing: Option<bool>,
        allow_chat: Option<bool>,
        allow_video: Option<bool>,
        require_registration: Option<bool>,
        invite_only: Option<bool>,
        push_to_talk: Option<bool>,
        secret: Option<bool>,
        password: Option<Option<String>>,
    ) -> Result<()> {
        // Authorize before doing expensive password work. Permissions are
        // checked again at commit time to handle concurrent role changes.
        let room_lock = self.get_room(room_id)?;
        {
            let room = room_lock.read().await;
            let admin = Self::participant_for_sender(&room, admin_id, expected_sender)?;
            if !admin.role.can_change_settings() {
                anyhow::bail!("Insufficient permissions to change room settings (requires Admin+)");
            }
        }

        Self::validate_capacity("max participants", max_participants, 10_000)?;
        Self::validate_capacity("max broadcasters", max_broadcasters, 1_000)?;
        let mut admin_mutation_reserved = false;
        if let Some(Some(value)) = password.as_ref() {
            if !settings::valid_new_room_password(value) {
                anyhow::bail!("Room password must be 8-256 bytes without control characters");
            }

            let mut room = room_lock.write().await;
            let admin = Self::participant_for_sender(&room, admin_id, expected_sender)?;
            if !admin.role.can_change_settings() {
                anyhow::bail!("Insufficient permissions to change room settings (requires Admin+)");
            }
            let now = std::time::Instant::now();
            if room
                .last_password_hash_at
                .is_some_and(|previous| now.duration_since(previous) < ROOM_PASSWORD_HASH_COOLDOWN)
            {
                anyhow::bail!("Room password updates are rate limited");
            }
            if !room.reserve_admin_mutation(now) {
                anyhow::bail!("Room administration changes are rate limited");
            }
            admin_mutation_reserved = true;
            room.last_password_hash_at = Some(now);
        }

        // Argon2 is deliberately expensive; keep it off the async executor and
        // bound concurrent work so settings updates cannot exhaust CPU threads.
        let password_hash = match password.as_ref() {
            Some(Some(value)) => {
                let value = value.clone();
                let permit = self
                    .password_hash_work
                    .clone()
                    .try_acquire_owned()
                    .map_err(|_| anyhow::anyhow!("Password service is busy; try again"))?;
                let hash = tokio::task::spawn_blocking(move || {
                    let _permit = permit;
                    crate::auth::password::hash_password(&value)
                })
                .await
                .map_err(|error| {
                    warn!(room_id, %error, "Password hash task failed");
                    anyhow::anyhow!("Password update failed")
                })?
                .map_err(|error| {
                    warn!(room_id, %error, "Password hashing failed");
                    anyhow::anyhow!("Password update failed")
                })?;
                Some(Some(hash))
            }
            Some(None) => Some(None),
            None => None,
        };

        let mut room = room_lock.write().await;
        let admin = Self::participant_for_sender(&room, admin_id, expected_sender)?;
        if !admin.role.can_change_settings() {
            anyhow::bail!("Insufficient permissions to change room settings (requires Admin+)");
        }

        let current = room
            .settings
            .clone()
            .unwrap_or_else(|| Self::default_room_settings(room_id));
        let password_changed = match password.as_ref() {
            Some(Some(_)) => true,
            Some(None) => room.password_hash.is_some() || current.password_protected,
            None => false,
        };
        let mut updated = current.clone();
        settings::apply_settings_update(
            &mut updated,
            moderated,
            lobby_enabled,
            guests_allowed,
            guests_can_broadcast,
            max_broadcasters,
            max_participants,
            allow_screen_sharing,
            allow_chat,
            allow_video,
            require_registration,
            invite_only,
            push_to_talk,
            secret,
            password.clone(),
        );
        if matches!(
            (updated.max_broadcasters, updated.max_participants),
            (Some(broadcasters), Some(participants)) if broadcasters > participants
        ) {
            anyhow::bail!("Maximum broadcasters cannot exceed maximum participants");
        }
        if !password_changed && updated == current {
            return Ok(());
        }
        if !admin_mutation_reserved && !room.reserve_admin_mutation(std::time::Instant::now()) {
            anyhow::bail!("Room administration changes are rate limited");
        }
        let settings_value = serde_json::to_value(&updated)
            .map_err(|_| anyhow::anyhow!("Room settings could not be serialized"))?;

        // For DB-backed rooms persistence is the commit point. Do not advertise
        // or enforce a setting that failed to persist.
        if room.persisted {
            let pool = self
                .db_pool
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("Room settings are temporarily unavailable"))?;
            settings::update_room_settings(
                pool,
                room_id,
                moderated,
                lobby_enabled,
                guests_allowed,
                guests_can_broadcast,
                max_broadcasters,
                max_participants,
                allow_screen_sharing,
                allow_chat,
                allow_video,
                require_registration,
                invite_only,
                push_to_talk,
                secret,
                password_hash.clone(),
            )
            .await
            .map_err(|error| {
                warn!(room_id, %error, "Failed to persist room settings");
                anyhow::anyhow!("Room settings could not be saved")
            })?;
        }

        if let Some(hash) = password_hash {
            room.password_hash = hash;
        }
        room.settings = Some(updated);
        room.policy_revision = room.policy_revision.wrapping_add(1);
        let revoked = Self::revoke_unauthorized_producers(
            &mut room,
            None,
            "Room policy no longer permits this producer",
        );
        let active_observer = room.active_speaker_observer.clone();
        let audio_observer = room.audio_level_observer.clone();
        room.broadcast_all(&ServerMessage::RoomSettingsChanged {
            settings: settings_value,
        });
        drop(room);

        self.close_revoked_producers(room_id, revoked, active_observer, audio_observer)
            .await;
        info!("Room settings updated for room {} by {}", room_id, admin_id);
        Ok(())
    }

    /// Set the room topic (Admin+ only).
    ///
    /// Updates the in-memory topic and broadcasts TopicChanged to all participants.
    pub async fn set_topic(
        &self,
        room_id: &str,
        admin_id: &str,
        expected_sender: &mpsc::Sender<Arc<String>>,
        topic: String,
    ) -> Result<()> {
        if topic.len() > 512 || topic.chars().any(char::is_control) {
            anyhow::bail!("Room topic must be at most 512 characters without control characters");
        }
        let room_lock = self.get_room(room_id)?;
        let mut room = room_lock.write().await;

        let admin = Self::participant_for_sender(&room, admin_id, expected_sender)?;
        if !admin.role.can_change_settings() {
            anyhow::bail!("Insufficient permissions to change room topic (requires Admin+)");
        }
        if room
            .settings
            .as_ref()
            .and_then(|settings| settings.topic.as_deref())
            == Some(topic.as_str())
        {
            return Ok(());
        }
        if !room.reserve_admin_mutation(std::time::Instant::now()) {
            anyhow::bail!("Room administration changes are rate limited");
        }

        if room.persisted {
            let pool = self
                .db_pool
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("Room topic is temporarily unavailable"))?;
            let result = sqlx::query("UPDATE rooms SET topic = $1 WHERE id = $2")
                .bind(&topic)
                .bind(room_id)
                .execute(pool)
                .await
                .map_err(|error| {
                    warn!(room_id, %error, "Failed to persist room topic");
                    anyhow::anyhow!("Room topic could not be saved")
                })?;
            if result.rows_affected() != 1 {
                anyhow::bail!("Room topic could not be saved");
            }
        }

        room.settings
            .get_or_insert_with(|| Self::default_room_settings(room_id))
            .topic = Some(topic.clone());
        room.broadcast_all(&ServerMessage::TopicChanged {
            topic: topic.clone(),
            changed_by: admin_id.to_string(),
        });
        info!("Topic set for room {} by {}", room_id, admin_id);
        Ok(())
    }

    // === Moderation methods ===

    async fn update_punitive_state(
        &self,
        room_id: &str,
        moderator_id: &str,
        expected_sender: &mpsc::Sender<Arc<String>>,
        target_participant_id: &str,
        kind: moderation::PunitiveKind,
        enabled: bool,
        reason: Option<&str>,
    ) -> Result<bool> {
        if reason.is_some_and(|value| value.len() > 256 || value.chars().any(char::is_control)) {
            anyhow::bail!(
                "Moderation reason must be at most 256 characters without control characters"
            );
        }

        let room_lock = self.get_room(room_id)?;
        let mut room = room_lock.write().await;
        let (moderator_role, moderator_authenticated, target_authenticated, target_ip) = {
            let moderator = Self::participant_for_sender(&room, moderator_id, expected_sender)?;
            let target = room
                .participants
                .get(target_participant_id)
                .ok_or_else(|| anyhow::anyhow!("Target participant not found"))?;
            (
                moderator.role,
                moderator.authenticated,
                target.authenticated,
                target.ip,
            )
        };
        let target_participant_ids =
            moderation_cohort(&room, target_participant_id, moderator_role)?;
        let target_lobby_ids =
            guest_ip_lobby_cohort(&room, target_authenticated, target_ip, moderator_role)?;
        let sanction_key =
            SanctionKey::for_identity(target_participant_id, target_authenticated, target_ip);
        if punitive_cohort_matches(
            &room,
            &target_participant_ids,
            &target_lobby_ids,
            &sanction_key,
            kind,
            enabled,
        ) {
            return Ok(false);
        }
        if !room.reserve_admin_mutation(std::time::Instant::now()) {
            anyhow::bail!("Room administration changes are rate limited");
        }

        if room.persisted {
            let pool = self
                .db_pool
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("Moderation service is temporarily unavailable"))?;
            let applied_by = moderator_authenticated
                .then(|| moderator_id.parse::<uuid::Uuid>().ok())
                .flatten()
                .ok_or_else(|| anyhow::anyhow!("A registered moderator is required"))?;
            let target_user = if target_authenticated {
                Some(
                    target_participant_id
                        .parse::<uuid::Uuid>()
                        .map_err(|_| anyhow::anyhow!("Invalid registered participant identity"))?,
                )
            } else {
                None
            };
            if target_user.is_none() && target_ip.is_none() {
                anyhow::bail!("The target cannot be identified for a durable sanction");
            }
            moderation::set_punitive_state(
                pool,
                room_id,
                target_user,
                target_ip,
                kind,
                enabled,
                reason,
                applied_by,
            )
            .await
            .map_err(|error| {
                warn!(room_id, %error, "Failed to persist participant sanction");
                anyhow::anyhow!("Moderation state could not be saved")
            })?;
        }

        apply_punitive_to_cohort(
            &mut room,
            &target_participant_ids,
            &target_lobby_ids,
            sanction_key,
            kind,
            enabled,
        );
        room.policy_revision = room.policy_revision.wrapping_add(1);

        for affected_participant_id in &target_participant_ids {
            let notification =
                punitive_notification(kind, enabled, affected_participant_id.clone());
            room.broadcast_all(&notification);
        }
        for affected_participant_id in &target_lobby_ids {
            let notification =
                punitive_notification(kind, enabled, affected_participant_id.clone());
            if let (Some(entry), Ok(json)) = (
                room.lobby.get(affected_participant_id),
                serde_json::to_string(&notification),
            ) {
                let _ = entry.sender.try_send(Arc::new(json));
            }
        }

        let revoked = if kind == moderation::PunitiveKind::CamBanned && enabled {
            let mut revoked = Vec::new();
            for affected_participant_id in &target_participant_ids {
                revoked.extend(Self::revoke_unauthorized_producers(
                    &mut room,
                    Some(affected_participant_id),
                    "Camera access revoked by moderator",
                ));
            }
            revoked
        } else {
            Vec::new()
        };
        let active_observer = room.active_speaker_observer.clone();
        let audio_observer = room.audio_level_observer.clone();
        drop(room);

        self.close_revoked_producers(room_id, revoked, active_observer, audio_observer)
            .await;
        Ok(true)
    }

    /// Force-close all video/screen producers for a target participant
    pub async fn close_cam(
        &self,
        room_id: &str,
        moderator_id: &str,
        expected_sender: &mpsc::Sender<Arc<String>>,
        target_participant_id: &str,
    ) -> Result<()> {
        // Collect producer IDs to close (under room lock)
        let (producers_to_close, target_media_session_id): (Vec<String>, uuid::Uuid) = {
            let room_lock = self.get_room(room_id)?;
            let mut room = room_lock.write().await;

            let moderator = Self::participant_for_sender(&room, moderator_id, expected_sender)?;
            let target = room
                .participants
                .get(target_participant_id)
                .ok_or_else(|| anyhow::anyhow!("Target participant not found"))?;

            if !moderator.role.can_moderate(target.role) {
                anyhow::bail!("Insufficient permissions to moderate this participant");
            }

            // Collect video producers (camera and screen). An authorized retry
            // against an already-closed camera is a successful no-op.
            let producers: Vec<String> = target
                .producers
                .iter()
                .filter(|(_, (kind, _))| *kind == MediaKind::Video)
                .map(|(id, _)| id.clone())
                .collect();
            let target_media_session_id = target.media_session_id;
            if producers.is_empty() {
                return Ok(());
            }
            if !room.reserve_admin_mutation(std::time::Instant::now()) {
                anyhow::bail!("Room administration changes are rate limited");
            }
            (producers, target_media_session_id)
        };

        // Close producers outside room lock (async mediasoup IPC)
        let target_media_id =
            Self::media_participant_id(room_id, target_participant_id, target_media_session_id);
        for producer_id in &producers_to_close {
            if let Err(e) = self
                .media_server
                .transport_manager()
                .close_producer(&target_media_id, producer_id)
                .await
            {
                warn!(
                    "Failed to close producer {} for close_cam: {}",
                    producer_id, e
                );
            }
        }

        // Remove from room tracking and broadcast
        {
            let room_lock = self.get_room(room_id)?;
            let mut room = room_lock.write().await;
            room.ensure_live()?;
            if let Some(target) = room
                .participants
                .get_mut(target_participant_id)
                .filter(|target| target.media_session_id == target_media_session_id)
            {
                for pid in &producers_to_close {
                    target.producers.remove(pid);
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
        }

        info!(
            "close_cam: {} closed {} video producers for {} in room {}",
            moderator_id,
            producers_to_close.len(),
            target_participant_id,
            room_id
        );
        Ok(())
    }

    /// Ban a participant from producing video/screen (close existing + prevent future)
    pub async fn cam_ban(
        &self,
        room_id: &str,
        moderator_id: &str,
        expected_sender: &mpsc::Sender<Arc<String>>,
        target_participant_id: &str,
        reason: Option<&str>,
    ) -> Result<()> {
        let changed = self
            .update_punitive_state(
                room_id,
                moderator_id,
                expected_sender,
                target_participant_id,
                moderation::PunitiveKind::CamBanned,
                true,
                reason,
            )
            .await?;

        if changed {
            info!(
                "cam_ban: {} cam-banned {} in room {}",
                moderator_id, target_participant_id, room_id
            );
        }
        Ok(())
    }

    /// Remove cam ban from a participant
    pub async fn cam_unban(
        &self,
        room_id: &str,
        moderator_id: &str,
        expected_sender: &mpsc::Sender<Arc<String>>,
        target_participant_id: &str,
    ) -> Result<()> {
        let changed = self
            .update_punitive_state(
                room_id,
                moderator_id,
                expected_sender,
                target_participant_id,
                moderation::PunitiveKind::CamBanned,
                false,
                None,
            )
            .await?;

        if changed {
            info!(
                "cam_unban: {} cam-unbanned {} in room {}",
                moderator_id, target_participant_id, room_id
            );
        }
        Ok(())
    }

    /// Mute a participant's text chat
    pub async fn text_mute(
        &self,
        room_id: &str,
        moderator_id: &str,
        expected_sender: &mpsc::Sender<Arc<String>>,
        target_participant_id: &str,
    ) -> Result<()> {
        let changed = self
            .update_punitive_state(
                room_id,
                moderator_id,
                expected_sender,
                target_participant_id,
                moderation::PunitiveKind::Muted,
                true,
                None,
            )
            .await?;

        if changed {
            info!(
                "text_mute: {} text-muted {} in room {}",
                moderator_id, target_participant_id, room_id
            );
        }
        Ok(())
    }

    /// Unmute a participant's text chat
    pub async fn text_unmute(
        &self,
        room_id: &str,
        moderator_id: &str,
        expected_sender: &mpsc::Sender<Arc<String>>,
        target_participant_id: &str,
    ) -> Result<()> {
        let changed = self
            .update_punitive_state(
                room_id,
                moderator_id,
                expected_sender,
                target_participant_id,
                moderation::PunitiveKind::Muted,
                false,
                None,
            )
            .await?;

        if changed {
            info!(
                "text_unmute: {} text-unmuted {} in room {}",
                moderator_id, target_participant_id, room_id
            );
        }
        Ok(())
    }

    /// Kick a participant from the room
    pub async fn kick_participant(
        &self,
        room_id: &str,
        moderator_id: &str,
        expected_sender: &mpsc::Sender<Arc<String>>,
        target_participant_id: &str,
        reason: Option<&str>,
    ) -> Result<()> {
        // Check permissions and remove under room lock
        let target_media_session_id = {
            let room_lock = self.get_room(room_id)?;
            let mut room = room_lock.write().await;

            let moderator = Self::participant_for_sender(&room, moderator_id, expected_sender)?;
            let target = room
                .participants
                .get(target_participant_id)
                .ok_or_else(|| anyhow::anyhow!("Target participant not found"))?;

            if !moderator.role.can_moderate(target.role) {
                anyhow::bail!("Insufficient permissions to kick this participant");
            }
            if !room.reserve_admin_mutation(std::time::Instant::now()) {
                anyhow::bail!("Room administration changes are rate limited");
            }

            // Broadcast kick to everyone (including the target) before removing
            room.broadcast_all(&ServerMessage::ParticipantKicked {
                participant_id: target_participant_id.to_string(),
                reason: reason.map(String::from),
            });

            let target = room
                .participants
                .remove(target_participant_id)
                .expect("target was checked under the same room lock");
            let target_media_session_id = target.media_session_id;
            for producer_id in target.producers.keys() {
                room.producer_to_participant.remove(producer_id);
            }

            // Broadcast leave to remaining participants
            room.broadcast_all(&ServerMessage::ParticipantLeft {
                participant_id: target_participant_id.to_string(),
            });
            target_media_session_id
        };

        // Clean up media outside lock
        let target_media_id =
            Self::media_participant_id(room_id, target_participant_id, target_media_session_id);
        if let Err(e) = self
            .media_server
            .transport_manager()
            .remove_participant(&target_media_id)
            .await
        {
            warn!(
                "Failed to clean up media for kicked participant {}: {}",
                target_participant_id, e
            );
        }

        info!(
            "kick: {} kicked {} from room {}",
            moderator_id, target_participant_id, room_id
        );
        Ok(())
    }

    /// Ban a participant from the room (kick + durable state for persisted rooms)
    pub async fn ban_participant(
        &self,
        room_id: &str,
        moderator_id: &str,
        expected_sender: &mpsc::Sender<Arc<String>>,
        target_participant_id: &str,
        reason: Option<&str>,
        duration: Option<u64>,
    ) -> Result<()> {
        const MAX_BAN_DURATION_SECS: u64 = 10 * 365 * 24 * 60 * 60;
        if duration.is_some_and(|seconds| seconds == 0 || seconds > MAX_BAN_DURATION_SECS) {
            anyhow::bail!("Ban duration must be between 1 second and 10 years");
        }
        if reason.is_some_and(|value| value.len() > 256 || value.chars().any(char::is_control)) {
            anyhow::bail!("Ban reason must be at most 256 characters without control characters");
        }
        let runtime_expiry = duration.and_then(|seconds| {
            std::time::Instant::now().checked_add(std::time::Duration::from_secs(seconds))
        });

        // Check permissions, add to ban list, and remove — all under one write lock.
        // An unauthenticated target with an IP represents the entire live guest
        // cohort on that IP, matching the durable/in-memory ban key.
        let target_sessions = {
            let room_lock = self.get_room(room_id)?;
            let mut room = room_lock.write().await;

            let moderator = Self::participant_for_sender(&room, moderator_id, expected_sender)?;
            let target = room
                .participants
                .get(target_participant_id)
                .ok_or_else(|| anyhow::anyhow!("Target participant not found"))?;

            let target_participant_ids =
                moderation_cohort(&room, target_participant_id, moderator.role)?;
            let moderator_authenticated = moderator.authenticated;
            let target_authenticated = target.authenticated;
            let target_ip = target.ip;
            let target_lobby_ids =
                guest_ip_lobby_cohort(&room, target_authenticated, target_ip, moderator.role)?;
            if !room.reserve_admin_mutation(std::time::Instant::now()) {
                anyhow::bail!("Room administration changes are rate limited");
            }

            // Persist first so a successful response never represents only a
            // transient ban that disappears when the runtime room is removed.
            if room.persisted {
                let pool = self
                    .db_pool
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("Ban service is temporarily unavailable"))?;
                let applied_by = moderator_authenticated
                    .then(|| moderator_id.parse::<uuid::Uuid>().ok())
                    .flatten()
                    .ok_or_else(|| anyhow::anyhow!("A registered moderator is required"))?;
                let target_user = target_authenticated
                    .then(|| target_participant_id.parse::<uuid::Uuid>().ok())
                    .flatten();
                if target_user.is_none() && target_ip.is_none() {
                    anyhow::bail!("The target cannot be identified for a durable ban");
                }
                let expires_at = duration.and_then(|seconds| {
                    chrono::Utc::now().checked_add_signed(chrono::Duration::seconds(seconds as i64))
                });
                moderation::persist_ban(
                    pool,
                    room_id,
                    target_user,
                    target_ip,
                    reason,
                    expires_at,
                    applied_by,
                )
                .await
                .map_err(|error| {
                    warn!(room_id, %error, "Failed to persist room ban");
                    anyhow::anyhow!("Ban could not be saved")
                })?;
            }

            // Add to ban list — prevents rejoining
            let banned_ids: Vec<String> = target_participant_ids
                .iter()
                .chain(target_lobby_ids.iter())
                .cloned()
                .collect();
            record_banned_cohort(
                &mut room,
                &banned_ids,
                target_authenticated,
                target_ip,
                runtime_expiry,
            );
            room.policy_revision = room.policy_revision.wrapping_add(1);

            let mut target_sessions = Vec::with_capacity(target_participant_ids.len());
            for affected_participant_id in &target_participant_ids {
                // Notify every socket in the affected IP cohort before removing it.
                room.broadcast_all(&ServerMessage::ParticipantBanned {
                    participant_id: affected_participant_id.clone(),
                    reason: reason.map(String::from),
                });

                let target = room
                    .participants
                    .remove(affected_participant_id)
                    .expect("moderation cohort was resolved under the same room lock");
                for producer_id in target.producers.keys() {
                    room.producer_to_participant.remove(producer_id);
                }
                target_sessions.push((affected_participant_id.clone(), target.media_session_id));

                room.broadcast_all(&ServerMessage::ParticipantLeft {
                    participant_id: affected_participant_id.clone(),
                });
            }
            for affected_participant_id in target_lobby_ids {
                if let Some(entry) = room.lobby.remove(&affected_participant_id)
                    && let Ok(json) = serde_json::to_string(&ServerMessage::LobbyDenied {
                        reason: Some(
                            reason.map_or_else(|| "Banned from room".to_string(), String::from),
                        ),
                    })
                {
                    let _ = entry.sender.try_send(Arc::new(json));
                }
            }
            target_sessions
        };

        // Clean up media outside lock
        for (affected_participant_id, media_session_id) in target_sessions {
            let target_media_id =
                Self::media_participant_id(room_id, &affected_participant_id, media_session_id);
            if let Err(e) = self
                .media_server
                .transport_manager()
                .remove_participant(&target_media_id)
                .await
            {
                warn!(
                    "Failed to clean up media for banned participant {}: {}",
                    affected_participant_id, e
                );
            }
        }

        info!(
            "ban: {} banned {} from room {}",
            moderator_id, target_participant_id, room_id
        );
        Ok(())
    }

    /// Remove a participant from the room's ban list
    pub async fn unban_participant(
        &self,
        room_id: &str,
        moderator_id: &str,
        expected_sender: &mpsc::Sender<Arc<String>>,
        target_participant_id: &str,
    ) -> Result<()> {
        let room_lock = self.get_room(room_id)?;
        let mut room = room_lock.write().await;
        room.ensure_live()?;

        let moderator = Self::participant_for_sender(&room, moderator_id, expected_sender)?;

        if moderator.role < roles::Role::Admin {
            anyhow::bail!("Insufficient permissions to unban (requires Admin+)");
        }

        room.prune_expired_bans(std::time::Instant::now());
        if room
            .banned_guest_participants
            .contains(target_participant_id)
        {
            anyhow::bail!(
                "Guest bans are identity/IP-scoped and cannot be removed by participant ID"
            );
        }

        let in_memory_exists = room.banned_participants.contains_key(target_participant_id);
        let persisted_target = room
            .persisted
            .then(|| target_participant_id.parse::<uuid::Uuid>().ok())
            .flatten();
        if !in_memory_exists && persisted_target.is_none() {
            anyhow::bail!("Participant is not banned");
        }
        if !room.reserve_admin_mutation(std::time::Instant::now()) {
            anyhow::bail!("Room administration changes are rate limited");
        }

        // Also clear any persisted user ban (target id == user uuid for
        // registered users; guests have no stable identity to unban). Keep the
        // room generation locked through this write so deletion/recreation
        // cannot redirect a delayed unban into a replacement room.
        let mut persisted_removed = false;
        if let Some(uid) = persisted_target {
            let pool = self
                .db_pool
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("Ban service is temporarily unavailable"))?;
            persisted_removed = moderation::remove_user_ban(pool, room_id, uid)
                .await
                .map_err(|error| {
                    warn!(room_id, %error, "Failed to remove persisted ban");
                    anyhow::anyhow!("Ban state could not be updated")
                })?;
        }

        let in_memory = room
            .banned_participants
            .remove(target_participant_id)
            .is_some();

        if !in_memory && !persisted_removed {
            anyhow::bail!("Participant is not banned");
        }

        info!(
            "unban: {} unbanned {} from room {}",
            moderator_id, target_participant_id, room_id
        );
        Ok(())
    }

    /// Set a participant's role
    pub async fn set_participant_role(
        &self,
        room_id: &str,
        setter_id: &str,
        expected_sender: &mpsc::Sender<Arc<String>>,
        target_participant_id: &str,
        new_role: roles::Role,
    ) -> Result<()> {
        let room_lock = self.get_room(room_id)?;
        let mut room = room_lock.write().await;

        let setter = Self::participant_for_sender(&room, setter_id, expected_sender)?;
        let target = room
            .participants
            .get(target_participant_id)
            .ok_or_else(|| anyhow::anyhow!("Target participant not found"))?;

        if !setter.role.can_set_role(target.role, new_role) {
            anyhow::bail!("Insufficient permissions to set this role");
        }
        if target.authenticated && new_role == roles::Role::Guest {
            anyhow::bail!("Registered participants must be assigned at least the user role");
        }
        if target.role == new_role {
            return Ok(());
        }

        let setter_name = setter.id.clone();
        let setter_authenticated = setter.authenticated;
        let target_authenticated = target.authenticated;
        if !room.reserve_admin_mutation(std::time::Instant::now()) {
            anyhow::bail!("Room administration changes are rate limited");
        }

        if room.persisted {
            if !setter_authenticated || !target_authenticated {
                anyhow::bail!("Persistent roles require registered participants");
            }
            let granted_by = setter_id
                .parse::<uuid::Uuid>()
                .map_err(|_| anyhow::anyhow!("Invalid registered participant identity"))?;
            let target_user = target_participant_id
                .parse::<uuid::Uuid>()
                .map_err(|_| anyhow::anyhow!("Invalid registered participant identity"))?;
            let pool = self
                .db_pool
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("Role service is temporarily unavailable"))?;
            roles::set_role(pool, room_id, &target_user, new_role, &granted_by)
                .await
                .map_err(|error| {
                    warn!(room_id, %error, "Failed to persist participant role");
                    anyhow::anyhow!("Role could not be saved")
                })?;
        }

        let target = room.participants.get_mut(target_participant_id).unwrap();
        target.role = new_role;
        room.policy_revision = room.policy_revision.wrapping_add(1);
        let revoked = Self::revoke_unauthorized_producers(
            &mut room,
            Some(target_participant_id),
            "Role no longer permits this producer",
        );
        let active_observer = room.active_speaker_observer.clone();
        let audio_observer = room.audio_level_observer.clone();

        room.broadcast_all(&ServerMessage::RoleChanged {
            participant_id: target_participant_id.to_string(),
            new_role: new_role.name().to_string(),
            granted_by: setter_name,
        });
        drop(room);

        self.close_revoked_producers(room_id, revoked, active_observer, audio_observer)
            .await;

        info!(
            "set_role: {} set {} to {:?} in room {}",
            setter_id, target_participant_id, new_role, room_id
        );
        Ok(())
    }

    /// Request voice in a moderated room (broadcasts to Moderator+ participants)
    pub async fn request_voice(
        &self,
        room_id: &str,
        participant_id: &str,
        expected_sender: &mpsc::Sender<Arc<String>>,
    ) -> Result<()> {
        let room_lock = self.get_room(room_id)?;
        let mut room = room_lock.write().await;

        let participant = Self::participant_for_sender(&room, participant_id, expected_sender)?;

        let display_name = participant.name.clone();

        if !room.reserve_voice_request(std::time::Instant::now()) {
            anyhow::bail!("Room voice-request rate limit exceeded");
        }

        room.broadcast_to_role(
            roles::Role::Moderator,
            &ServerMessage::VoiceRequested {
                participant_id: participant_id.to_string(),
                display_name,
            },
        );

        info!(
            "request_voice: {} requested voice in room {}",
            participant_id, room_id
        );
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
        expected_sender: &mpsc::Sender<Arc<String>>,
        target_id: &str,
    ) -> Result<()> {
        let room_lock = self.get_room(room_id)?;
        let mut room = room_lock.write().await;

        // Verify moderator exists and has permission
        let moderator = Self::participant_for_sender(&room, moderator_id, expected_sender)?;
        if !moderator.role.can_admit_lobby() {
            anyhow::bail!("Insufficient permissions to admit from lobby");
        }
        if !room.lobby.contains_key(target_id) {
            anyhow::bail!("Participant not found in lobby");
        }

        if let Some(maximum) = room
            .settings
            .as_ref()
            .and_then(|settings| settings.max_participants)
        {
            let maximum = usize::try_from(maximum)
                .map_err(|_| anyhow::anyhow!("Room capacity is unavailable"))?;
            if maximum == 0 || room.participants.len() >= maximum {
                anyhow::bail!("Room is full");
            }
        }

        room.prune_expired_bans(std::time::Instant::now());
        if room
            .lobby
            .get(target_id)
            .is_some_and(|entry| room.identity_is_banned(target_id, entry.authenticated, entry.ip))
        {
            anyhow::bail!("Participant is banned from this room");
        }

        if room
            .lobby
            .get(target_id)
            .is_some_and(|entry| entry.sender.is_closed())
        {
            anyhow::bail!("Participant has already disconnected");
        }
        if !room.reserve_admin_mutation(std::time::Instant::now()) {
            anyhow::bail!("Room administration changes are rate limited");
        }

        // Remove from lobby
        let entry = room
            .lobby
            .remove(target_id)
            .ok_or_else(|| anyhow::anyhow!("Participant not found in lobby"))?;

        // Build the participants list (before inserting the new one)
        let participants: Vec<ParticipantInfo> = room
            .participants
            .values()
            .map(|p| ParticipantInfo {
                id: p.id.clone(),
                name: p.name.clone(),
                producers: p
                    .producers
                    .iter()
                    .map(|(id, (kind, source))| ProducerMetadata {
                        id: id.clone(),
                        kind: *kind,
                        source: source.clone(),
                    })
                    .collect(),
                role: p.role.name().to_string(),
            })
            .collect();

        let (admitted_role, admitted_punitive) = lobby_admission_state(&room, &entry);

        // Insert into participants
        let participant = Participant {
            id: entry.participant_id.clone(),
            name: entry.name.clone(),
            sender: entry.sender.clone(),
            media_session_id: entry.media_session_id,
            producers: HashMap::new(),
            role: admitted_role,
            punitive: admitted_punitive,
            authenticated: entry.authenticated,
            ip: entry.ip,
        };
        room.participants
            .insert(entry.participant_id.clone(), participant);

        // Clear the connection task's lobby guard BEFORE notifying the client,
        // so its follow-up media-setup messages are not rejected.
        entry.in_lobby_flag.store(false, Ordering::Release);

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
        let room_settings = room
            .settings
            .as_ref()
            .and_then(|s| serde_json::to_value(s).ok());
        if let Ok(json) = serde_json::to_string(&ServerMessage::RoomJoined {
            participant_id: entry.participant_id.clone(),
            participants,
            reconnect_token,
            your_role: admitted_role.name().to_string(),
            room_settings,
        }) {
            let _ = entry.sender.try_send(Arc::new(json));
        }

        // Broadcast ParticipantJoined to existing participants (excluding the admitted one)
        room.broadcast_except(
            &entry.participant_id,
            &ServerMessage::ParticipantJoined {
                participant_id: entry.participant_id.clone(),
                participant_name: entry.name.clone(),
                role: admitted_role.name().to_string(),
                authenticated: entry.authenticated,
            },
        );

        info!(
            "admit_from_lobby: {} admitted {} to room {}",
            moderator_id, target_id, room_id
        );
        Ok(())
    }

    /// Deny a participant from the lobby.
    ///
    /// Removes the target from `room.lobby` and sends `LobbyDenied` to them.
    pub async fn deny_from_lobby(
        &self,
        room_id: &str,
        moderator_id: &str,
        expected_sender: &mpsc::Sender<Arc<String>>,
        target_id: &str,
        reason: Option<String>,
    ) -> Result<()> {
        let room_lock = self.get_room(room_id)?;
        let mut room = room_lock.write().await;

        // Verify moderator exists and has permission
        let moderator = Self::participant_for_sender(&room, moderator_id, expected_sender)?;
        if !moderator.role.can_admit_lobby() {
            anyhow::bail!("Insufficient permissions to deny from lobby");
        }
        if !room.lobby.contains_key(target_id) {
            anyhow::bail!("Participant not found in lobby");
        }
        if !room.reserve_admin_mutation(std::time::Instant::now()) {
            anyhow::bail!("Room administration changes are rate limited");
        }

        // Remove from lobby
        let entry = room
            .lobby
            .remove(target_id)
            .ok_or_else(|| anyhow::anyhow!("Participant not found in lobby"))?;

        // Send LobbyDenied to the denied participant
        if let Ok(json) = serde_json::to_string(&ServerMessage::LobbyDenied { reason }) {
            let _ = entry.sender.try_send(Arc::new(json));
        }

        info!(
            "deny_from_lobby: {} denied {} from room {}",
            moderator_id, target_id, room_id
        );
        Ok(())
    }

    fn participant_for_sender<'a>(
        room: &'a Room,
        participant_id: &str,
        expected_sender: &mpsc::Sender<Arc<String>>,
    ) -> Result<&'a Participant> {
        room.ensure_live()?;
        let participant = room
            .participants
            .get(participant_id)
            .ok_or_else(|| anyhow::anyhow!("Participant is not an active member of this room"))?;
        if !participant.sender.same_channel(expected_sender) {
            anyhow::bail!("This connection no longer owns the participant session");
        }
        Ok(participant)
    }

    async fn reserve_media_mutation_for_sender(
        &self,
        room_id: &str,
        participant_id: &str,
        expected_sender: &mpsc::Sender<Arc<String>>,
        producer_id: Option<&str>,
    ) -> Result<MediaMutationReservation> {
        let room_lock = self.get_room(room_id)?;
        let mut room = room_lock.write().await;
        let participant = Self::participant_for_sender(&room, participant_id, expected_sender)?;
        if producer_id.is_some_and(|producer_id| !participant.producers.contains_key(producer_id)) {
            anyhow::bail!("Producer does not belong to this participant");
        }
        let reserved_at = std::time::Instant::now();
        if !room.reserve_media_mutation(reserved_at) {
            anyhow::bail!("Room media changes are rate limited");
        }
        drop(room);
        Ok(MediaMutationReservation {
            room: room_lock,
            reserved_at,
        })
    }

    async fn reserve_producer_pause_mutation_for_sender(
        &self,
        room_id: &str,
        participant_id: &str,
        expected_sender: &mpsc::Sender<Arc<String>>,
        producer_id: &str,
        desired_paused: bool,
    ) -> Result<Option<MediaMutationReservation>> {
        let room_lock = self.get_room(room_id)?;
        let mut room = room_lock.write().await;
        let participant = Self::participant_for_sender(&room, participant_id, expected_sender)?;
        if !participant.producers.contains_key(producer_id) {
            anyhow::bail!("Producer does not belong to this participant");
        }
        if self
            .media_server
            .transport_manager()
            .find_producer_paused(producer_id)
            == Some(desired_paused)
        {
            return Ok(None);
        }
        let reserved_at = std::time::Instant::now();
        if !room.reserve_media_mutation(reserved_at) {
            anyhow::bail!("Room media changes are rate limited");
        }
        drop(room);
        Ok(Some(MediaMutationReservation {
            room: room_lock,
            reserved_at,
        }))
    }

    async fn refund_media_mutation(reservation: MediaMutationReservation) {
        let mut room = reservation.room.write().await;
        room.refund_media_mutation(reservation.reserved_at);
    }

    async fn reserve_media_control_ipc_for_sender(
        &self,
        room_id: &str,
        participant_id: &str,
        expected_sender: &mpsc::Sender<Arc<String>>,
    ) -> Result<MediaControlIpcReservation> {
        let room_lock = self.get_room(room_id)?;
        let mut room = room_lock.write().await;
        Self::participant_for_sender(&room, participant_id, expected_sender)?;
        let reserved_at = std::time::Instant::now();
        if !room.reserve_media_control_ipc(reserved_at) {
            anyhow::bail!("Room media control operations are rate limited");
        }
        drop(room);
        Ok(MediaControlIpcReservation {
            room: room_lock,
            reserved_at,
        })
    }

    async fn refund_media_control_ipc(reservation: MediaControlIpcReservation) {
        let mut room = reservation.room.write().await;
        room.refund_media_control_ipc(reservation.reserved_at);
    }

    fn participant_session_is_current(
        room: &Room,
        participant_id: &str,
        expected_sender: &mpsc::Sender<Arc<String>>,
        media_session_id: uuid::Uuid,
    ) -> bool {
        !room.deleting
            && room
                .participants
                .get(participant_id)
                .is_some_and(|participant| {
                    participant.media_session_id == media_session_id
                        && participant.sender.same_channel(expected_sender)
                })
    }

    pub async fn is_bound_participant(
        &self,
        room_id: &str,
        participant_id: &str,
        expected_sender: &mpsc::Sender<Arc<String>>,
    ) -> bool {
        let Ok(room_lock) = self.get_room(room_id) else {
            return false;
        };
        let room = room_lock.read().await;
        !room.deleting
            && (room
                .participants
                .get(participant_id)
                .is_some_and(|participant| participant.sender.same_channel(expected_sender))
                || room
                    .lobby
                    .get(participant_id)
                    .is_some_and(|entry| entry.sender.same_channel(expected_sender)))
    }

    async fn media_session_for_sender(
        &self,
        room_id: &str,
        participant_id: &str,
        expected_sender: &mpsc::Sender<Arc<String>>,
    ) -> Result<uuid::Uuid> {
        let room_lock = self.get_room(room_id)?;
        let room = room_lock.read().await;
        Ok(Self::participant_for_sender(&room, participant_id, expected_sender)?.media_session_id)
    }

    async fn is_current_media_session(
        &self,
        room_id: &str,
        participant_id: &str,
        expected_sender: &mpsc::Sender<Arc<String>>,
        media_session_id: uuid::Uuid,
    ) -> bool {
        let Ok(room_lock) = self.get_room(room_id) else {
            return false;
        };
        let room = room_lock.read().await;
        Self::participant_session_is_current(
            &room,
            participant_id,
            expected_sender,
            media_session_id,
        )
    }

    fn valid_media_source(kind: MediaKind, source: &str) -> bool {
        matches!(
            (kind, source),
            (MediaKind::Audio, "microphone")
                | (MediaKind::Audio, "screen-audio")
                | (MediaKind::Video, "camera")
                | (MediaKind::Video, "screen")
        )
    }

    fn producer_allowed_without_capacity(
        room: &Room,
        participant: &Participant,
        kind: MediaKind,
        source: &str,
    ) -> bool {
        if !Self::valid_media_source(kind, source) {
            return false;
        }

        if let Some(settings) = &room.settings {
            if participant.role == roles::Role::Guest && !settings.guests_can_broadcast {
                return false;
            }
            if !settings.allow_screen_sharing && matches!(source, "screen" | "screen-audio") {
                return false;
            }
            if !settings.allow_video && kind == MediaKind::Video && source == "camera" {
                return false;
            }
        }

        let moderated = room.settings.as_ref().map_or(false, |s| s.moderated);
        moderation::can_produce(&participant.punitive, participant.role, moderated, kind)
    }

    fn participant_can_produce(
        room: &Room,
        participant: &Participant,
        kind: MediaKind,
        source: &str,
    ) -> bool {
        if !Self::producer_allowed_without_capacity(room, participant, kind, source) {
            return false;
        }

        if let Some(max) = room
            .settings
            .as_ref()
            .and_then(|settings| settings.max_broadcasters)
        {
            let Ok(max) = usize::try_from(max) else {
                return false;
            };
            let broadcaster_count = room
                .participants
                .values()
                .filter(|participant| !participant.producers.is_empty())
                .count();
            if participant.producers.is_empty() && broadcaster_count >= max {
                return false;
            }
        }

        true
    }

    /// Remove producer bookkeeping for streams that no longer satisfy current
    /// room policy. Physical mediasoup closure is deliberately performed by
    /// `close_revoked_producers` after the room lock is released.
    fn revoke_unauthorized_producers(
        room: &mut Room,
        participant_filter: Option<&str>,
        reason: &str,
    ) -> Vec<RevokedProducer> {
        let max_broadcasters = room
            .settings
            .as_ref()
            .and_then(|settings| settings.max_broadcasters)
            .and_then(|maximum| usize::try_from(maximum).ok());

        // Determine which otherwise-authorized broadcasters survive a newly
        // reduced cap. Prefer more privileged roles, then stable participant ID,
        // so enforcement is deterministic and does not depend on HashMap order.
        let mut broadcaster_candidates: Vec<(String, roles::Role)> = room
            .participants
            .values()
            .filter(|participant| {
                participant.producers.values().any(|(kind, source)| {
                    source.as_deref().is_some_and(|source| {
                        Self::producer_allowed_without_capacity(room, participant, *kind, source)
                    })
                })
            })
            .map(|participant| (participant.id.clone(), participant.role))
            .collect();
        broadcaster_candidates
            .sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
        let over_capacity: HashSet<String> = max_broadcasters
            .map(|maximum| {
                broadcaster_candidates
                    .into_iter()
                    .skip(maximum)
                    .map(|(participant_id, _)| participant_id)
                    .collect()
            })
            .unwrap_or_default();

        let revoked: Vec<RevokedProducer> = room
            .participants
            .values()
            .filter(|participant| participant_filter.is_none_or(|filter| participant.id == filter))
            .flat_map(|participant| {
                participant
                    .producers
                    .iter()
                    .filter_map(|(producer_id, (kind, source))| {
                        let allowed = source.as_deref().is_some_and(|source| {
                            Self::producer_allowed_without_capacity(
                                room,
                                participant,
                                *kind,
                                source,
                            )
                        });
                        (!allowed || over_capacity.contains(&participant.id)).then(|| {
                            RevokedProducer {
                                participant_id: participant.id.clone(),
                                media_session_id: participant.media_session_id,
                                producer_id: producer_id.clone(),
                                kind: *kind,
                            }
                        })
                    })
            })
            .collect();

        for producer in &revoked {
            if let Some(participant) = room.participants.get_mut(&producer.participant_id) {
                participant.producers.remove(&producer.producer_id);
            }
            room.producer_to_participant.remove(&producer.producer_id);
            room.broadcast_all(&ServerMessage::ForceClosedProducer {
                producer_id: producer.producer_id.clone(),
                reason: reason.to_string(),
            });
            room.broadcast_all(&ServerMessage::ProducerClosed {
                producer_id: producer.producer_id.clone(),
            });
        }

        revoked
    }

    async fn close_revoked_producers(
        &self,
        room_id: &str,
        revoked: Vec<RevokedProducer>,
        active_observer: Option<ActiveSpeakerObserver>,
        audio_observer: Option<AudioLevelObserver>,
    ) {
        let mut failed_participants = HashSet::new();
        for producer in revoked {
            let media_participant_id = Self::media_participant_id(
                room_id,
                &producer.participant_id,
                producer.media_session_id,
            );
            if !failed_participants.contains(&producer.participant_id)
                && let Err(error) = self
                    .media_server
                    .transport_manager()
                    .close_producer(&media_participant_id, &producer.producer_id)
                    .await
            {
                // A failed individual close must not leave an unauthorized
                // stream alive. Fall back to dropping all media state for that
                // participant; they may establish fresh authorized transports.
                warn!(
                    room_id,
                    participant_id = %producer.participant_id,
                    producer_id = %producer.producer_id,
                    %error,
                    "Failed to close revoked producer; dropping participant media state"
                );
                failed_participants.insert(producer.participant_id.clone());
                if let Err(cleanup_error) = self
                    .media_server
                    .transport_manager()
                    .remove_participant(&media_participant_id)
                    .await
                {
                    warn!(
                        room_id,
                        participant_id = %producer.participant_id,
                        %cleanup_error,
                        "Failed to drop media state after producer revocation"
                    );
                }
            }

            if producer.kind == MediaKind::Audio
                && let Ok(producer_id) = producer.producer_id.parse::<ProducerId>()
            {
                if let Some(observer) = &active_observer {
                    let _ = observer.remove_producer(producer_id).await;
                }
                if let Some(observer) = &audio_observer {
                    let _ = observer.remove_producer(producer_id).await;
                }
            }
        }
    }

    /// Check if a participant can produce a given source type
    pub async fn can_participant_produce(
        &self,
        room_id: &str,
        participant_id: &str,
        expected_sender: &mpsc::Sender<Arc<String>>,
        kind: MediaKind,
        source: &str,
    ) -> Result<bool> {
        let room_lock = self.get_room(room_id)?;
        let room = room_lock.read().await;
        let participant = Self::participant_for_sender(&room, participant_id, expected_sender)?;

        Ok(Self::participant_can_produce(
            &room,
            participant,
            kind,
            source,
        ))
    }

    /// Check if a participant can chat
    pub async fn can_participant_chat(
        &self,
        room_id: &str,
        participant_id: &str,
        expected_sender: &mpsc::Sender<Arc<String>>,
    ) -> Result<bool> {
        let room_lock = self.get_room(room_id)?;
        let room = room_lock.read().await;
        let participant = Self::participant_for_sender(&room, participant_id, expected_sender)?;
        if room.settings.as_ref().is_some_and(|s| !s.allow_chat) {
            return Ok(false);
        }
        let moderated = room.settings.as_ref().map_or(false, |s| s.moderated);
        Ok(moderation::can_chat(
            &participant.punitive,
            participant.role,
            moderated,
        ))
    }

    /// Broadcasts a chat message to all participants in a room except the sender
    pub async fn broadcast_chat(
        &self,
        room_id: &str,
        sender_id: &str,
        expected_sender: &mpsc::Sender<Arc<String>>,
        content: String,
    ) -> Result<()> {
        let room_lock = self.get_room(room_id)?;
        let mut room = room_lock.write().await;
        let sender = Self::participant_for_sender(&room, sender_id, expected_sender)?;
        let sender_name = sender.name.clone();
        let moderated = room
            .settings
            .as_ref()
            .is_some_and(|settings| settings.moderated);
        if room
            .settings
            .as_ref()
            .is_some_and(|settings| !settings.allow_chat)
            || !moderation::can_chat(&sender.punitive, sender.role, moderated)
        {
            anyhow::bail!("You are not allowed to chat");
        }
        if !room.reserve_chat_broadcast(std::time::Instant::now()) {
            anyhow::bail!("Room chat rate limit exceeded");
        }

        room.broadcast_except(
            sender_id,
            &ServerMessage::ChatReceived {
                participant_id: sender_id.to_string(),
                participant_name: sender_name,
                content,
            },
        );

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
            let participant_sessions: Vec<(String, uuid::Uuid)> = {
                let room = room_lock.read().await;
                room.participants
                    .values()
                    .map(|participant| (participant.id.clone(), participant.media_session_id))
                    .collect()
            };

            for (pid, media_session_id) in &participant_sessions {
                let media_participant_id =
                    Self::media_participant_id(room_id, pid, *media_session_id);
                if let Err(e) = self
                    .media_server
                    .transport_manager()
                    .remove_participant(&media_participant_id)
                    .await
                {
                    warn!(
                        "Failed to clean up media for participant {} during shutdown: {}",
                        pid, e
                    );
                }
            }

            self.media_server.remove_router(room_id).await.ok();
            info!(
                "Shut down room {} ({} participants)",
                room_id,
                participant_sessions.len()
            );
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
        rooms
            .get(room_id)
            .and_then(|lock| lock.try_read().ok())
            .map(|room| room.participants.len())
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod security_tests {
    use super::*;

    #[test]
    fn aggregate_room_chat_budget_is_bounded_and_recovers() {
        let mut room = Room::new("room".to_string(), "router".to_string(), None, false, None);
        let start = std::time::Instant::now();
        for _ in 0..MAX_ROOM_CHAT_MESSAGES_PER_WINDOW {
            assert!(room.reserve_chat_broadcast(start));
        }
        assert!(!room.reserve_chat_broadcast(start));
        assert!(room.reserve_chat_broadcast(start + ROOM_CHAT_WINDOW));
        assert_eq!(room.recent_chat_broadcasts.len(), 1);
    }

    #[test]
    fn ad_hoc_rooms_default_closed_and_require_explicit_opt_in() {
        assert!(
            !parse_environment_flag_value(
                "ALLOW_AD_HOC_ROOMS",
                None,
                ALLOW_AD_HOC_ROOMS_BY_DEFAULT,
            )
            .unwrap()
        );
        assert!(
            parse_environment_flag_value(
                "ALLOW_AD_HOC_ROOMS",
                Some("true"),
                ALLOW_AD_HOC_ROOMS_BY_DEFAULT,
            )
            .unwrap()
        );
    }

    #[test]
    fn aggregate_room_media_budget_is_bounded_and_recovers() {
        let mut room = Room::new("room".to_string(), "router".to_string(), None, false, None);
        let start = std::time::Instant::now();
        for _ in 0..MAX_ROOM_MEDIA_MUTATIONS_PER_WINDOW {
            assert!(room.reserve_media_mutation(start));
        }
        assert!(!room.reserve_media_mutation(start));
        assert!(room.reserve_media_mutation(start + ROOM_MEDIA_MUTATION_WINDOW));
        assert_eq!(room.recent_media_mutations.len(), 1);
    }

    #[test]
    fn no_op_media_mutation_refund_preserves_aggregate_capacity() {
        let mut room = Room::new("room".to_string(), "router".to_string(), None, false, None);
        let start = std::time::Instant::now();
        for _ in 0..(MAX_ROOM_MEDIA_MUTATIONS_PER_WINDOW * 2) {
            assert!(room.reserve_media_mutation(start));
            assert!(room.refund_media_mutation(start));
        }
        assert!(room.recent_media_mutations.is_empty());
        for _ in 0..MAX_ROOM_MEDIA_MUTATIONS_PER_WINDOW {
            assert!(room.reserve_media_mutation(start));
        }
        assert!(!room.reserve_media_mutation(start));
    }

    #[tokio::test]
    async fn media_mutation_refund_targets_original_room_generation() {
        let start = std::time::Instant::now();
        let original = Arc::new(TokioRwLock::new(Room::new(
            "room".to_string(),
            "old-router".to_string(),
            None,
            false,
            None,
        )));
        let replacement = Arc::new(TokioRwLock::new(Room::new(
            "room".to_string(),
            "new-router".to_string(),
            None,
            false,
            None,
        )));
        assert!(original.write().await.reserve_media_mutation(start));
        assert!(replacement.write().await.reserve_media_mutation(start));

        RoomManager::refund_media_mutation(MediaMutationReservation {
            room: original.clone(),
            reserved_at: start,
        })
        .await;

        assert!(original.read().await.recent_media_mutations.is_empty());
        assert_eq!(replacement.read().await.recent_media_mutations.len(), 1);
    }

    #[test]
    fn aggregate_room_media_control_ipc_budget_is_bounded_and_recovers() {
        let mut room = Room::new("room".to_string(), "router".to_string(), None, false, None);
        let start = std::time::Instant::now();
        for _ in 0..MAX_ROOM_MEDIA_CONTROL_IPC_PER_WINDOW {
            assert!(room.reserve_media_control_ipc(start));
        }
        assert!(!room.reserve_media_control_ipc(start));
        assert!(room.reserve_media_control_ipc(start + ROOM_MEDIA_CONTROL_IPC_WINDOW));
        assert_eq!(room.recent_media_control_ipc.len(), 1);
    }

    #[test]
    fn no_op_media_control_ipc_refund_preserves_aggregate_capacity() {
        let mut room = Room::new("room".to_string(), "router".to_string(), None, false, None);
        let start = std::time::Instant::now();
        for _ in 0..(MAX_ROOM_MEDIA_CONTROL_IPC_PER_WINDOW * 2) {
            assert!(room.reserve_media_control_ipc(start));
            assert!(room.refund_media_control_ipc(start));
        }
        assert!(room.recent_media_control_ipc.is_empty());
        for _ in 0..MAX_ROOM_MEDIA_CONTROL_IPC_PER_WINDOW {
            assert!(room.reserve_media_control_ipc(start));
        }
        assert!(!room.reserve_media_control_ipc(start));
    }

    #[test]
    fn aggregate_room_admin_budget_is_bounded_and_recovers() {
        let mut room = Room::new("room".to_string(), "router".to_string(), None, false, None);
        let start = std::time::Instant::now();
        for _ in 0..MAX_ROOM_ADMIN_MUTATIONS_PER_WINDOW {
            assert!(room.reserve_admin_mutation(start));
        }
        assert!(!room.reserve_admin_mutation(start));
        assert!(room.reserve_admin_mutation(start + ROOM_ADMIN_MUTATION_WINDOW));
        assert_eq!(room.recent_admin_mutations.len(), 1);
    }

    #[test]
    fn aggregate_room_voice_request_budget_is_bounded_and_recovers() {
        let mut room = Room::new("room".to_string(), "router".to_string(), None, false, None);
        let start = std::time::Instant::now();
        for _ in 0..MAX_ROOM_VOICE_REQUESTS_PER_WINDOW {
            assert!(room.reserve_voice_request(start));
        }
        assert!(!room.reserve_voice_request(start));
        assert!(room.reserve_voice_request(start + ROOM_VOICE_REQUEST_WINDOW));
        assert_eq!(room.recent_voice_requests.len(), 1);
    }

    #[test]
    fn shared_join_budget_aggregates_sockets_and_ipv6_addresses() {
        let limiter = SharedRateLimiter::new(2);
        let start = std::time::Instant::now();
        let first = rate_limit_ip("2001:db8:1:2::1".parse().unwrap());
        let rotated = rate_limit_ip("2001:db8:1:2:ffff::42".parse().unwrap());
        assert_eq!(first, rotated);
        assert!(limiter.allow(first, start));
        assert!(limiter.allow(rotated, start));
        assert!(!limiter.allow(first, start));
        assert!(limiter.allow(first, start + JOIN_RATE_WINDOW));

        let mapped_first = rate_limit_ip("::ffff:192.0.2.1".parse().unwrap());
        let mapped_second = rate_limit_ip("::ffff:192.0.2.2".parse().unwrap());
        assert_eq!(mapped_first, "192.0.2.1".parse::<IpAddr>().unwrap());
        assert_ne!(mapped_first, mapped_second);
    }

    #[test]
    fn failed_join_cleanup_evicts_only_the_last_unpopulated_reservation() {
        let mut abandoned = Room::new(
            "abandoned".to_string(),
            "router".to_string(),
            None,
            false,
            None,
        );
        abandoned.pending_joins = 1;
        assert!(release_join_reservation(&mut abandoned));
        assert_eq!(abandoned.pending_joins, 0);

        let mut concurrent = Room::new(
            "concurrent".to_string(),
            "router".to_string(),
            None,
            false,
            None,
        );
        concurrent.pending_joins = 2;
        // One failed join must not evict the room while another join is still
        // performing its authorization/password work.
        assert!(!release_join_reservation(&mut concurrent));
        assert_eq!(concurrent.pending_joins, 1);

        concurrent.participants.insert(
            "winner".to_string(),
            participant(
                "winner",
                roles::Role::Guest,
                moderation::PunitiveState::default(),
                Some(IpAddr::from([192, 0, 2, 9])),
            ),
        );
        // Once the concurrent join succeeds, releasing its reservation leaves
        // an occupied room and therefore cannot authorize teardown.
        assert!(!release_join_reservation(&mut concurrent));
        assert_eq!(concurrent.pending_joins, 0);
        assert!(concurrent.participants.contains_key("winner"));
    }

    fn participant(
        id: &str,
        role: roles::Role,
        punitive: moderation::PunitiveState,
        ip: Option<IpAddr>,
    ) -> Participant {
        let (sender, _receiver) = mpsc::channel(4);
        Participant {
            id: id.to_string(),
            name: id.to_string(),
            sender,
            media_session_id: uuid::Uuid::new_v4(),
            producers: HashMap::new(),
            role,
            punitive,
            authenticated: role != roles::Role::Guest,
            ip,
        }
    }

    #[test]
    fn persisted_first_joiner_never_inherits_owner() {
        assert_eq!(
            select_join_role(true, true, Some(roles::Role::Guest), false),
            Some(roles::Role::Guest)
        );
        assert_eq!(select_join_role(true, true, None, true), None);
        assert_eq!(
            select_join_role(false, true, None, false),
            Some(roles::Role::Owner)
        );
    }

    #[test]
    fn guest_sanctions_survive_a_new_connection_identity() {
        let ip: IpAddr = "203.0.113.7".parse().unwrap();
        assert_eq!(
            SanctionKey::for_identity("old-socket", false, Some(ip)),
            SanctionKey::for_identity("new-socket", false, Some(ip))
        );
        assert_ne!(
            SanctionKey::for_identity("old-socket", false, None),
            SanctionKey::for_identity("new-socket", false, None)
        );

        let merged = merge_punitive_state(
            moderation::PunitiveState {
                cam_banned: false,
                text_muted: true,
            },
            Some(&moderation::PunitiveState {
                cam_banned: true,
                text_muted: false,
            }),
        );
        assert!(merged.cam_banned && merged.text_muted);
    }

    #[test]
    fn unchanged_punitive_cohort_is_detected_without_mutation() {
        let mut room = Room::new("room".to_string(), "router".to_string(), None, false, None);
        let target_id = "target".to_string();
        let key = SanctionKey::User(target_id.clone());
        let muted = moderation::PunitiveState {
            cam_banned: false,
            text_muted: true,
        };
        room.participants.insert(
            target_id.clone(),
            participant(&target_id, roles::Role::User, muted.clone(), None),
        );
        room.sanctions.insert(key.clone(), muted);

        assert!(punitive_cohort_matches(
            &room,
            std::slice::from_ref(&target_id),
            &[],
            &key,
            moderation::PunitiveKind::Muted,
            true,
        ));
        assert!(!punitive_cohort_matches(
            &room,
            std::slice::from_ref(&target_id),
            &[],
            &key,
            moderation::PunitiveKind::Muted,
            false,
        ));

        room.sanctions.remove(&key);
        room.participants
            .get_mut(&target_id)
            .unwrap()
            .punitive
            .text_muted = false;
        assert!(punitive_cohort_matches(
            &room,
            std::slice::from_ref(&target_id),
            &[],
            &key,
            moderation::PunitiveKind::Muted,
            false,
        ));
    }

    #[test]
    fn guest_ip_moderation_covers_every_unauthenticated_socket() {
        let ip: IpAddr = "203.0.113.7".parse().unwrap();
        let other_ip: IpAddr = "203.0.113.8".parse().unwrap();
        let mut room = Room::new("room".to_string(), "router".to_string(), None, false, None);

        room.participants.insert(
            "target".to_string(),
            participant(
                "target",
                roles::Role::Guest,
                moderation::PunitiveState::default(),
                Some(ip),
            ),
        );
        let mut peer = participant(
            "peer",
            roles::Role::Moderator,
            moderation::PunitiveState::default(),
            Some(ip),
        );
        peer.authenticated = false;
        room.participants.insert("peer".to_string(), peer);
        room.participants.insert(
            "registered".to_string(),
            participant(
                "registered",
                roles::Role::User,
                moderation::PunitiveState::default(),
                Some(ip),
            ),
        );
        room.participants.insert(
            "other".to_string(),
            participant(
                "other",
                roles::Role::Guest,
                moderation::PunitiveState::default(),
                Some(other_ip),
            ),
        );
        let (lobby_sender, _lobby_receiver) = mpsc::channel(4);
        room.lobby.insert(
            "waiting".to_string(),
            LobbyEntry {
                participant_id: "waiting".to_string(),
                name: "waiting".to_string(),
                sender: lobby_sender,
                media_session_id: uuid::Uuid::new_v4(),
                authenticated: false,
                reconnect_token: "token".to_string(),
                in_lobby_flag: Arc::new(AtomicBool::new(true)),
                ip: Some(ip),
                role: roles::Role::Moderator,
                punitive: moderation::PunitiveState::default(),
            },
        );

        assert!(moderation_cohort(&room, "target", roles::Role::Moderator).is_err());
        assert!(guest_ip_lobby_cohort(&room, false, Some(ip), roles::Role::Moderator).is_err());

        room.participants.get_mut("peer").unwrap().role = roles::Role::User;
        room.lobby.get_mut("waiting").unwrap().role = roles::Role::User;
        let cohort = moderation_cohort(&room, "target", roles::Role::Moderator).unwrap();
        assert_eq!(cohort, vec!["peer".to_string(), "target".to_string()]);
        let lobby_cohort =
            guest_ip_lobby_cohort(&room, false, Some(ip), roles::Role::Moderator).unwrap();
        assert_eq!(lobby_cohort, vec!["waiting".to_string()]);

        let banned_ids: Vec<String> = cohort.iter().chain(lobby_cohort.iter()).cloned().collect();
        record_banned_cohort(&mut room, &banned_ids, false, Some(ip), None);
        assert!(room.identity_is_banned("target", false, Some(ip)));
        assert!(room.identity_is_banned("preopened-guest", false, Some(ip)));
        assert!(!room.identity_is_banned("registered", true, Some(ip)));
        assert!(room.banned_guest_participants.contains("target"));
        assert!(room.banned_guest_participants.contains("peer"));
        assert!(room.banned_guest_participants.contains("waiting"));
    }

    #[test]
    fn guest_ipv6_moderation_matches_one_64_but_not_an_adjacent_prefix() {
        let first: IpAddr = "2001:db8:1:2::1".parse().unwrap();
        let rotated: IpAddr = "2001:db8:1:2:ffff:ffff:ffff:42".parse().unwrap();
        let adjacent: IpAddr = "2001:db8:1:3::1".parse().unwrap();
        let mut room = Room::new("room".to_string(), "router".to_string(), None, false, None);

        for (id, ip) in [
            ("target", first),
            ("rotated", rotated),
            ("adjacent", adjacent),
        ] {
            room.participants.insert(
                id.to_string(),
                participant(
                    id,
                    roles::Role::Guest,
                    moderation::PunitiveState::default(),
                    Some(ip),
                ),
            );
        }

        let cohort = moderation_cohort(&room, "target", roles::Role::Moderator).unwrap();
        assert_eq!(cohort, vec!["rotated".to_string(), "target".to_string()]);

        record_banned_cohort(&mut room, &cohort, false, Some(first), None);
        assert!(room.identity_is_banned("new-socket", false, Some(rotated)));
        assert!(!room.identity_is_banned("outside-prefix", false, Some(adjacent)));
        assert_eq!(
            SanctionKey::for_identity("first", false, Some(first)),
            SanctionKey::for_identity("rotated", false, Some(rotated))
        );
        assert_ne!(
            SanctionKey::for_identity("first", false, Some(first)),
            SanctionKey::for_identity("adjacent", false, Some(adjacent))
        );
    }

    #[test]
    fn guest_ip_punitive_state_is_applied_consistently_to_the_cohort() {
        let ip: IpAddr = "203.0.113.7".parse().unwrap();
        let mut room = Room::new("room".to_string(), "router".to_string(), None, false, None);
        room.participants.insert(
            "first".to_string(),
            participant(
                "first",
                roles::Role::Guest,
                moderation::PunitiveState {
                    cam_banned: false,
                    text_muted: true,
                },
                Some(ip),
            ),
        );
        room.participants.insert(
            "second".to_string(),
            participant(
                "second",
                roles::Role::Guest,
                moderation::PunitiveState {
                    cam_banned: true,
                    text_muted: false,
                },
                Some(ip),
            ),
        );
        let (lobby_sender, _lobby_receiver) = mpsc::channel(4);
        room.lobby.insert(
            "waiting".to_string(),
            LobbyEntry {
                participant_id: "waiting".to_string(),
                name: "waiting".to_string(),
                sender: lobby_sender,
                media_session_id: uuid::Uuid::new_v4(),
                authenticated: false,
                reconnect_token: "token".to_string(),
                in_lobby_flag: Arc::new(AtomicBool::new(true)),
                ip: Some(ip),
                role: roles::Role::Guest,
                punitive: moderation::PunitiveState {
                    cam_banned: true,
                    text_muted: true,
                },
            },
        );
        let cohort = vec!["first".to_string(), "second".to_string()];
        let lobby_cohort = vec!["waiting".to_string()];

        let punitive = apply_punitive_to_cohort(
            &mut room,
            &cohort,
            &lobby_cohort,
            SanctionKey::GuestIp(ip),
            moderation::PunitiveKind::Muted,
            false,
        );
        assert_eq!(
            punitive,
            moderation::PunitiveState {
                cam_banned: true,
                text_muted: false,
            }
        );
        assert_eq!(room.participants["first"].punitive, punitive);
        assert_eq!(room.participants["second"].punitive, punitive);
        assert_eq!(room.lobby["waiting"].punitive, punitive);

        apply_punitive_to_cohort(
            &mut room,
            &cohort,
            &lobby_cohort,
            SanctionKey::GuestIp(ip),
            moderation::PunitiveKind::CamBanned,
            false,
        );
        assert!(!room.sanctions.contains_key(&SanctionKey::GuestIp(ip)));
        assert_eq!(
            room.participants["first"].punitive,
            moderation::PunitiveState::default()
        );
        assert_eq!(
            room.participants["second"].punitive,
            moderation::PunitiveState::default()
        );
        assert_eq!(
            room.lobby["waiting"].punitive,
            moderation::PunitiveState::default()
        );
    }

    #[test]
    fn expired_guest_ban_pruning_clears_participant_origin_marker() {
        let mut room = Room::new("room".to_string(), "router".to_string(), None, false, None);
        room.banned_participants.insert(
            "guest".to_string(),
            Some(std::time::Instant::now() - std::time::Duration::from_secs(1)),
        );
        room.banned_guest_participants.insert("guest".to_string());

        room.prune_expired_bans(std::time::Instant::now());
        assert!(!room.banned_participants.contains_key("guest"));
        assert!(!room.banned_guest_participants.contains("guest"));
    }

    #[test]
    fn stale_join_policy_snapshot_is_rejected() {
        let mut room = Room::new("room".to_string(), "router".to_string(), None, false, None);
        let snapshot = room.policy_revision;
        assert!(policy_snapshot_matches(&room, snapshot));
        room.policy_revision = room.policy_revision.wrapping_add(1);
        assert!(!policy_snapshot_matches(&room, snapshot));
    }

    #[test]
    fn deletion_tombstone_rejects_in_flight_room_commits() {
        let mut room = Room::new("room".to_string(), "router".to_string(), None, true, None);
        let snapshot = room.policy_revision;
        room.deleting = true;
        room.policy_revision = room.policy_revision.wrapping_add(1);

        assert!(room.ensure_live().is_err());
        assert!(!policy_snapshot_matches(&room, snapshot));
    }

    #[test]
    fn only_the_owning_delete_task_releases_a_room_reservation() {
        let deleting = StdRwLock::new(HashMap::new());
        let owner = uuid::Uuid::new_v4();
        deleting.write().unwrap().insert("room".to_string(), owner);

        release_deletion_reservation(&deleting, "room", uuid::Uuid::new_v4());
        assert_eq!(deleting.read().unwrap().get("room"), Some(&owner));

        release_deletion_reservation(&deleting, "room", owner);
        assert!(!deleting.read().unwrap().contains_key("room"));
    }

    #[test]
    fn failed_router_teardown_retains_the_room_reservation() {
        let deleting = StdRwLock::new(HashMap::new());
        let owner = uuid::Uuid::new_v4();
        deleting.write().unwrap().insert("room".to_string(), owner);

        release_deletion_reservation_after_router_teardown(&deleting, "room", owner, false);
        assert_eq!(deleting.read().unwrap().get("room"), Some(&owner));

        release_deletion_reservation_after_router_teardown(&deleting, "room", owner, true);
        assert!(!deleting.read().unwrap().contains_key("room"));
    }

    #[test]
    fn sender_and_media_session_identify_one_room_incarnation() {
        let (old_sender, _old_receiver) = mpsc::channel(1);
        let (new_sender, _new_receiver) = mpsc::channel(1);
        let media_session_id = uuid::Uuid::new_v4();
        let mut room = Room::new("room".to_string(), "router".to_string(), None, false, None);
        let mut member = participant(
            "member",
            roles::Role::User,
            moderation::PunitiveState::default(),
            None,
        );
        member.sender = old_sender.clone();
        member.media_session_id = media_session_id;
        room.participants.insert("member".to_string(), member);

        assert!(RoomManager::participant_for_sender(&room, "member", &old_sender).is_ok());
        assert!(RoomManager::participant_for_sender(&room, "member", &new_sender).is_err());
        assert!(RoomManager::participant_session_is_current(
            &room,
            "member",
            &old_sender,
            media_session_id,
        ));
        assert!(!RoomManager::participant_session_is_current(
            &room,
            "member",
            &old_sender,
            uuid::Uuid::new_v4(),
        ));
    }

    #[test]
    fn media_namespaces_do_not_alias_same_uuid_rejoins() {
        let participant_id = uuid::Uuid::new_v4().to_string();
        let first =
            RoomManager::media_participant_id("room", &participant_id, uuid::Uuid::new_v4());
        let second =
            RoomManager::media_participant_id("room", &participant_id, uuid::Uuid::new_v4());
        assert_ne!(first, second);
    }

    #[test]
    fn lobby_admission_preserves_resolved_role_and_sanctions() {
        let participant_id = uuid::Uuid::new_v4().to_string();
        let (sender, _receiver) = mpsc::channel(4);
        let mut room = Room::new("room".to_string(), "router".to_string(), None, false, None);
        room.sanctions.insert(
            SanctionKey::User(participant_id.clone()),
            moderation::PunitiveState {
                cam_banned: true,
                text_muted: false,
            },
        );
        let entry = LobbyEntry {
            participant_id,
            name: "member".to_string(),
            sender,
            media_session_id: uuid::Uuid::new_v4(),
            authenticated: true,
            reconnect_token: "token".to_string(),
            in_lobby_flag: Arc::new(AtomicBool::new(true)),
            ip: None,
            role: roles::Role::Member,
            punitive: moderation::PunitiveState {
                cam_banned: false,
                text_muted: true,
            },
        };
        let (role, punitive) = lobby_admission_state(&room, &entry);
        assert_eq!(role, roles::Role::Member);
        assert!(punitive.cam_banned && punitive.text_muted);
    }

    #[test]
    fn restrictive_policy_removes_existing_unauthorized_producers() {
        let mut settings = RoomManager::default_room_settings("room");
        settings.guests_can_broadcast = false;
        let mut room = Room::new(
            "room".to_string(),
            "router".to_string(),
            Some(settings),
            false,
            None,
        );
        let mut guest = participant(
            "guest",
            roles::Role::Guest,
            moderation::PunitiveState::default(),
            Some("203.0.113.8".parse().unwrap()),
        );
        guest.producers.insert(
            "microphone".to_string(),
            (MediaKind::Audio, Some("microphone".to_string())),
        );
        room.producer_to_participant
            .insert("microphone".to_string(), "guest".to_string());
        room.participants.insert("guest".to_string(), guest);

        let revoked = RoomManager::revoke_unauthorized_producers(&mut room, None, "policy changed");
        assert_eq!(revoked.len(), 1);
        assert!(room.participants["guest"].producers.is_empty());
        assert!(!room.producer_to_participant.contains_key("microphone"));
    }

    #[test]
    fn moderated_role_demotion_removes_existing_producers() {
        let mut settings = RoomManager::default_room_settings("room");
        settings.moderated = true;
        let mut room = Room::new(
            "room".to_string(),
            "router".to_string(),
            Some(settings),
            false,
            None,
        );
        let mut member = participant(
            "member",
            roles::Role::User,
            moderation::PunitiveState::default(),
            None,
        );
        member.producers.insert(
            "camera".to_string(),
            (MediaKind::Video, Some("camera".to_string())),
        );
        room.participants.insert("member".to_string(), member);

        let revoked =
            RoomManager::revoke_unauthorized_producers(&mut room, Some("member"), "role changed");
        assert_eq!(revoked.len(), 1);
        assert!(room.participants["member"].producers.is_empty());
    }

    #[test]
    fn media_source_must_match_kind_and_cam_ban_uses_kind() {
        assert!(RoomManager::valid_media_source(
            MediaKind::Audio,
            "microphone"
        ));
        assert!(RoomManager::valid_media_source(MediaKind::Video, "camera"));
        assert!(!RoomManager::valid_media_source(
            MediaKind::Video,
            "microphone"
        ));

        let (sender, _receiver) = mpsc::channel(1);
        let participant = Participant {
            id: uuid::Uuid::new_v4().to_string(),
            name: "test".to_string(),
            sender,
            media_session_id: uuid::Uuid::new_v4(),
            producers: HashMap::new(),
            role: roles::Role::Guest,
            punitive: moderation::PunitiveState {
                cam_banned: true,
                text_muted: false,
            },
            authenticated: false,
            ip: None,
        };
        let room = Room::new("room".to_string(), "router".to_string(), None, false, None);
        assert!(!RoomManager::participant_can_produce(
            &room,
            &participant,
            MediaKind::Video,
            "camera"
        ));
        assert!(RoomManager::participant_can_produce(
            &room,
            &participant,
            MediaKind::Audio,
            "microphone"
        ));
    }

    #[test]
    fn guest_broadcast_and_chat_settings_are_enforceable() {
        let (sender, _receiver) = mpsc::channel(1);
        let participant = Participant {
            id: uuid::Uuid::new_v4().to_string(),
            name: "guest".to_string(),
            sender,
            media_session_id: uuid::Uuid::new_v4(),
            producers: HashMap::new(),
            role: roles::Role::Guest,
            punitive: moderation::PunitiveState::default(),
            authenticated: false,
            ip: None,
        };
        let mut settings = RoomManager::default_room_settings("room");
        settings.guests_can_broadcast = false;
        settings.allow_chat = false;
        let room = Room::new(
            "room".to_string(),
            "router".to_string(),
            Some(settings),
            false,
            None,
        );
        assert!(!RoomManager::participant_can_produce(
            &room,
            &participant,
            MediaKind::Audio,
            "microphone"
        ));
        assert!(!room.settings.as_ref().unwrap().allow_chat);
    }
}
