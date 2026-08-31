#![forbid(unsafe_code)]

// WebSocket connection handler for individual clients

use super::protocol::{ClientMessage, ServerMessage};
use crate::auth::types::Claims;
use crate::metrics::ServerMetrics;
use crate::room::{JoinResult, RoomManager, settings};
use crate::turn::TurnConfig;
use axum::extract::ws::{Message, WebSocket};
use futures_util::{SinkExt, StreamExt};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::RwLock as StdRwLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::OwnedSemaphorePermit;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

/// Bounded channel capacity per client.
/// At 100 msg/s rate limit, 64 slots = 640ms of burst buffer.
/// Messages queued beyond this are stale — drop them early.
const CHANNEL_CAPACITY: usize = 64;
const MAX_SIGNAL_MESSAGE_LEN: usize = 64 * 1024;
const SEND_TIMEOUT: Duration = Duration::from_secs(10);

/// Idle timeout — close connection if no message received within this duration.
/// Prevents Slowloris-style attacks that hold semaphore permits indefinitely.
const IDLE_TIMEOUT: Duration = Duration::from_secs(300); // 5 minutes

/// Token bucket rate limiter: max tokens (burst capacity).
const RATE_LIMIT_MAX_TOKENS: u64 = 100;
/// Token bucket: refill rate in tokens per second.
const RATE_LIMIT_REFILL_RATE: u64 = 100;
/// Internal: 1 token in microseconds (for integer math).
const TOKEN_US: u64 = 1_000_000;
/// Internal: max tokens in microseconds.
const MAX_TOKENS_US: u64 = RATE_LIMIT_MAX_TOKENS * TOKEN_US;
/// Repeated overruns indicate a sender that is ignoring backpressure. Stop
/// reading it instead of spending an unbounded amount of CPU discarding frames.
const RATE_LIMIT_VIOLATION_WINDOW: Duration = Duration::from_secs(10);
const RATE_LIMIT_MAX_VIOLATIONS: u32 = 3;

/// Chat is broadcast work, so it has a much smaller per-connection budget than
/// ordinary point-to-point signaling.
const CHAT_RATE_LIMIT_MAX_TOKENS: u64 = 5;
const CHAT_RATE_LIMIT_REFILL_RATE: u64 = 2;
const MAX_CHAT_TOKENS_US: u64 = CHAT_RATE_LIMIT_MAX_TOKENS * TOKEN_US;

/// Producer mutations cross the mediasoup IPC boundary and may fan out to
/// every participant. Keep their budget well below ordinary signaling while
/// retaining enough burst capacity for microphone/camera/screen-share setup.
const MEDIA_MUTATION_RATE_LIMIT_MAX_TOKENS: u64 = 8;
const MEDIA_MUTATION_RATE_LIMIT_REFILL_RATE: u64 = 2;
const MAX_MEDIA_MUTATION_TOKENS_US: u64 = MEDIA_MUTATION_RATE_LIMIT_MAX_TOKENS * TOKEN_US;
const MEDIA_MUTATION_VIOLATION_WINDOW: Duration = Duration::from_secs(10);
const MEDIA_MUTATION_MAX_VIOLATIONS: u32 = 3;

/// A media session needs two transport creations and two DTLS connects during
/// normal startup. Leave room for immediate ICE recovery, but prevent repeated
/// transport/ICE requests from turning one socket into an IPC work generator.
const TRANSPORT_MUTATION_RATE_LIMIT_MAX_TOKENS: u64 = 6;
const TRANSPORT_MUTATION_RATE_LIMIT_REFILL_RATE: u64 = 1;
const MAX_TRANSPORT_MUTATION_TOKENS_US: u64 = TRANSPORT_MUTATION_RATE_LIMIT_MAX_TOKENS * TOKEN_US;

/// Joining a full default-size media session can create and resume sixteen
/// consumers, with an optional layer selection for each. Preserve that burst
/// while sharply bounding sustained consumer IPC churn.
const CONSUMER_MUTATION_RATE_LIMIT_MAX_TOKENS: u64 = 48;
const CONSUMER_MUTATION_RATE_LIMIT_REFILL_RATE: u64 = 4;
const MAX_CONSUMER_MUTATION_TOKENS_US: u64 = CONSUMER_MUTATION_RATE_LIMIT_MAX_TOKENS * TOKEN_US;

/// Moderation and room-settings messages can persist state or broadcast to an
/// entire room. Keep them on a separate, human-scale budget so an authorized
/// socket cannot turn those operations into DB or fan-out amplification.
const ADMIN_MUTATION_RATE_LIMIT_MAX_TOKENS: u64 = 20;
const ADMIN_MUTATION_RATE_LIMIT_REFILL_RATE: u64 = 5;
const MAX_ADMIN_MUTATION_TOKENS_US: u64 = ADMIN_MUTATION_RATE_LIMIT_MAX_TOKENS * TOKEN_US;
const ADMIN_MUTATION_VIOLATION_WINDOW: Duration = Duration::from_secs(10);
const ADMIN_MUTATION_MAX_VIOLATIONS: u32 = 3;

/// A voice request creates action UI for every moderator. It represents state
/// a human must act on, so sending it more often than this is never useful.
const VOICE_REQUEST_COOLDOWN: Duration = Duration::from_secs(5);

/// Hashing a new room password is intentionally expensive. A connection may
/// request it only occasionally, even if it recreates ad-hoc rooms to evade a
/// per-room cooldown.
const ROOM_PASSWORD_HASH_COOLDOWN: Duration = Duration::from_secs(30);

fn consume_rate_token(
    tokens_us: &mut u64,
    last_refill: &mut Instant,
    now: Instant,
    refill_rate: u64,
    max_tokens_us: u64,
) -> bool {
    let elapsed_us =
        u64::try_from(now.duration_since(*last_refill).as_micros()).unwrap_or(u64::MAX);
    *last_refill = now;
    *tokens_us = tokens_us
        .saturating_add(elapsed_us.saturating_mul(refill_rate))
        .min(max_tokens_us);
    if *tokens_us < TOKEN_US {
        return false;
    }
    *tokens_us -= TOKEN_US;
    true
}

fn is_media_mutation(message: &ClientMessage) -> bool {
    matches!(
        message,
        ClientMessage::Produce { .. }
            | ClientMessage::CloseProducer { .. }
            | ClientMessage::PauseProducer { .. }
            | ClientMessage::ResumeProducer { .. }
    )
}

fn is_transport_mutation(message: &ClientMessage) -> bool {
    matches!(
        message,
        ClientMessage::CreateSendTransport
            | ClientMessage::CreateRecvTransport
            | ClientMessage::ConnectTransport { .. }
            | ClientMessage::RestartIce { .. }
    )
}

fn is_consumer_mutation(message: &ClientMessage) -> bool {
    matches!(
        message,
        ClientMessage::Consume { .. }
            | ClientMessage::ResumeConsumer { .. }
            | ClientMessage::PauseConsumer { .. }
            | ClientMessage::SetConsumerPreferredLayers { .. }
    )
}

/// Rate state for mediasoup operations that belong to one room/media session.
///
/// Unlike frame, chat, and administration limits, these buckets must survive a
/// grace reconnect because the reconnect deliberately retains the underlying
/// transports, producers, and consumers. Moving this state through the exact
/// reconnect-token entry prevents a new socket from restoring full media burst
/// capacity for an existing session.
struct MediaSessionRateState {
    media_mutation_tokens_us: u64,
    media_mutation_last_refill: Instant,
    transport_mutation_tokens_us: u64,
    transport_mutation_last_refill: Instant,
    consumer_mutation_tokens_us: u64,
    consumer_mutation_last_refill: Instant,
    violation_window_started: Option<Instant>,
    violations: u32,
}

impl MediaSessionRateState {
    fn new() -> Self {
        Self::new_at(Instant::now())
    }

    fn new_at(now: Instant) -> Self {
        Self {
            media_mutation_tokens_us: MAX_MEDIA_MUTATION_TOKENS_US,
            media_mutation_last_refill: now,
            transport_mutation_tokens_us: MAX_TRANSPORT_MUTATION_TOKENS_US,
            transport_mutation_last_refill: now,
            consumer_mutation_tokens_us: MAX_CONSUMER_MUTATION_TOKENS_US,
            consumer_mutation_last_refill: now,
            violation_window_started: None,
            violations: 0,
        }
    }

    fn limit_exceeded(&mut self, message: &ClientMessage, now: Instant) -> bool {
        if is_media_mutation(message) {
            !consume_rate_token(
                &mut self.media_mutation_tokens_us,
                &mut self.media_mutation_last_refill,
                now,
                MEDIA_MUTATION_RATE_LIMIT_REFILL_RATE,
                MAX_MEDIA_MUTATION_TOKENS_US,
            )
        } else if is_transport_mutation(message) {
            !consume_rate_token(
                &mut self.transport_mutation_tokens_us,
                &mut self.transport_mutation_last_refill,
                now,
                TRANSPORT_MUTATION_RATE_LIMIT_REFILL_RATE,
                MAX_TRANSPORT_MUTATION_TOKENS_US,
            )
        } else if is_consumer_mutation(message) {
            !consume_rate_token(
                &mut self.consumer_mutation_tokens_us,
                &mut self.consumer_mutation_last_refill,
                now,
                CONSUMER_MUTATION_RATE_LIMIT_REFILL_RATE,
                MAX_CONSUMER_MUTATION_TOKENS_US,
            )
        } else {
            false
        }
    }

    fn record_violation(&mut self, now: Instant) -> u32 {
        if self
            .violation_window_started
            .is_none_or(|started| now.duration_since(started) >= MEDIA_MUTATION_VIOLATION_WINDOW)
        {
            self.violation_window_started = Some(now);
            self.violations = 0;
        }
        self.violations = self.violations.saturating_add(1);
        self.violations
    }
}

fn is_admin_mutation(message: &ClientMessage) -> bool {
    matches!(
        message,
        ClientMessage::CloseCam { .. }
            | ClientMessage::CamBan { .. }
            | ClientMessage::CamUnban { .. }
            | ClientMessage::TextMute { .. }
            | ClientMessage::TextUnmute { .. }
            | ClientMessage::Kick { .. }
            | ClientMessage::Ban { .. }
            | ClientMessage::Unban { .. }
            | ClientMessage::SetRole { .. }
            | ClientMessage::UpdateRoomSettings { .. }
            | ClientMessage::SetTopic { .. }
            | ClientMessage::AdmitFromLobby { .. }
            | ClientMessage::DenyFromLobby { .. }
    )
}

/// Grace period entry for a disconnected participant
struct GraceEntry {
    reconnect_token: String,
    authenticated_subject: Option<String>,
    /// Media limiter state for this exact room-session incarnation.
    media_rate_state: MediaSessionRateState,
    /// Sender channel that owned the participant session when it disconnected.
    /// `same_channel` is the immutable connection-incarnation check.
    sender: mpsc::Sender<Arc<String>>,
    timer: tokio::task::JoinHandle<()>,
}

/// Shared map of participants in grace period (disconnected but not yet removed)
#[derive(Clone)]
pub struct GracePeriodMap {
    inner: Arc<StdRwLock<HashMap<(String, String, String), GraceEntry>>>,
    max_entries: usize,
}

impl GracePeriodMap {
    pub fn new() -> Self {
        Self::with_capacity(10_000)
    }

    pub fn with_capacity(max_entries: usize) -> Self {
        Self {
            inner: Arc::new(StdRwLock::new(HashMap::new())),
            max_entries: max_entries.max(1),
        }
    }

    /// Retain a disconnected session without evicting another reconnectable
    /// participant. On capacity exhaustion the new timer is aborted and the
    /// caller must immediately remove that participant's room/media state.
    fn insert(&self, room_id: String, participant_id: String, entry: GraceEntry) -> bool {
        let mut map = self.inner.write().unwrap_or_else(|e| e.into_inner());
        let key = (room_id, participant_id, entry.reconnect_token.clone());
        if map.len() >= self.max_entries && !map.contains_key(&key) {
            entry.timer.abort();
            return false;
        }
        // Separate connection incarnations for the same stable UUID may briefly
        // coexist after kick/rejoin. Key by the per-session reconnect token so
        // a stale disconnect cannot abort a replacement session's grace timer.
        if let Some(old) = map.remove(&key) {
            old.timer.abort();
        }
        map.insert(key, entry);
        true
    }

    fn remove_if_token_matches(
        &self,
        room_id: &str,
        participant_id: &str,
        reconnect_token: &str,
        authenticated_subject: Option<&str>,
    ) -> Option<GraceEntry> {
        let mut map = self.inner.write().unwrap_or_else(|e| e.into_inner());
        let key = (
            room_id.to_string(),
            participant_id.to_string(),
            reconnect_token.to_string(),
        );
        if map
            .get(&key)
            .is_some_and(|entry| entry.authenticated_subject.as_deref() == authenticated_subject)
        {
            map.remove(&key)
        } else {
            None
        }
    }
}

/// Serialize a ServerMessage and send it through the channel as pre-serialized JSON.
fn send_json(sender: &mpsc::Sender<Arc<String>>, msg: &ServerMessage) -> anyhow::Result<()> {
    let json = Arc::new(serde_json::to_string(msg)?);
    sender.try_send(json).map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(())
}

fn begin_join_session(reconnect_token: &mut String) -> String {
    let fresh = Uuid::new_v4().to_string();
    *reconnect_token = fresh.clone();
    fresh
}

fn reconnect_attempt_allowed(current_room_id: Option<&str>, in_lobby: bool) -> bool {
    current_room_id.is_none() && !in_lobby
}

/// Handles a single WebSocket connection
pub async fn handle_connection(
    socket: WebSocket,
    room_manager: Arc<RoomManager>,
    turn_config: Option<Arc<TurnConfig>>,
    grace_periods: GracePeriodMap,
    metrics: ServerMetrics,
    _permit: OwnedSemaphorePermit,
    authenticated_user: Option<Claims>,
    client_ip: Option<std::net::IpAddr>,
) {
    // Use authenticated user ID if available, otherwise generate anonymous UUID
    let mut participant_id = authenticated_user
        .as_ref()
        .map(|c| c.sub.clone())
        .unwrap_or_else(|| Uuid::new_v4().to_string());

    let is_authenticated = authenticated_user.is_some();
    let authenticated_display_name = authenticated_user.as_ref().map(|c| c.name.clone());
    let auth_exp = authenticated_user.as_ref().map(|claims| claims.exp as u64);

    info!(
        "New WebSocket connection: {} (authenticated: {})",
        participant_id, is_authenticated
    );

    metrics.inc_connections_total();
    let _conn_guard = metrics.connection_active_guard();

    // Generate reconnect token for this session
    let mut reconnect_token = Uuid::new_v4().to_string();

    let (mut ws_sender, mut ws_receiver) = socket.split();

    // Bounded channel for sending messages to this client
    let (tx, mut rx) = mpsc::channel::<Arc<String>>(CHANNEL_CAPACITY);

    // Clone for the send task
    let participant_id_clone = participant_id.clone();
    let send_metrics = metrics.clone();

    // Spawn task to send messages to client
    let send_task = tokio::spawn(async move {
        while let Some(json) = rx.recv().await {
            send_metrics.inc_messages_sent();
            match tokio::time::timeout(
                SEND_TIMEOUT,
                ws_sender.send(Message::Text((*json).clone().into())),
            )
            .await
            {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    debug!(%error, "WebSocket send failed");
                    break;
                }
                Err(_) => {
                    warn!(
                        participant_id = participant_id_clone,
                        "WebSocket send timed out"
                    );
                    break;
                }
            }
        }
        debug!(
            "Send task finished for participant: {}",
            participant_id_clone
        );
    });

    // Handle incoming messages
    let mut current_room_id: Option<String> = None;
    // True while the participant waits in a lobby. Shared (Arc) because admission
    // happens on the moderator's connection task — admit_from_lobby clears it via
    // the LobbyEntry so this task's guard opens without any local event.
    let in_lobby = Arc::new(AtomicBool::new(false));
    let mut stats_task: Option<tokio::task::JoinHandle<()>> = None;
    let mut bwe_sender: Option<mpsc::Sender<u32>> = None;

    // Token bucket rate limiter state
    let mut frame_tokens_us: u64 = MAX_TOKENS_US;
    let mut frame_last_refill = Instant::now();
    let mut rate_limit_window_started: Option<Instant> = None;
    let mut rate_limit_violations = 0_u32;
    let mut chat_tokens_us: u64 = MAX_CHAT_TOKENS_US;
    let mut chat_last_refill = Instant::now();
    let mut media_rate_state = MediaSessionRateState::new();
    let mut admin_mutation_tokens_us: u64 = MAX_ADMIN_MUTATION_TOKENS_US;
    let mut admin_mutation_last_refill = Instant::now();
    let mut admin_mutation_violation_window_started: Option<Instant> = None;
    let mut admin_mutation_violations = 0_u32;
    let mut last_room_password_hash: Option<Instant> = None;
    let mut last_join_attempt: Option<Instant> = None;
    let mut last_voice_request: Option<Instant> = None;

    loop {
        // The JWT is a connection credential, not only a handshake credential.
        // Cap every receive wait by its absolute expiry so active traffic cannot
        // keep an authenticated socket alive indefinitely.
        let now_unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(u64::MAX, |duration| duration.as_secs());
        if auth_exp.is_some_and(|exp| exp <= now_unix) {
            info!(participant_id, "JWT expired; closing WebSocket");
            break;
        }
        let receive_timeout = auth_exp
            .map(|exp| Duration::from_secs(exp.saturating_sub(now_unix)))
            .map_or(IDLE_TIMEOUT, |remaining| remaining.min(IDLE_TIMEOUT));

        let receive_result = tokio::select! {
            _ = tx.closed() => break,
            result = tokio::time::timeout(receive_timeout, ws_receiver.next()) => result,
        };
        let msg = match receive_result {
            Ok(Some(Ok(message))) => message,
            Ok(Some(Err(_))) | Ok(None) => break, // Stream error or closed
            Err(_) => {
                if auth_exp
                    .is_some_and(|exp| exp <= now_unix.saturating_add(receive_timeout.as_secs()))
                {
                    info!(participant_id, "JWT expired; closing WebSocket");
                } else {
                    warn!("Idle timeout for participant {}", participant_id);
                }
                break;
            }
        };

        // Every inbound frame consumes rate-limit capacity. Limiting only text
        // frames lets binary/control-frame floods bypass connection accounting.
        metrics.inc_messages_received();
        let now = Instant::now();
        if !consume_rate_token(
            &mut frame_tokens_us,
            &mut frame_last_refill,
            now,
            RATE_LIMIT_REFILL_RATE,
            MAX_TOKENS_US,
        ) {
            if matches!(&msg, Message::Close(_)) {
                break;
            }
            if rate_limit_window_started
                .is_none_or(|started| now.duration_since(started) >= RATE_LIMIT_VIOLATION_WINDOW)
            {
                rate_limit_window_started = Some(now);
                rate_limit_violations = 0;
            }
            rate_limit_violations = rate_limit_violations.saturating_add(1);
            if rate_limit_violations == 1 {
                warn!("Rate limit exceeded for participant {}", participant_id);
                let _ = send_json(
                    &tx,
                    &ServerMessage::Error {
                        message: format!(
                            "Rate limit exceeded: max {} frames/second",
                            RATE_LIMIT_REFILL_RATE
                        ),
                    },
                );
            }
            if rate_limit_violations >= RATE_LIMIT_MAX_VIOLATIONS {
                warn!(
                    participant_id,
                    rate_limit_violations, "Closing WebSocket after repeated frame-rate violations"
                );
                break;
            }
            continue;
        }

        match msg {
            Message::Text(text) => {
                if text.len() > MAX_SIGNAL_MESSAGE_LEN {
                    warn!(
                        participant_id,
                        size = text.len(),
                        "Oversized signaling message"
                    );
                    break;
                }

                match serde_json::from_str::<ClientMessage>(&text) {
                    Ok(client_msg) => {
                        let now = Instant::now();
                        let media_limit_exceeded =
                            media_rate_state.limit_exceeded(&client_msg, now);
                        if media_limit_exceeded {
                            let media_mutation_violations = media_rate_state.record_violation(now);
                            let _ = send_json(
                                &tx,
                                &ServerMessage::Error {
                                    message: "Media changes are rate limited".to_string(),
                                },
                            );
                            if media_mutation_violations >= MEDIA_MUTATION_MAX_VIOLATIONS {
                                warn!(
                                    participant_id,
                                    media_mutation_violations,
                                    "Closing WebSocket after repeated media-mutation rate violations"
                                );
                                break;
                            }
                            continue;
                        }

                        if is_admin_mutation(&client_msg)
                            && !consume_rate_token(
                                &mut admin_mutation_tokens_us,
                                &mut admin_mutation_last_refill,
                                now,
                                ADMIN_MUTATION_RATE_LIMIT_REFILL_RATE,
                                MAX_ADMIN_MUTATION_TOKENS_US,
                            )
                        {
                            if admin_mutation_violation_window_started.is_none_or(|started| {
                                now.duration_since(started) >= ADMIN_MUTATION_VIOLATION_WINDOW
                            }) {
                                admin_mutation_violation_window_started = Some(now);
                                admin_mutation_violations = 0;
                            }
                            admin_mutation_violations = admin_mutation_violations.saturating_add(1);
                            let _ = send_json(
                                &tx,
                                &ServerMessage::Error {
                                    message: "Room administration changes are rate limited"
                                        .to_string(),
                                },
                            );
                            if admin_mutation_violations >= ADMIN_MUTATION_MAX_VIOLATIONS {
                                warn!(
                                    participant_id,
                                    admin_mutation_violations,
                                    "Closing WebSocket after repeated room-administration rate violations"
                                );
                                break;
                            }
                            continue;
                        }

                        if matches!(&client_msg, ClientMessage::ChatMessage { .. })
                            && !consume_rate_token(
                                &mut chat_tokens_us,
                                &mut chat_last_refill,
                                Instant::now(),
                                CHAT_RATE_LIMIT_REFILL_RATE,
                                MAX_CHAT_TOKENS_US,
                            )
                        {
                            warn!(participant_id, "Closing WebSocket for chat flooding");
                            let _ = send_json(
                                &tx,
                                &ServerMessage::Error {
                                    message: "Chat rate limit exceeded".to_string(),
                                },
                            );
                            break;
                        }

                        if matches!(
                            &client_msg,
                            ClientMessage::UpdateRoomSettings {
                                password: Some(Some(_)),
                                ..
                            }
                        ) {
                            let now = Instant::now();
                            if last_room_password_hash.is_some_and(|previous| {
                                now.duration_since(previous) < ROOM_PASSWORD_HASH_COOLDOWN
                            }) {
                                let _ = send_json(
                                    &tx,
                                    &ServerMessage::Error {
                                        message: "Room password updates are rate limited"
                                            .to_string(),
                                    },
                                );
                                continue;
                            }
                            last_room_password_hash = Some(now);
                        }

                        if matches!(&client_msg, ClientMessage::JoinRoom { .. }) {
                            let now = Instant::now();
                            if last_join_attempt.is_some_and(|previous| {
                                now.duration_since(previous) < Duration::from_secs(1)
                            }) {
                                let _ = send_json(
                                    &tx,
                                    &ServerMessage::Error {
                                        message: "Join attempts are rate limited".to_string(),
                                    },
                                );
                                continue;
                            }
                            last_join_attempt = Some(now);
                        }

                        if matches!(&client_msg, ClientMessage::RequestVoice) {
                            let now = Instant::now();
                            if last_voice_request.is_some_and(|previous| {
                                now.duration_since(previous) < VOICE_REQUEST_COOLDOWN
                            }) {
                                let _ = send_json(
                                    &tx,
                                    &ServerMessage::Error {
                                        message: "Voice requests are rate limited".to_string(),
                                    },
                                );
                                continue;
                            }
                            last_voice_request = Some(now);
                        }

                        // Handle reconnect specially — it must be processed before
                        // any room-dependent messages
                        if let ClientMessage::Reconnect {
                            participant_id: reconnect_id,
                            room_id: reconnect_room,
                            reconnect_token: client_token,
                        } = &client_msg
                        {
                            if !reconnect_attempt_allowed(
                                current_room_id.as_deref(),
                                in_lobby.load(Ordering::Acquire),
                            ) {
                                warn!(
                                    participant_id,
                                    "Rejected reconnect from a socket with an active room session"
                                );
                                let _ = send_json(
                                    &tx,
                                    &ServerMessage::ReconnectResult {
                                        success: false,
                                        participant_id: reconnect_id.clone(),
                                        reconnect_token: None,
                                    },
                                );
                                continue;
                            }

                            let restored_media_rate_state = handle_reconnect(
                                reconnect_id,
                                reconnect_room,
                                client_token,
                                authenticated_user
                                    .as_ref()
                                    .map(|claims| claims.sub.as_str()),
                                &grace_periods,
                                &room_manager,
                                &tx,
                            )
                            .await;

                            let success = restored_media_rate_state.is_some();
                            let fresh_reconnect_token = success.then(|| Uuid::new_v4().to_string());

                            if let Some(restored_media_rate_state) = restored_media_rate_state {
                                media_rate_state = restored_media_rate_state;
                                participant_id = reconnect_id.clone();
                                current_room_id = Some(reconnect_room.clone());

                                // Restart stats task for reconnected session
                                if let Some(task) = stats_task.take() {
                                    task.abort();
                                }

                                let (bwe_tx, bwe_rx) = mpsc::channel::<u32>(32);
                                // Try to subscribe immediately (recv transport may exist from previous session)
                                if let Err(e) = room_manager
                                    .subscribe_bwe_events(
                                        reconnect_room,
                                        &participant_id,
                                        &tx,
                                        bwe_tx.clone(),
                                    )
                                    .await
                                {
                                    debug!(
                                        "BWE subscription deferred for reconnecting {}: {}",
                                        participant_id, e
                                    );
                                }
                                bwe_sender = Some(bwe_tx);

                                stats_task = Some(spawn_stats_task(
                                    room_manager.clone(),
                                    reconnect_room.clone(),
                                    participant_id.clone(),
                                    tx.clone(),
                                    bwe_rx,
                                ));
                            }

                            let response_sent = send_json(
                                &tx,
                                &ServerMessage::ReconnectResult {
                                    success,
                                    participant_id: if success {
                                        participant_id.clone()
                                    } else {
                                        reconnect_id.clone()
                                    },
                                    reconnect_token: fresh_reconnect_token.clone(),
                                },
                            );
                            if response_sent.is_ok()
                                && let Some(fresh_token) = fresh_reconnect_token
                            {
                                // Only retire the client-visible token after the
                                // replacement has been queued successfully.
                                reconnect_token = fresh_token;
                            }
                            continue;
                        }

                        // Track room changes to manage stats task lifecycle
                        let was_in_room = current_room_id.is_some();
                        let is_join = matches!(&client_msg, ClientMessage::JoinRoom { .. });
                        let is_leave = matches!(&client_msg, ClientMessage::LeaveRoom);
                        let previous_reconnect_token = reconnect_token.clone();

                        let start = Instant::now();
                        let result = handle_client_message(
                            &client_msg,
                            &participant_id,
                            &mut current_room_id,
                            &in_lobby,
                            &tx,
                            &room_manager,
                            &turn_config,
                            &metrics,
                            &mut reconnect_token,
                            &bwe_sender,
                            is_authenticated,
                            authenticated_display_name.as_deref(),
                            client_ip,
                        )
                        .await;
                        metrics.observe_message_handling(start.elapsed());

                        // A successful JoinRoom creates a new media-session
                        // incarnation and rotates its reconnect token. Give that
                        // genuinely new session a fresh budget; failed join
                        // attempts against an existing session must not reset it.
                        if is_join
                            && current_room_id.is_some()
                            && reconnect_token != previous_reconnect_token
                        {
                            media_rate_state = MediaSessionRateState::new();
                        }

                        if let Err(e) = result {
                            // Invalid IDs, stale media state, and authorization
                            // failures are routine client-controlled outcomes.
                            // Keep them observable without allowing a socket to
                            // amplify them into production error logs.
                            debug!(
                                participant_id,
                                error = %e,
                                "Client signaling request was rejected"
                            );
                            metrics.inc_errors();
                            // If channel is closed, send task has exited — break
                            if tx.is_closed() {
                                break;
                            }
                            if send_json(
                                &tx,
                                &ServerMessage::Error {
                                    message: "Request could not be completed".to_string(),
                                },
                            )
                            .is_err()
                            {
                                break;
                            }
                        }

                        if is_join && current_room_id.is_some() {
                            if !in_lobby.load(Ordering::Acquire)
                                && let Some(task) = stats_task.take()
                            {
                                // Joined a new room directly — restart stats below
                                task.abort();
                            }
                        }

                        // Ensure a stats task runs whenever we are a full room member.
                        // Invariant-based (not join-triggered) so it also covers lobby
                        // admission: admit_from_lobby clears in_lobby, and the admitted
                        // client's first media-setup message lands here.
                        if current_room_id.is_some()
                            && !in_lobby.load(Ordering::Acquire)
                            && stats_task.is_none()
                        {
                            // Create BWE event channel (subscription happens later in CreateRecvTransport)
                            let (bwe_tx, bwe_rx) = mpsc::channel::<u32>(32);
                            bwe_sender = Some(bwe_tx);

                            stats_task = Some(spawn_stats_task(
                                room_manager.clone(),
                                current_room_id.as_ref().unwrap().clone(),
                                participant_id.clone(),
                                tx.clone(),
                                bwe_rx,
                            ));
                        }

                        // Stop stats task when leaving a room
                        if is_leave || (was_in_room && current_room_id.is_none()) {
                            if let Some(task) = stats_task.take() {
                                task.abort();
                            }
                            bwe_sender = None;
                        }
                    }
                    Err(e) => {
                        debug!(
                            participant_id,
                            error = %e,
                            "Invalid client signaling message"
                        );
                        metrics.inc_errors();
                        let _ = send_json(
                            &tx,
                            &ServerMessage::Error {
                                message: "Invalid message format".to_string(),
                            },
                        );
                    }
                }
            }
            Message::Binary(payload) => {
                warn!(
                    participant_id,
                    size = payload.len(),
                    "Unsupported binary WebSocket frame; closing connection"
                );
                break;
            }
            Message::Close(_) => {
                info!("Client {} closed connection", participant_id);
                break;
            }
            Message::Ping(_) | Message::Pong(_) => {
                // WebSocket ping/pong handled automatically
            }
        }
    }

    // Stop stats task
    if let Some(task) = stats_task.take() {
        task.abort();
    }

    // On disconnect: lobby participants clean up immediately, room participants get grace period
    if let Some(room_id) = current_room_id.take() {
        if in_lobby.load(Ordering::Acquire) {
            // Lobby participants have no transports/media — clean up immediately
            info!(
                "Lobby participant {} disconnected from room {}, cleaning up immediately",
                participant_id, room_id
            );
            if let Err(e) = room_manager
                .remove_participant_for_sender(&room_id, &participant_id, &tx)
                .await
            {
                error!("Error removing lobby participant: {}", e);
            }
        } else {
            info!(
                "Participant {} disconnected from room {}, starting 30s grace period",
                participant_id, room_id
            );

            let grace_map = grace_periods.clone();
            let rm = room_manager.clone();
            let pid = participant_id.clone();
            let rid = room_id.clone();
            let timer_token = reconnect_token.clone();
            let authenticated_subject =
                authenticated_user.as_ref().map(|claims| claims.sub.clone());
            let timer_subject = authenticated_subject.clone();
            let timer_sender = tx.clone();

            let timer = tokio::spawn(async move {
                tokio::time::sleep(tokio::time::Duration::from_secs(30)).await;
                // Only the timer that still owns this exact grace entry may
                // remove the participant. A reconnect or replacement timer can
                // win the race at the deadline without being torn down by an
                // older task.
                if grace_map
                    .remove_if_token_matches(&rid, &pid, &timer_token, timer_subject.as_deref())
                    .is_some()
                {
                    info!(
                        "Grace period expired for participant {} in room {}",
                        pid, rid
                    );
                    if let Err(e) = rm
                        .remove_participant_for_sender(&rid, &pid, &timer_sender)
                        .await
                    {
                        error!("Error removing participant after grace period: {}", e);
                    }
                }
            });

            let retained_for_reconnect = grace_periods.insert(
                room_id.clone(),
                participant_id.clone(),
                GraceEntry {
                    reconnect_token,
                    authenticated_subject,
                    media_rate_state,
                    sender: tx.clone(),
                    timer,
                },
            );
            if !retained_for_reconnect {
                warn!(
                    room_id,
                    participant_id,
                    "Grace-session capacity reached; cleaning up disconnected participant immediately"
                );
                if let Err(error) = room_manager
                    .remove_participant_for_sender(&room_id, &participant_id, &tx)
                    .await
                {
                    debug!(
                        room_id,
                        participant_id,
                        %error,
                        "Immediate grace-cap cleanup found no current participant session"
                    );
                }
            }
        }
    }

    // _conn_guard dropped here → dec_connections_active
    // _permit dropped here → release semaphore

    drop(tx);
    send_task.abort();
    let _ = send_task.await;

    info!(
        "Connection handler finished for participant: {}",
        participant_id
    );
}

/// Attempt to reconnect a participant to their existing session
async fn handle_reconnect(
    participant_id: &str,
    room_id: &str,
    client_token: &str,
    authenticated_subject: Option<&str>,
    grace_periods: &GracePeriodMap,
    room_manager: &Arc<RoomManager>,
    new_sender: &mpsc::Sender<Arc<String>>,
) -> Option<MediaSessionRateState> {
    if !settings::valid_room_id(room_id)
        || participant_id.parse::<Uuid>().is_err()
        || client_token.parse::<Uuid>().is_err()
    {
        return None;
    }
    // Check if participant is in grace period
    if let Some(entry) = grace_periods.remove_if_token_matches(
        room_id,
        participant_id,
        client_token,
        authenticated_subject,
    ) {
        // Cancel the cleanup timer
        entry.timer.abort();

        // Rebind the sender in the room's participant list
        match room_manager
            .rebind_participant_sender(
                room_id,
                participant_id,
                authenticated_subject,
                &entry.sender,
                new_sender.clone(),
            )
            .await
        {
            Ok(true) => {
                info!(
                    "Participant {} reconnected to room {}",
                    participant_id, room_id
                );
                Some(entry.media_rate_state)
            }
            Ok(false) => {
                warn!(
                    "Participant {} not found in room {} during reconnect",
                    participant_id, room_id
                );
                let _ = room_manager
                    .remove_participant_for_sender(room_id, participant_id, &entry.sender)
                    .await;
                None
            }
            Err(e) => {
                error!("Error during reconnect for {}: {}", participant_id, e);
                let _ = room_manager
                    .remove_participant_for_sender(room_id, participant_id, &entry.sender)
                    .await;
                None
            }
        }
    } else {
        debug!("No grace period found for participant {}", participant_id);
        None
    }
}

/// Spawns a background task that performs bandwidth adaptation and sends connection stats.
///
/// Uses event-driven BWE (bandwidth estimation) events for layer adaptation:
/// - BWE events arrive via channel when mediasoup detects bandwidth changes
/// - Layer preferences update immediately on tier change (debounced at 2s)
/// - ConnectionStats sent to client every ~10s using last-known bitrate (no IPC, skipped if unchanged)
fn spawn_stats_task(
    room_manager: Arc<RoomManager>,
    room_id: String,
    participant_id: String,
    sender: mpsc::Sender<Arc<String>>,
    mut bwe_rx: mpsc::Receiver<u32>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut current_layers: HashMap<String, u8> = HashMap::new();
        let mut last_bitrate: u32 = 0;
        let mut last_sent_bitrate: u32 = 0;
        let mut last_tier: Option<u8> = None;

        // Minimum interval between layer updates (debounce rapid BWE fluctuations)
        let mut last_layer_update = Instant::now();
        let debounce = Duration::from_secs(2);

        // ConnectionStats interval — no IPC call, just uses last-known bitrate
        let mut stats_interval = tokio::time::interval(Duration::from_secs(10));
        stats_interval.tick().await; // Skip first immediate tick

        loop {
            tokio::select! {
                // BWE event: bandwidth estimation changed
                bwe = bwe_rx.recv() => {
                    match bwe {
                        Some(bitrate) => {
                            last_bitrate = bitrate;

                            let target_layer = if bitrate < 200_000 {
                                0u8
                            } else if bitrate < 600_000 {
                                1u8
                            } else {
                                2u8
                            };

                            // Only update layers on tier change, with debounce
                            if last_tier != Some(target_layer) && last_layer_update.elapsed() >= debounce {
                                last_tier = Some(target_layer);
                                last_layer_update = Instant::now();

                                // Get current consumer IDs (in-memory only — zero IPC)
                                match room_manager
                                    .get_consumer_ids(&room_id, &participant_id, &sender)
                                    .await
                                {
                                    Ok(consumer_ids) if !consumer_ids.is_empty() => {
                                        for consumer_id in &consumer_ids {
                                            let prev = current_layers.get(consumer_id).copied();
                                            if prev != Some(target_layer) {
                                                if let Err(e) = room_manager.set_preferred_layers(
                                                    &room_id,
                                                    &participant_id,
                                                    &sender,
                                                    consumer_id,
                                                    target_layer,
                                                    None,
                                                ).await {
                                                    debug!("Failed to set layers for consumer {}: {}", consumer_id, e);
                                                } else {
                                                    current_layers.insert(consumer_id.clone(), target_layer);
                                                }
                                            }
                                        }
                                        current_layers.retain(|id, _| consumer_ids.contains(id));
                                    }
                                    Ok(_) => {
                                        // No consumers — skip layer updates, clear stale state
                                        current_layers.clear();
                                    }
                                    Err(e) => {
                                        debug!("Failed to get consumer IDs for {}: {}", participant_id, e);
                                    }
                                }
                            }
                        }
                        None => break, // Channel closed — participant disconnected
                    }
                }

                // Timer: send ConnectionStats every ~10s using last-known bitrate (zero IPC)
                _ = stats_interval.tick() => {
                    if last_bitrate > 0 && last_bitrate != last_sent_bitrate {
                        last_sent_bitrate = last_bitrate;
                        if let Ok(json) = serde_json::to_string(&ServerMessage::ConnectionStats {
                            available_bitrate: Some(last_bitrate),
                            rtt: None,
                        }) {
                            let _ = sender.try_send(Arc::new(json));
                        }
                    }
                }
            }
        }
    })
}

/// Generate ICE servers with an unlinkable TURN identity for each issuance.
fn make_ice_servers(turn_config: &Option<Arc<TurnConfig>>) -> Vec<crate::turn::IceServer> {
    match turn_config {
        Some(tc) => vec![tc.generate_credentials()],
        None => vec![],
    }
}

const MAX_PARTICIPANT_NAME_LEN: usize = 64;
const MAX_TARGET_ID_LEN: usize = 128;
const MAX_CHAT_LEN: usize = 4096;

fn validate_target_id(id: &str) -> anyhow::Result<()> {
    if id.is_empty() || id.len() > MAX_TARGET_ID_LEN || id.parse::<Uuid>().is_err() {
        anyhow::bail!("Invalid target participant ID");
    }
    Ok(())
}

fn validate_reason(reason: Option<&str>) -> anyhow::Result<()> {
    if reason.is_some_and(|reason| reason.len() > 1_024 || reason.chars().any(char::is_control)) {
        anyhow::bail!("Reason must be at most 1024 characters without control characters");
    }
    Ok(())
}

fn validate_durable_reason(reason: Option<&str>) -> anyhow::Result<()> {
    if reason.is_some_and(|reason| reason.len() > 256 || reason.chars().any(char::is_control)) {
        anyhow::bail!("Reason must be at most 256 characters without control characters");
    }
    Ok(())
}

/// Handle a single client message
async fn handle_client_message(
    message: &ClientMessage,
    participant_id: &str,
    current_room_id: &mut Option<String>,
    in_lobby: &Arc<AtomicBool>,
    sender: &mpsc::Sender<Arc<String>>,
    room_manager: &Arc<RoomManager>,
    turn_config: &Option<Arc<TurnConfig>>,
    metrics: &ServerMetrics,
    reconnect_token: &mut String,
    bwe_sender: &Option<mpsc::Sender<u32>>,
    is_authenticated: bool,
    authenticated_display_name: Option<&str>,
    client_ip: Option<std::net::IpAddr>,
) -> anyhow::Result<()> {
    // Block media/moderation operations for lobby participants
    if in_lobby.load(Ordering::Acquire) {
        match message {
            ClientMessage::JoinRoom { .. }
            | ClientMessage::LeaveRoom
            | ClientMessage::ChatMessage { .. }
            | ClientMessage::Reconnect { .. } => {} // allowed while in lobby
            _ => anyhow::bail!("Cannot perform this action while waiting in lobby"),
        }
    }

    // A moderator can remove the participant while this socket remains open.
    // Re-check authoritative membership before every room operation so the
    // stale socket cannot continue creating or controlling media.
    if !matches!(
        message,
        ClientMessage::JoinRoom { .. } | ClientMessage::LeaveRoom | ClientMessage::Reconnect { .. }
    ) {
        if let Some(room_id) = current_room_id.as_ref() {
            if !room_manager
                .is_bound_participant(room_id, participant_id, sender)
                .await
            {
                anyhow::bail!("Participant is no longer in this room");
            }
        }
    }

    match message {
        ClientMessage::JoinRoom {
            room_id,
            participant_name,
            password,
        } => {
            if !settings::valid_room_id(room_id) {
                anyhow::bail!(
                    "Invalid room_id: use 1-128 letters, numbers, hyphens, or underscores"
                );
            }
            let participant_name = authenticated_display_name.unwrap_or(participant_name);
            if participant_name.trim().is_empty()
                || participant_name.len() > MAX_PARTICIPANT_NAME_LEN
                || participant_name.chars().any(char::is_control)
            {
                anyhow::bail!(
                    "Invalid participant_name: must be 1-{MAX_PARTICIPANT_NAME_LEN} characters without control characters"
                );
            }
            if password.as_ref().is_some_and(|password| {
                password.is_empty()
                    || password.len() > settings::MAX_PASSWORD_LEN
                    || password.chars().any(char::is_control)
            }) {
                anyhow::bail!("Invalid room password");
            }
            // Rotate before RoomJoined is serialized. The same token remains in
            // connection state for disconnect grace and (for lobby joins) is
            // stored for the later admission message.
            let session_reconnect_token = begin_join_session(reconnect_token);
            // Leave current room if in one
            if let Some(old_room_id) = current_room_id.take() {
                room_manager
                    .remove_participant_for_sender(&old_room_id, participant_id, sender)
                    .await?;
            }

            // Join new room (may be placed in lobby)
            let join_result = room_manager
                .add_participant(
                    room_id,
                    participant_id.to_string(),
                    participant_name.to_string(),
                    sender.clone(),
                    is_authenticated,
                    in_lobby.clone(),
                    password.as_deref(),
                    &session_reconnect_token,
                    client_ip,
                )
                .await?;

            match join_result {
                JoinResult::Joined {
                    participants,
                    role,
                    room_settings,
                } => {
                    *current_room_id = Some(room_id.clone());
                    in_lobby.store(false, Ordering::Release);
                    metrics.inc_joins();

                    send_json(
                        sender,
                        &ServerMessage::RoomJoined {
                            participant_id: participant_id.to_string(),
                            participants,
                            reconnect_token: session_reconnect_token,
                            your_role: role,
                            room_settings,
                        },
                    )?;
                }
                JoinResult::Lobbied => {
                    // Set current_room_id so disconnect cleanup removes from lobby
                    *current_room_id = Some(room_id.clone());
                    in_lobby.store(true, Ordering::Release);
                    // Don't send RoomJoined — participant is in lobby
                    // Don't start stats task — no media for lobby participants
                    // LobbyWaiting was already sent by add_participant
                }
            }
        }

        ClientMessage::LeaveRoom => {
            if let Some(room_id) = current_room_id.take() {
                room_manager
                    .remove_participant_for_sender(&room_id, participant_id, sender)
                    .await?;
                in_lobby.store(false, Ordering::Release);
                metrics.inc_leaves();
            }
        }

        ClientMessage::GetRouterRtpCapabilities => {
            if let Some(room_id) = current_room_id.as_ref() {
                let capabilities = room_manager
                    .get_router_rtp_capabilities(room_id, participant_id, sender)
                    .await?;
                send_json(
                    sender,
                    &ServerMessage::RouterRtpCapabilities {
                        rtp_capabilities: capabilities,
                    },
                )?;
            } else {
                anyhow::bail!("Not in a room");
            }
        }

        ClientMessage::CreateSendTransport => {
            if let Some(room_id) = current_room_id.as_ref() {
                let transport_info = room_manager
                    .create_send_transport(room_id, participant_id, sender)
                    .await?;

                send_json(
                    sender,
                    &ServerMessage::TransportCreated {
                        transport_id: transport_info.id,
                        ice_parameters: transport_info.ice_parameters,
                        ice_candidates: transport_info.ice_candidates,
                        dtls_parameters: transport_info.dtls_parameters,
                        ice_servers: make_ice_servers(turn_config),
                    },
                )?;
            } else {
                anyhow::bail!("Not in a room");
            }
        }

        ClientMessage::CreateRecvTransport => {
            if let Some(room_id) = current_room_id.as_ref() {
                let transport_info = room_manager
                    .create_recv_transport(room_id, participant_id, sender)
                    .await?;

                // Subscribe to BWE events now that recv transport exists
                if let Some(bwe_tx) = bwe_sender {
                    if let Err(e) = room_manager
                        .subscribe_bwe_events(room_id, participant_id, sender, bwe_tx.clone())
                        .await
                    {
                        debug!(
                            "Failed to subscribe BWE events for {}: {}",
                            participant_id, e
                        );
                    }
                }

                send_json(
                    sender,
                    &ServerMessage::TransportCreated {
                        transport_id: transport_info.id,
                        ice_parameters: transport_info.ice_parameters,
                        ice_candidates: transport_info.ice_candidates,
                        dtls_parameters: transport_info.dtls_parameters,
                        ice_servers: make_ice_servers(turn_config),
                    },
                )?;
            } else {
                anyhow::bail!("Not in a room");
            }
        }

        ClientMessage::ConnectTransport {
            transport_id,
            dtls_parameters,
        } => {
            if let Some(room_id) = current_room_id.as_ref() {
                room_manager
                    .connect_transport(
                        room_id,
                        participant_id,
                        sender,
                        transport_id,
                        dtls_parameters.clone(),
                    )
                    .await?;

                send_json(
                    sender,
                    &ServerMessage::TransportConnected {
                        transport_id: transport_id.clone(),
                    },
                )?;
            } else {
                anyhow::bail!("Not in a room");
            }
        }

        ClientMessage::Produce {
            transport_id: _,
            kind,
            rtp_parameters,
            source,
        } => {
            if let Some(room_id) = current_room_id.as_ref() {
                let source = source.clone().unwrap_or_else(|| match kind {
                    mediasoup::prelude::MediaKind::Audio => "microphone".to_string(),
                    mediasoup::prelude::MediaKind::Video => "camera".to_string(),
                });

                // Newer Chromium omits the RTCP CNAME from its SDP, so browsers send an
                // empty cname. Consumers' mediasoup-client uses the cname as the msid
                // stream id, and Chromium rejects SDP with an empty msid stream id —
                // normalize to a per-participant cname so consuming works everywhere.
                let mut rtp_parameters = rtp_parameters.clone();
                if rtp_parameters
                    .rtcp
                    .cname
                    .as_deref()
                    .is_none_or(str::is_empty)
                {
                    rtp_parameters.rtcp.cname = Some(format!("ms-{participant_id}"));
                }

                let producer_id = room_manager
                    .create_producer(
                        room_id,
                        participant_id,
                        sender,
                        *kind,
                        rtp_parameters,
                        Some(source),
                    )
                    .await?;

                metrics.inc_producers_created();
                send_json(sender, &ServerMessage::ProducerCreated { producer_id })?;
            } else {
                anyhow::bail!("Not in a room");
            }
        }

        ClientMessage::Consume {
            producer_id,
            rtp_capabilities,
        } => {
            if let Some(room_id) = current_room_id.as_ref() {
                let producer_paused = room_manager
                    .media_server()
                    .transport_manager()
                    .find_producer_paused(producer_id)
                    .unwrap_or(false);

                let consumer_info = room_manager
                    .create_consumer(
                        room_id,
                        participant_id,
                        sender,
                        producer_id.parse()?,
                        rtp_capabilities.clone(),
                        Some(sender.clone()),
                        producer_paused,
                    )
                    .await?;

                metrics.inc_consumers_created();
                send_json(
                    sender,
                    &ServerMessage::ConsumerCreated {
                        consumer_id: consumer_info.id,
                        producer_id: consumer_info.producer_id.clone(),
                        kind: consumer_info.kind,
                        rtp_parameters: consumer_info.rtp_parameters,
                    },
                )?;

                // If the producer is already paused, immediately notify the consuming client
                // so it can hide the video tile instead of showing a black square.
                if producer_paused {
                    send_json(
                        sender,
                        &ServerMessage::ProducerPaused {
                            producer_id: producer_id.clone(),
                        },
                    )?;
                }
            } else {
                anyhow::bail!("Not in a room");
            }
        }

        ClientMessage::ResumeConsumer { consumer_id } => {
            if let Some(room_id) = current_room_id.as_ref() {
                room_manager
                    .resume_consumer(room_id, participant_id, sender, consumer_id)
                    .await?;

                send_json(
                    sender,
                    &ServerMessage::ConsumerResumed {
                        consumer_id: consumer_id.clone(),
                    },
                )?;
            } else {
                anyhow::bail!("Not in a room");
            }
        }

        ClientMessage::PauseConsumer { consumer_id } => {
            if let Some(room_id) = current_room_id.as_ref() {
                room_manager
                    .pause_consumer(room_id, participant_id, sender, consumer_id)
                    .await?;

                send_json(
                    sender,
                    &ServerMessage::ConsumerPaused {
                        consumer_id: consumer_id.clone(),
                    },
                )?;
            } else {
                anyhow::bail!("Not in a room");
            }
        }

        ClientMessage::CloseProducer { producer_id } => {
            if let Some(room_id) = current_room_id.as_ref() {
                room_manager
                    .close_producer(room_id, participant_id, sender, producer_id)
                    .await?;
            } else {
                anyhow::bail!("Not in a room");
            }
        }

        ClientMessage::PauseProducer { producer_id } => {
            if let Some(room_id) = current_room_id.as_ref() {
                room_manager
                    .pause_producer(room_id, participant_id, sender, producer_id)
                    .await?;
                send_json(
                    sender,
                    &ServerMessage::ProducerPaused {
                        producer_id: producer_id.clone(),
                    },
                )?;
            } else {
                anyhow::bail!("Not in a room");
            }
        }

        ClientMessage::ResumeProducer { producer_id } => {
            if let Some(room_id) = current_room_id.as_ref() {
                room_manager
                    .resume_producer(room_id, participant_id, sender, producer_id)
                    .await?;
                send_json(
                    sender,
                    &ServerMessage::ProducerResumed {
                        producer_id: producer_id.clone(),
                    },
                )?;
            } else {
                anyhow::bail!("Not in a room");
            }
        }

        ClientMessage::Reconnect { .. } => {
            // Handled in the main message loop before dispatching here
            // This branch should never be reached
        }

        ClientMessage::RestartIce { transport_id } => {
            if let Some(room_id) = current_room_id.as_ref() {
                let ice_parameters = room_manager
                    .restart_ice(room_id, participant_id, sender, transport_id)
                    .await?;

                send_json(
                    sender,
                    &ServerMessage::IceRestarted {
                        transport_id: transport_id.clone(),
                        ice_parameters,
                    },
                )?;
            } else {
                anyhow::bail!("Not in a room");
            }
        }

        ClientMessage::SetConsumerPreferredLayers {
            consumer_id,
            spatial_layer,
            temporal_layer,
        } => {
            if let Some(room_id) = current_room_id.as_ref() {
                room_manager
                    .set_preferred_layers(
                        room_id,
                        participant_id,
                        sender,
                        consumer_id,
                        *spatial_layer,
                        *temporal_layer,
                    )
                    .await?;
            } else {
                anyhow::bail!("Not in a room");
            }
        }

        ClientMessage::ChatMessage { content } => {
            if content.is_empty() || content.len() > MAX_CHAT_LEN {
                anyhow::bail!("Invalid chat message: must be 1-{MAX_CHAT_LEN} characters");
            }
            if let Some(room_id) = current_room_id.as_ref() {
                // Check if participant can chat
                if !room_manager
                    .can_participant_chat(room_id, participant_id, sender)
                    .await?
                {
                    anyhow::bail!("You are not allowed to chat");
                }

                room_manager
                    .broadcast_chat(room_id, participant_id, sender, content.clone())
                    .await?;
            } else {
                anyhow::bail!("Not in a room");
            }
        }

        // === Moderation ===
        ClientMessage::CloseCam {
            target_participant_id,
        } => {
            let room_id = current_room_id
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("Not in a room"))?;
            validate_target_id(target_participant_id)?;
            room_manager
                .close_cam(room_id, participant_id, sender, target_participant_id)
                .await?;
        }

        ClientMessage::CamBan {
            target_participant_id,
            reason,
        } => {
            let room_id = current_room_id
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("Not in a room"))?;
            validate_target_id(target_participant_id)?;
            validate_durable_reason(reason.as_deref())?;
            room_manager
                .cam_ban(
                    room_id,
                    participant_id,
                    sender,
                    target_participant_id,
                    reason.as_deref(),
                )
                .await?;
        }

        ClientMessage::CamUnban {
            target_participant_id,
        } => {
            let room_id = current_room_id
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("Not in a room"))?;
            validate_target_id(target_participant_id)?;
            room_manager
                .cam_unban(room_id, participant_id, sender, target_participant_id)
                .await?;
        }

        ClientMessage::TextMute {
            target_participant_id,
        } => {
            let room_id = current_room_id
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("Not in a room"))?;
            validate_target_id(target_participant_id)?;
            room_manager
                .text_mute(room_id, participant_id, sender, target_participant_id)
                .await?;
        }

        ClientMessage::TextUnmute {
            target_participant_id,
        } => {
            let room_id = current_room_id
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("Not in a room"))?;
            validate_target_id(target_participant_id)?;
            room_manager
                .text_unmute(room_id, participant_id, sender, target_participant_id)
                .await?;
        }

        ClientMessage::Kick {
            target_participant_id,
            reason,
        } => {
            let room_id = current_room_id
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("Not in a room"))?;
            validate_target_id(target_participant_id)?;
            validate_reason(reason.as_deref())?;
            room_manager
                .kick_participant(
                    room_id,
                    participant_id,
                    sender,
                    target_participant_id,
                    reason.as_deref(),
                )
                .await?;
        }

        ClientMessage::Ban {
            target_participant_id,
            reason,
            duration,
        } => {
            let room_id = current_room_id
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("Not in a room"))?;
            validate_target_id(target_participant_id)?;
            validate_durable_reason(reason.as_deref())?;
            room_manager
                .ban_participant(
                    room_id,
                    participant_id,
                    sender,
                    target_participant_id,
                    reason.as_deref(),
                    *duration,
                )
                .await?;
        }

        ClientMessage::Unban { target_user_id } => {
            let room_id = current_room_id
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("Not in a room"))?;
            validate_target_id(target_user_id)?;
            room_manager
                .unban_participant(room_id, participant_id, sender, target_user_id)
                .await?;
        }

        ClientMessage::SetRole {
            target_participant_id,
            role,
        } => {
            let room_id = current_room_id
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("Not in a room"))?;
            validate_target_id(target_participant_id)?;
            let new_role = crate::room::roles::Role::from_u8(*role)
                .ok_or_else(|| anyhow::anyhow!("Invalid role value: {}", role))?;
            room_manager
                .set_participant_role(
                    room_id,
                    participant_id,
                    sender,
                    target_participant_id,
                    new_role,
                )
                .await?;
        }

        ClientMessage::RequestVoice => {
            let room_id = current_room_id
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("Not in a room"))?;
            room_manager
                .request_voice(room_id, participant_id, sender)
                .await?;
        }

        // === Room management (stubs) ===
        ClientMessage::UpdateRoomSettings {
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
            password,
        } => {
            let room_id = current_room_id
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("Not in a room"))?;
            room_manager
                .update_room_settings(
                    room_id,
                    participant_id,
                    sender,
                    *moderated,
                    *lobby_enabled,
                    *guests_allowed,
                    *guests_can_broadcast,
                    *max_broadcasters,
                    *max_participants,
                    *allow_screen_sharing,
                    *allow_chat,
                    *allow_video,
                    *require_registration,
                    *invite_only,
                    *push_to_talk,
                    *secret,
                    password.clone(),
                )
                .await?;
        }

        ClientMessage::SetTopic { topic } => {
            let room_id = current_room_id
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("Not in a room"))?;
            room_manager
                .set_topic(room_id, participant_id, sender, topic.clone())
                .await?;
        }

        // === Lobby ===
        ClientMessage::AdmitFromLobby {
            target_participant_id,
        } => {
            let room_id = current_room_id
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("Not in a room"))?;
            validate_target_id(target_participant_id)?;
            room_manager
                .admit_from_lobby(room_id, participant_id, sender, target_participant_id)
                .await?;
        }

        ClientMessage::DenyFromLobby {
            target_participant_id,
        } => {
            let room_id = current_room_id
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("Not in a room"))?;
            validate_target_id(target_participant_id)?;
            room_manager
                .deny_from_lobby(room_id, participant_id, sender, target_participant_id, None)
                .await?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod security_tests {
    use super::*;

    #[test]
    fn token_bucket_refills_and_remains_bounded() {
        let start = Instant::now();
        let mut tokens = TOKEN_US;
        let mut last_refill = start;

        assert!(consume_rate_token(
            &mut tokens,
            &mut last_refill,
            start,
            2,
            2 * TOKEN_US,
        ));
        assert!(!consume_rate_token(
            &mut tokens,
            &mut last_refill,
            start,
            2,
            2 * TOKEN_US,
        ));
        assert!(consume_rate_token(
            &mut tokens,
            &mut last_refill,
            start + Duration::from_millis(500),
            2,
            2 * TOKEN_US,
        ));

        assert!(consume_rate_token(
            &mut tokens,
            &mut last_refill,
            start + Duration::from_secs(10),
            2,
            2 * TOKEN_US,
        ));
        assert!(consume_rate_token(
            &mut tokens,
            &mut last_refill,
            start + Duration::from_secs(10),
            2,
            2 * TOKEN_US,
        ));
        assert!(!consume_rate_token(
            &mut tokens,
            &mut last_refill,
            start + Duration::from_secs(10),
            2,
            2 * TOKEN_US,
        ));
    }

    #[test]
    fn media_mutation_budgets_cover_all_client_driven_mediasoup_ipc() {
        assert!(is_media_mutation(&ClientMessage::Produce {
            transport_id: Uuid::new_v4().to_string(),
            kind: mediasoup::prelude::MediaKind::Audio,
            rtp_parameters: mediasoup::prelude::RtpParameters::default(),
            source: Some("microphone".to_string()),
        }));
        assert!(is_media_mutation(&ClientMessage::CloseProducer {
            producer_id: Uuid::new_v4().to_string(),
        }));
        assert!(is_media_mutation(&ClientMessage::PauseProducer {
            producer_id: Uuid::new_v4().to_string(),
        }));
        assert!(is_media_mutation(&ClientMessage::ResumeProducer {
            producer_id: Uuid::new_v4().to_string(),
        }));
        assert!(is_transport_mutation(&ClientMessage::CreateSendTransport));
        assert!(is_transport_mutation(&ClientMessage::CreateRecvTransport));
        assert!(is_transport_mutation(&ClientMessage::RestartIce {
            transport_id: Uuid::new_v4().to_string(),
        }));
        assert!(is_consumer_mutation(&ClientMessage::PauseConsumer {
            consumer_id: Uuid::new_v4().to_string(),
        }));
        assert!(is_consumer_mutation(&ClientMessage::ResumeConsumer {
            consumer_id: Uuid::new_v4().to_string(),
        }));
        assert!(is_consumer_mutation(
            &ClientMessage::SetConsumerPreferredLayers {
                consumer_id: Uuid::new_v4().to_string(),
                spatial_layer: 1,
                temporal_layer: Some(1),
            }
        ));
        assert!(!is_media_mutation(&ClientMessage::RequestVoice));
        assert!(!is_transport_mutation(&ClientMessage::RequestVoice));
        assert!(!is_consumer_mutation(&ClientMessage::RequestVoice));
        assert!(!is_media_mutation(&ClientMessage::ChatMessage {
            content: "hello".to_string(),
        }));
    }

    #[tokio::test]
    async fn grace_reconnect_does_not_restore_consumer_burst_capacity() {
        let map = GracePeriodMap::new();
        let participant_id = Uuid::new_v4().to_string();
        let token = Uuid::new_v4().to_string();
        let now = Instant::now();
        let consumer_mutation = ClientMessage::PauseConsumer {
            consumer_id: Uuid::new_v4().to_string(),
        };
        let transport_mutation = ClientMessage::RestartIce {
            transport_id: Uuid::new_v4().to_string(),
        };
        let mut entry = grace_entry(&token);
        entry.media_rate_state = MediaSessionRateState::new_at(now);

        for _ in 0..CONSUMER_MUTATION_RATE_LIMIT_MAX_TOKENS {
            assert!(
                !entry
                    .media_rate_state
                    .limit_exceeded(&consumer_mutation, now)
            );
        }
        for _ in 0..TRANSPORT_MUTATION_RATE_LIMIT_MAX_TOKENS {
            assert!(
                !entry
                    .media_rate_state
                    .limit_exceeded(&transport_mutation, now)
            );
        }
        assert!(
            entry
                .media_rate_state
                .limit_exceeded(&consumer_mutation, now)
        );
        assert!(
            entry
                .media_rate_state
                .limit_exceeded(&transport_mutation, now)
        );
        assert_eq!(entry.media_rate_state.record_violation(now), 1);
        assert_eq!(entry.media_rate_state.record_violation(now), 2);

        map.insert("room-a".to_string(), participant_id.clone(), entry);
        let mut restored = map
            .remove_if_token_matches("room-a", &participant_id, &token, None)
            .expect("the matching reconnect token must recover its session state");
        restored.timer.abort();

        assert!(
            restored
                .media_rate_state
                .limit_exceeded(&consumer_mutation, now)
        );
        assert!(
            restored
                .media_rate_state
                .limit_exceeded(&transport_mutation, now)
        );
        assert_eq!(restored.media_rate_state.record_violation(now), 3);
    }

    #[tokio::test]
    async fn reconnect_rate_state_is_isolated_between_session_incarnations() {
        let map = GracePeriodMap::new();
        let participant_id = Uuid::new_v4().to_string();
        let stale_token = Uuid::new_v4().to_string();
        let replacement_token = Uuid::new_v4().to_string();
        let now = Instant::now();
        let mutation = ClientMessage::Produce {
            transport_id: Uuid::new_v4().to_string(),
            kind: mediasoup::prelude::MediaKind::Audio,
            rtp_parameters: mediasoup::prelude::RtpParameters::default(),
            source: Some("microphone".to_string()),
        };

        let mut stale = grace_entry(&stale_token);
        stale.media_rate_state = MediaSessionRateState::new_at(now);
        for _ in 0..MEDIA_MUTATION_RATE_LIMIT_MAX_TOKENS {
            assert!(!stale.media_rate_state.limit_exceeded(&mutation, now));
        }
        assert!(stale.media_rate_state.limit_exceeded(&mutation, now));

        let mut replacement = grace_entry(&replacement_token);
        replacement.media_rate_state = MediaSessionRateState::new_at(now);
        map.insert("room-a".to_string(), participant_id.clone(), stale);
        map.insert("room-a".to_string(), participant_id.clone(), replacement);

        let mut replacement = map
            .remove_if_token_matches("room-a", &participant_id, &replacement_token, None)
            .expect("replacement incarnation should remain independently addressable");
        replacement.timer.abort();
        assert!(!replacement.media_rate_state.limit_exceeded(&mutation, now));

        let mut stale = map
            .remove_if_token_matches("room-a", &participant_id, &stale_token, None)
            .expect("stale incarnation should remain independently addressable");
        stale.timer.abort();
        assert!(stale.media_rate_state.limit_exceeded(&mutation, now));
    }

    #[test]
    fn admin_mutation_budget_covers_moderation_and_settings_messages() {
        assert!(is_admin_mutation(&ClientMessage::CloseCam {
            target_participant_id: "target".to_string(),
        }));
        assert!(is_admin_mutation(&ClientMessage::SetTopic {
            topic: "topic".to_string(),
        }));
        assert!(is_admin_mutation(&ClientMessage::UpdateRoomSettings {
            moderated: Some(true),
            lobby_enabled: None,
            guests_allowed: None,
            guests_can_broadcast: None,
            max_broadcasters: None,
            max_participants: None,
            allow_screen_sharing: None,
            allow_chat: None,
            allow_video: None,
            require_registration: None,
            invite_only: None,
            push_to_talk: None,
            secret: None,
            password: None,
        }));
        assert!(is_admin_mutation(&ClientMessage::DenyFromLobby {
            target_participant_id: "target".to_string(),
        }));
        assert!(!is_admin_mutation(&ClientMessage::RequestVoice));
        assert!(!is_admin_mutation(&ClientMessage::ChatMessage {
            content: "hello".to_string(),
        }));
    }

    fn grace_entry(token: &str) -> GraceEntry {
        let (sender, _receiver) = mpsc::channel(1);
        GraceEntry {
            reconnect_token: token.to_string(),
            authenticated_subject: None,
            media_rate_state: MediaSessionRateState::new(),
            sender,
            timer: tokio::spawn(std::future::pending()),
        }
    }

    #[test]
    fn joined_token_is_the_token_retained_for_disconnect() {
        let mut retained = "old-token".to_string();
        let presented_in_room_joined = begin_join_session(&mut retained);
        assert_ne!(presented_in_room_joined, "old-token");
        assert_eq!(presented_in_room_joined, retained);
    }

    #[test]
    fn reconnect_is_only_allowed_from_an_unbound_socket() {
        assert!(reconnect_attempt_allowed(None, false));
        assert!(!reconnect_attempt_allowed(Some("room-a"), false));
        assert!(!reconnect_attempt_allowed(None, true));
    }

    #[test]
    fn successful_reconnect_result_carries_rotated_token() {
        let token = Uuid::new_v4().to_string();
        let json = serde_json::to_value(ServerMessage::ReconnectResult {
            success: true,
            participant_id: Uuid::new_v4().to_string(),
            reconnect_token: Some(token.clone()),
        })
        .unwrap();
        assert_eq!(
            json.get("reconnectToken").and_then(|value| value.as_str()),
            Some(token.as_str())
        );
    }

    #[tokio::test]
    async fn wrong_reconnect_token_does_not_consume_or_extend_grace() {
        let map = GracePeriodMap::new();
        let participant_id = Uuid::new_v4().to_string();
        let token = Uuid::new_v4().to_string();
        map.insert(
            "room-a".to_string(),
            participant_id.clone(),
            grace_entry(&token),
        );

        assert!(
            map.remove_if_token_matches("room-a", &participant_id, "wrong", None)
                .is_none()
        );
        let entry = map
            .remove_if_token_matches("room-a", &participant_id, &token, None)
            .expect("valid token must still work after a failed attempt");
        entry.timer.abort();
    }

    #[tokio::test]
    async fn grace_entries_are_scoped_by_room() {
        let map = GracePeriodMap::new();
        let participant_id = Uuid::new_v4().to_string();
        let token_a = Uuid::new_v4().to_string();
        let token_b = Uuid::new_v4().to_string();
        map.insert(
            "room-a".to_string(),
            participant_id.clone(),
            grace_entry(&token_a),
        );
        map.insert(
            "room-b".to_string(),
            participant_id.clone(),
            grace_entry(&token_b),
        );

        let first = map
            .remove_if_token_matches("room-a", &participant_id, &token_a, None)
            .unwrap();
        let second = map
            .remove_if_token_matches("room-b", &participant_id, &token_b, None)
            .unwrap();
        first.timer.abort();
        second.timer.abort();
    }

    #[tokio::test]
    async fn grace_capacity_rejects_new_state_without_evicting_reconnectable_session() {
        let map = GracePeriodMap::with_capacity(1);
        let first_id = Uuid::new_v4().to_string();
        let first_token = Uuid::new_v4().to_string();
        let rejected_id = Uuid::new_v4().to_string();
        let rejected_token = Uuid::new_v4().to_string();

        assert!(map.insert(
            "room-a".to_string(),
            first_id.clone(),
            grace_entry(&first_token),
        ));
        assert!(!map.insert(
            "room-b".to_string(),
            rejected_id.clone(),
            grace_entry(&rejected_token),
        ));
        assert!(
            map.remove_if_token_matches("room-b", &rejected_id, &rejected_token, None)
                .is_none()
        );

        let retained = map
            .remove_if_token_matches("room-a", &first_id, &first_token, None)
            .expect("capacity failure must not evict an existing grace session");
        retained.timer.abort();
    }

    #[tokio::test]
    async fn stale_disconnect_does_not_replace_same_uuid_grace_entry() {
        let map = GracePeriodMap::new();
        let participant_id = Uuid::new_v4().to_string();
        let stale_token = Uuid::new_v4().to_string();
        let replacement_token = Uuid::new_v4().to_string();
        map.insert(
            "room-a".to_string(),
            participant_id.clone(),
            grace_entry(&stale_token),
        );
        map.insert(
            "room-a".to_string(),
            participant_id.clone(),
            grace_entry(&replacement_token),
        );

        let stale = map
            .remove_if_token_matches("room-a", &participant_id, &stale_token, None)
            .expect("stale and replacement incarnations must coexist");
        let replacement = map
            .remove_if_token_matches("room-a", &participant_id, &replacement_token, None)
            .expect("stale disconnect must not abort replacement grace");
        stale.timer.abort();
        replacement.timer.abort();
    }

    #[tokio::test]
    async fn authenticated_grace_requires_same_refreshed_identity() {
        let map = GracePeriodMap::new();
        let participant_id = Uuid::new_v4().to_string();
        let token = Uuid::new_v4().to_string();
        let mut entry = grace_entry(&token);
        entry.authenticated_subject = Some(participant_id.clone());
        map.insert("room-a".to_string(), participant_id.clone(), entry);

        assert!(
            map.remove_if_token_matches("room-a", &participant_id, &token, None)
                .is_none()
        );
        let entry = map
            .remove_if_token_matches("room-a", &participant_id, &token, Some(&participant_id))
            .unwrap();
        entry.timer.abort();
    }
}
