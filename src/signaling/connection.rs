#![forbid(unsafe_code)]

// WebSocket connection handler for individual clients

use super::protocol::{ClientMessage, RequestHeader, ServerMessage, ServerReply};
use crate::auth::types::Claims;
use crate::diagnostics::{self, OperationKind, Outcome, Stage};
use crate::metrics::ServerMetrics;
use crate::room::{JoinResult, RoomManager, RoomPasswordRequired, settings};
use crate::turn::TurnConfig;
use axum::extract::ws::{CloseFrame, Message, WebSocket, close_code};
use futures_util::{Sink, SinkExt, Stream, StreamExt};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::RwLock as StdRwLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio::sync::{Notify, OwnedSemaphorePermit};
use tracing::{Instrument, debug, error, info, warn};
use uuid::Uuid;

#[path = "connection_authentication.rs"]
mod authentication;
pub use authentication::RenewalAuthenticator;
use authentication::{RenewalBudget, RenewalOutcome, renew_authentication, unix_seconds};
#[path = "connection_credentials.rs"]
mod credentials;
use credentials::{
    CredentialContinuity, CredentialStatus, account_credentials_current,
    account_credentials_current_owned,
};

/// Bounded channel capacity per client.
/// At 100 msg/s rate limit, 64 slots = 640ms of burst buffer.
/// Messages queued beyond this are stale — drop them early.
const CHANNEL_CAPACITY: usize = 64;
const MAX_SIGNAL_MESSAGE_LEN: usize = 64 * 1024;
const SEND_TIMEOUT: Duration = Duration::from_secs(10);
const DRAIN_SEND_TIMEOUT: Duration = Duration::from_secs(1);
const PEER_CLOSE_TIMEOUT: Duration = Duration::from_secs(1);

/// Connection-owned tasks must not detach if a handler is cancelled at the
/// process drain deadline. Aborting an already completed task is harmless.
struct OwnedTask(tokio::task::JoinHandle<()>);

impl OwnedTask {
    fn abort(&self) {
        self.0.abort();
    }
}

impl Drop for OwnedTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Flush the protocol's queued peer-close reply without sending more app data.
///
/// The split sink can retain an unsent text frame, so stop its owner before
/// driving the existing receiver to EOF. Tungstenite queues the exact close
/// echo when it reads Close and flushes it on the next read. Do not manufacture
/// another Close or dispatch any further application messages here.
///
/// Writer cancellation and receiver completion share a short deadline. This
/// consumes the writer's join result; callers must not await that handle again.
async fn complete_peer_close<S, E>(receiver: &mut S, writer: &mut OwnedTask) -> Outcome
where
    S: Stream<Item = Result<Message, E>> + Unpin,
{
    writer.abort();
    match tokio::time::timeout(PEER_CLOSE_TIMEOUT, async {
        let _ = (&mut writer.0).await;
        loop {
            match receiver.next().await {
                None => return Outcome::Ok,
                Some(Err(_)) => return Outcome::Error,
                // Keep the deadline enforceable even if buffered frames stay
                // immediately ready; none may re-enter application dispatch.
                Some(Ok(_)) => tokio::task::yield_now().await,
            }
        }
    })
    .await
    {
        Ok(outcome) => outcome,
        Err(_) => Outcome::Timeout,
    }
}

/// Idle timeout — close connection if no message received within this duration.
/// Prevents Slowloris-style attacks that hold semaphore permits indefinitely.
const IDLE_TIMEOUT: Duration = Duration::from_secs(300); // 5 minutes
/// Protocol Ping keeps quiet joined browsers responsive without relying on
/// background-tab JavaScript timers. Unjoined sockets retain the idle cutoff.
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);
const HEARTBEAT_MEMBERSHIP_TIMEOUT: Duration = Duration::from_millis(100);
const AUTH_REVALIDATE_INTERVAL: Duration = Duration::from_secs(5);

/// Internal timing policy keeps short socket regressions independent of process
/// environment. Production always uses the fixed five-minute idle deadline.
#[derive(Clone, Copy)]
struct ConnectionTiming {
    idle_timeout: Duration,
    heartbeat_interval: Duration,
    membership_timeout: Duration,
}

impl Default for ConnectionTiming {
    fn default() -> Self {
        Self {
            idle_timeout: IDLE_TIMEOUT,
            heartbeat_interval: HEARTBEAT_INTERVAL,
            membership_timeout: HEARTBEAT_MEMBERSHIP_TIMEOUT,
        }
    }
}

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

/// Permit create, resume and one layer selection for every configured consumer.
/// Keep sustained churn bounded independently of the initial join burst.
const CONSUMER_MUTATION_RATE_LIMIT_REFILL_RATE: u64 = 4;

fn consumer_mutation_capacity(consumer_limit: usize) -> u64 {
    u64::try_from(consumer_limit)
        .unwrap_or(u64::MAX)
        .saturating_mul(3 * TOKEN_US)
}

fn frame_capacity(consumer_capacity: u64) -> u64 {
    // The outer frame limiter must also admit a full media setup, including
    // join/capabilities and the independent producer/transport bursts.
    consumer_capacity
        .saturating_add(MAX_MEDIA_MUTATION_TOKENS_US)
        .saturating_add(MAX_TRANSPORT_MUTATION_TOKENS_US)
        .saturating_add(2 * TOKEN_US)
        .max(MAX_TOKENS_US)
}

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

#[derive(Default)]
struct JoinAttemptState {
    last_attempt: Option<Instant>,
    password_challenge_room: Option<String>,
}

impl JoinAttemptState {
    fn allow(&mut self, room_id: &str, has_password: bool, now: Instant) -> bool {
        // One response to an issued password challenge belongs to the same
        // interaction, even when a password manager answers immediately.
        // RoomManager's shared IP and room/IP budgets still count both requests.
        let answering_challenge = self
            .password_challenge_room
            .take()
            .is_some_and(|pending| pending == room_id && has_password);
        if !answering_challenge
            && self
                .last_attempt
                .is_some_and(|previous| now.duration_since(previous) < Duration::from_secs(1))
        {
            return false;
        }
        self.last_attempt = Some(now);
        true
    }
}

fn client_error_response(error: &anyhow::Error) -> ServerMessage {
    if error.is::<RoomPasswordRequired>() {
        ServerMessage::RoomPasswordRequired
    } else {
        ServerMessage::Error {
            message: "Request could not be completed".to_string(),
        }
    }
}

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
            | ClientMessage::CloseConsumer { .. }
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
    consumer_mutation_capacity_us: u64,
    consumer_mutation_last_refill: Instant,
    violation_window_started: Option<Instant>,
    violations: u32,
}

impl MediaSessionRateState {
    fn new() -> Self {
        Self::new_at(Instant::now())
    }

    fn new_at(now: Instant) -> Self {
        Self::with_consumer_limit(
            now,
            crate::media::transport_manager::max_consumers_per_participant(),
        )
    }

    fn with_consumer_limit(now: Instant, consumer_limit: usize) -> Self {
        let consumer_mutation_capacity_us = consumer_mutation_capacity(consumer_limit);
        Self {
            media_mutation_tokens_us: MAX_MEDIA_MUTATION_TOKENS_US,
            media_mutation_last_refill: now,
            transport_mutation_tokens_us: MAX_TRANSPORT_MUTATION_TOKENS_US,
            transport_mutation_last_refill: now,
            consumer_mutation_tokens_us: consumer_mutation_capacity_us,
            consumer_mutation_capacity_us,
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
                self.consumer_mutation_capacity_us,
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
    message.social_request().is_some()
        || matches!(
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
    authenticated_version: Option<i64>,
    /// Media limiter state for this exact room-session incarnation.
    media_rate_state: MediaSessionRateState,
    /// Sender channel that owned the participant session when it disconnected.
    /// `same_channel` is the immutable connection-incarnation check.
    sender: mpsc::Sender<crate::OutboundJson>,
    timer: tokio::task::JoinHandle<()>,
}

type GraceEntries = HashMap<(String, String, String), GraceEntry>;

/// Shared map of participants in grace period (disconnected but not yet removed)
#[derive(Clone)]
pub struct GracePeriodMap {
    inner: Arc<StdRwLock<GraceEntries>>,
    max_entries: usize,
    closed: Arc<AtomicBool>,
}

impl Default for GracePeriodMap {
    fn default() -> Self {
        Self::new()
    }
}

impl GracePeriodMap {
    pub fn new() -> Self {
        Self::with_capacity(10_000)
    }

    pub fn with_capacity(max_entries: usize) -> Self {
        Self {
            inner: Arc::new(StdRwLock::new(HashMap::new())),
            max_entries: max_entries.max(1),
            closed: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Stop only this map's owned timers. Global room drain removes retained
    /// memberships; closing also rejects timers created concurrently afterwards.
    pub(super) fn close(&self) -> usize {
        let mut map = self
            .inner
            .write()
            .unwrap_or_else(|error| error.into_inner());
        self.closed.store(true, Ordering::Release);
        let count = map.len();
        for (_, entry) in map.drain() {
            entry.timer.abort();
        }
        count
    }

    pub(crate) fn take_revoked(
        &self,
        user_id: &str,
        minimum_version: i64,
    ) -> Vec<(String, String, mpsc::Sender<crate::OutboundJson>)> {
        let mut map = self
            .inner
            .write()
            .unwrap_or_else(|error| error.into_inner());
        let mut removed = Vec::new();
        map.retain(|(room_id, participant_id, _), entry| {
            if entry.authenticated_subject.as_deref() == Some(user_id)
                && entry
                    .authenticated_version
                    .is_none_or(|version| version < minimum_version)
            {
                entry.timer.abort();
                removed.push((
                    room_id.clone(),
                    participant_id.clone(),
                    entry.sender.clone(),
                ));
                false
            } else {
                true
            }
        });
        removed
    }

    /// Retain a disconnected session without evicting another reconnectable
    /// participant. On capacity exhaustion the new timer is aborted and the
    /// caller must immediately remove that participant's room/media state.
    fn insert(&self, room_id: String, participant_id: String, entry: GraceEntry) -> bool {
        let mut map = self.inner.write().unwrap_or_else(|e| e.into_inner());
        let key = (room_id, participant_id, entry.reconnect_token.clone());
        if self.closed.load(Ordering::Acquire)
            || (map.len() >= self.max_entries && !map.contains_key(&key))
        {
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

    /// A retained timer may start only after its matching map entry exists.
    fn insert_activated(
        &self,
        room_id: String,
        participant_id: String,
        entry: GraceEntry,
        activation: tokio::sync::oneshot::Sender<()>,
    ) -> bool {
        let retained = self.insert(room_id, participant_id, entry);
        if retained {
            let _ = activation.send(());
        }
        retained
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

/// Serialize a message or reply envelope into the bounded outbound queue.
fn send_json(
    metrics: &ServerMetrics,
    sender: &mpsc::Sender<crate::OutboundJson>,
    msg: &impl serde::Serialize,
) -> anyhow::Result<()> {
    let json = crate::OutboundJson::from(serde_json::to_string(msg)?);
    if let Err(error) = sender.try_send(json) {
        match error {
            mpsc::error::TrySendError::Full(_) => metrics.inc_outbound_queue_full(),
            mpsc::error::TrySendError::Closed(_) => metrics.inc_outbound_queue_closed(),
        }
        anyhow::bail!("Outbound signaling queue unavailable");
    }
    Ok(())
}

/// The request ID belongs only to direct replies, never to room notifications.
struct ReplySender<'a> {
    metrics: &'a ServerMetrics,
    sender: &'a mpsc::Sender<crate::OutboundJson>,
    request_id: Option<&'a str>,
}

impl ReplySender<'_> {
    /// Legacy closure/layer commands have no reply. Modern clients explicitly
    /// request acknowledgement so a lost socket cannot hide an unapplied control.
    fn acknowledge_control(&self) -> anyhow::Result<()> {
        if self.request_id.is_some() {
            self.send(&ServerMessage::MediaControlApplied)?;
        }
        Ok(())
    }

    fn acknowledge_room_control(&self) -> anyhow::Result<()> {
        if self.request_id.is_some() {
            self.send(&ServerMessage::RoomControlApplied)?;
        }
        Ok(())
    }

    fn send(&self, message: &ServerMessage) -> anyhow::Result<()> {
        // These protocols already serialize their own IDs. Do not emit a
        // duplicate JSON key or replace their independent correlation state.
        let request_id = match message {
            ServerMessage::AuthenticationRenewed { .. }
            | ServerMessage::AuthenticationRenewalFailed { .. }
            | ServerMessage::AuthenticationRenewalDeferred { .. }
            | ServerMessage::SocialResponse { .. }
            | ServerMessage::SocialError { .. } => None,
            _ => self.request_id,
        };
        send_json(
            self.metrics,
            self.sender,
            &ServerReply {
                message,
                request_id,
            },
        )
    }
}

/// Only completed text writes increment sent; queue acceptance is not delivery.
/// Cancellation remains a diagnostic outcome, not a completed send failure.
async fn write_message<S: Sink<Message> + Unpin>(
    sink: &mut S,
    message: Message,
    metrics: &ServerMetrics,
    connection_id: Option<u64>,
    deadline: tokio::time::Instant,
    kind: OperationKind,
) -> Result<(), Outcome> {
    let is_application_text = matches!(&message, Message::Text(_));
    let operation = metrics.diagnostics().operation(kind, connection_id);
    let result = operation
        .scope(diagnostics::measure_result(Stage::SocketWrite, async {
            match tokio::time::timeout_at(deadline, sink.send(message)).await {
                Ok(Ok(())) => Ok(()),
                Ok(Err(_)) => Err(Outcome::Error),
                Err(_) => Err(Outcome::Timeout),
            }
        }))
        .await;
    let outcome = match result {
        Ok(()) => {
            if is_application_text {
                metrics.inc_messages_sent();
            }
            Outcome::Ok
        }
        Err(outcome) => {
            if is_application_text {
                metrics.inc_message_send_failed();
            }
            outcome
        }
    };
    operation.finish(outcome);
    result
}

/// Operation names come from protocol variants, never message data. Social and
/// administrative requests are deliberately grouped until they gain stage probes.
fn diagnostic_operation(message: &ClientMessage) -> OperationKind {
    match message {
        ClientMessage::JoinRoom { .. } => OperationKind::JoinRoom,
        ClientMessage::LeaveRoom => OperationKind::LeaveRoom,
        ClientMessage::Reconnect { .. } => OperationKind::Reconnect,
        ClientMessage::RenewAuthentication { .. } => OperationKind::RoomAction,
        ClientMessage::GetRouterRtpCapabilities => OperationKind::RouterCapabilities,
        ClientMessage::CreateSendTransport => OperationKind::CreateSendTransport,
        ClientMessage::CreateRecvTransport => OperationKind::CreateRecvTransport,
        ClientMessage::ConnectTransport { .. } => OperationKind::ConnectTransport,
        ClientMessage::Produce { .. } => OperationKind::Produce,
        ClientMessage::Consume { .. } => OperationKind::Consume,
        ClientMessage::ResumeConsumer { .. } => OperationKind::ResumeConsumer,
        ClientMessage::PauseConsumer { .. } => OperationKind::PauseConsumer,
        ClientMessage::CloseConsumer { .. } => OperationKind::CloseConsumer,
        ClientMessage::PauseProducer { .. } => OperationKind::PauseProducer,
        ClientMessage::ResumeProducer { .. } => OperationKind::ResumeProducer,
        ClientMessage::CloseProducer { .. } => OperationKind::CloseProducer,
        ClientMessage::RestartIce { .. } => OperationKind::RestartIce,
        ClientMessage::SetConsumerPreferredLayers { .. } => OperationKind::SetPreferredLayers,
        ClientMessage::ChatMessage { .. } => OperationKind::Chat,
        ClientMessage::PrivateMessage { .. } => OperationKind::PrivateMessage,
        ClientMessage::RetryChatMessage(retry) => {
            if retry.target_participant_id.is_some() {
                OperationKind::PrivateMessage
            } else {
                OperationKind::Chat
            }
        }
        ClientMessage::SetChatPreferences { .. }
        | ClientMessage::ChangeNickname { .. }
        | ClientMessage::GetRoomSnapshot { .. }
        | ClientMessage::ListRoomBans { .. }
        | ClientMessage::RemoveRoomBan { .. }
        | ClientMessage::ListRoomMembers { .. }
        | ClientMessage::SetMemberRole { .. }
        | ClientMessage::ReportParticipant { .. }
        | ClientMessage::ListRoomReports { .. }
        | ClientMessage::ResolveRoomReport { .. }
        | ClientMessage::CloseCam { .. }
        | ClientMessage::CamBan { .. }
        | ClientMessage::CamUnban { .. }
        | ClientMessage::TextMute { .. }
        | ClientMessage::TextUnmute { .. }
        | ClientMessage::Kick { .. }
        | ClientMessage::Ban { .. }
        | ClientMessage::Unban { .. }
        | ClientMessage::SetRole { .. }
        | ClientMessage::RequestVoice
        | ClientMessage::UpdateRoomSettings { .. }
        | ClientMessage::SetTopic { .. }
        | ClientMessage::AdmitFromLobby { .. }
        | ClientMessage::DenyFromLobby { .. } => OperationKind::RoomAction,
    }
}

fn begin_join_session(reconnect_token: &mut String) -> String {
    let fresh = Uuid::new_v4().to_string();
    *reconnect_token = fresh.clone();
    fresh
}

fn reconnect_attempt_allowed(current_room_id: Option<&str>, in_lobby: bool) -> bool {
    current_room_id.is_none() && !in_lobby
}

/// Convert accepted wall-clock expiry once, retaining fractional-second precision.
fn credential_expiry_deadline(exp: u64) -> Instant {
    let now = Instant::now();
    let remaining = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(Duration::ZERO, |elapsed| {
            Duration::from_secs(exp).saturating_sub(elapsed)
        });
    now.checked_add(remaining).unwrap_or(now)
}

/// Revalidation waits are bounded independently of PostgreSQL's statement limit.
/// Revocation notices and drain remain observable while the query is pending.
/// `None` means drain, not invalid credentials. The caller supplies its original
/// accepted expiry (and, for a retained session, its original grace deadline).
async fn revalidate_account(
    pool: Option<&sqlx::PgPool>,
    claims: &Claims,
    continuity: &mut CredentialContinuity,
    revocations: &mut tokio::sync::broadcast::Receiver<(String, i64)>,
    drain: &crate::shutdown::DrainSignal,
    hard_deadline: Instant,
) -> Option<bool> {
    let now = Instant::now();
    if continuity.expired(now) || now >= hard_deadline || claims.exp as u64 <= unix_seconds() {
        return Some(false);
    }
    let deadline = continuity
        .deadline()
        .map_or(hard_deadline, |limit| limit.min(hard_deadline))
        .min(now + AUTH_REVALIDATE_INTERVAL);
    let validation = account_credentials_current(pool, claims, deadline);
    observe_account_validation(
        validation,
        claims,
        continuity,
        revocations,
        drain,
        hard_deadline,
    )
    .await
}

/// Query budget for one validation attempt, or `None` when the accepted
/// credentials are already invalid before any query is issued.
fn validation_deadline(
    claims: &Claims,
    continuity: &CredentialContinuity,
    hard_deadline: Instant,
) -> Option<Instant> {
    let now = Instant::now();
    if continuity.expired(now) || now >= hard_deadline || claims.exp as u64 <= unix_seconds() {
        return None;
    }
    Some(
        continuity
            .deadline()
            .map_or(hard_deadline, |limit| limit.min(hard_deadline))
            .min(now + AUTH_REVALIDATE_INTERVAL),
    )
}

/// Applies one completed validation. `None` means drain, not invalid
/// credentials. Shared by the awaited form below and the connection loop's
/// pending-validation arm so both apply identical policy.
fn settle_account_validation(
    result: CredentialStatus,
    claims: &Claims,
    continuity: &mut CredentialContinuity,
    revocations: &mut tokio::sync::broadcast::Receiver<(String, i64)>,
    drain: &crate::shutdown::DrainSignal,
    hard_deadline: Instant,
    deadline: Instant,
) -> Option<bool> {
    let now = Instant::now();
    if drain.is_draining() {
        return None;
    }
    if now >= hard_deadline || claims.exp as u64 <= unix_seconds() {
        return Some(false);
    }
    let result = if !authentication::revocations_current(revocations, claims) {
        CredentialStatus::Revoked
    } else if result == CredentialStatus::Current && now >= deadline {
        CredentialStatus::Unavailable
    } else {
        result
    };
    Some(retain_credentials(continuity, result, now))
}

/// Keep control-plane races testable without modifying a database or accepting
/// injected validators at the public connection boundary.
async fn observe_account_validation(
    validation: impl std::future::Future<Output = CredentialStatus>,
    claims: &Claims,
    continuity: &mut CredentialContinuity,
    revocations: &mut tokio::sync::broadcast::Receiver<(String, i64)>,
    drain: &crate::shutdown::DrainSignal,
    hard_deadline: Instant,
) -> Option<bool> {
    let Some(deadline) = validation_deadline(claims, continuity, hard_deadline) else {
        return Some(false);
    };
    tokio::pin!(validation);
    let result = loop {
        tokio::select! {
            biased;
            _ = drain.wait() => return None,
            _ = tokio::time::sleep_until(deadline.into()) => break CredentialStatus::Unavailable,
            notice = revocations.recv() => match notice {
                Ok((subject, version)) if subject == claims.sub && claims.auth_version < version => break CredentialStatus::Revoked,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break CredentialStatus::Revoked,
                // The in-flight query may predate a lost revocation. It cannot
                // satisfy a fresh-validation requirement after notification loss.
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => break CredentialStatus::Revoked,
                _ => {},
            },
            result = &mut validation => break result,
        }
    };
    settle_account_validation(
        result,
        claims,
        continuity,
        revocations,
        drain,
        hard_deadline,
        deadline,
    )
}

/// Log only state transitions, never per-check credentials or database errors.
fn retain_credentials(
    continuity: &mut CredentialContinuity,
    result: CredentialStatus,
    now: Instant,
) -> bool {
    let was_unavailable = continuity.unavailable();
    let accepted = continuity.observe(result, now);
    if !was_unavailable && continuity.unavailable() && accepted {
        warn!(
            event = "authentication_validation_unavailable",
            "Retaining established credentials within the bounded uncertainty allowance"
        );
    } else if was_unavailable && result == CredentialStatus::Current && accepted {
        info!(
            event = "authentication_validation_restored",
            "Account validation recovered before its deadline"
        );
    }
    accepted
}

/// Handles a single WebSocket connection
#[expect(
    clippy::too_many_arguments,
    reason = "connection boundary keeps owned session resources explicit"
)]
pub async fn handle_connection(
    socket: WebSocket,
    room_manager: Arc<RoomManager>,
    turn_config: Option<Arc<TurnConfig>>,
    grace_periods: GracePeriodMap,
    metrics: ServerMetrics,
    _permit: OwnedSemaphorePermit,
    authenticated_user: Option<Claims>,
    client_ip: Option<std::net::IpAddr>,
    db_pool: Option<sqlx::PgPool>,
    auth_revocations: tokio::sync::broadcast::Receiver<(String, i64)>,
    renewal_authenticator: Option<RenewalAuthenticator>,
) {
    handle_connection_with_timing(
        socket,
        room_manager,
        turn_config,
        grace_periods,
        metrics,
        _permit,
        authenticated_user,
        client_ip,
        db_pool,
        auth_revocations,
        renewal_authenticator,
        ConnectionTiming::default(),
    )
    .await;
}

#[expect(
    clippy::too_many_arguments,
    reason = "connection boundary retains explicit resources and an internal test timing policy"
)]
async fn handle_connection_with_timing(
    socket: WebSocket,
    room_manager: Arc<RoomManager>,
    turn_config: Option<Arc<TurnConfig>>,
    grace_periods: GracePeriodMap,
    metrics: ServerMetrics,
    _permit: OwnedSemaphorePermit,
    mut authenticated_user: Option<Claims>,
    client_ip: Option<std::net::IpAddr>,
    db_pool: Option<sqlx::PgPool>,
    mut auth_revocations: tokio::sync::broadcast::Receiver<(String, i64)>,
    renewal_authenticator: Option<RenewalAuthenticator>,
    timing: ConnectionTiming,
) {
    // Use authenticated user ID if available, otherwise generate anonymous UUID
    let mut participant_id = authenticated_user
        .as_ref()
        .map(|c| c.sub.clone())
        .unwrap_or_else(|| Uuid::new_v4().to_string());

    let is_authenticated = authenticated_user.is_some();
    let authenticated_display_name = authenticated_user.as_ref().map(|c| c.name.clone());
    let mut auth_exp = authenticated_user.as_ref().map(|claims| claims.exp as u64);
    let mut auth_deadline = auth_exp.map(credential_expiry_deadline);

    let diagnostic_connection_id = Some(metrics.next_connection_id());
    info!(
        connection_id = diagnostic_connection_id,
        authenticated = is_authenticated,
        "New WebSocket connection"
    );

    metrics.inc_connections_total();
    let _conn_guard = metrics.connection_active_guard();

    // Generate reconnect token for this session
    let mut reconnect_token = Uuid::new_v4().to_string();

    let (mut ws_sender, mut ws_receiver) = socket.split();

    // Bounded channel for sending messages to this client
    let (tx, mut rx) = mpsc::channel::<crate::OutboundJson>(CHANNEL_CAPACITY);

    // Clone for the send task
    let send_metrics = metrics.clone();
    let drain = room_manager.drain_signal();
    let writer_drain = drain.clone();
    // Notify retains at most one pending permit: a slow writer cannot build an
    // unbounded queue of stale heartbeat requests. Only this writer owns the
    // sink, for both application text and protocol control frames.
    let heartbeat = Arc::new(Notify::new());
    let writer_heartbeat = heartbeat.clone();

    // Drain interrupts both an idle writer and a slow ordinary send, independent
    // of a reader currently awaiting database/media work. Notification is best
    // effort: a non-reading or already disconnected peer cannot delay shutdown.
    let mut send_task = OwnedTask(tokio::spawn(async move {
        tokio::select! {
            biased;
            _ = writer_drain.wait() => {},
            _ = async {
                loop {
                    let message = tokio::select! {
                        biased;
                        _ = writer_heartbeat.notified() => Message::Ping(bytes::Bytes::new()),
                        json = rx.recv() => match json {
                            Some(json) => Message::Text(json),
                            None => break,
                        },
                    };
                    match write_message(
                        &mut ws_sender, message,
                        &send_metrics, diagnostic_connection_id, tokio::time::Instant::now() + SEND_TIMEOUT, OperationKind::SocketWrite,
                    ).await {
                        Ok(()) => {},
                        Err(Outcome::Timeout) => { warn!(connection_id = diagnostic_connection_id, "WebSocket send timed out"); break; },
                        Err(_) => { debug!(connection_id = diagnostic_connection_id, "WebSocket send failed"); break; },
                    }
                }
            } => {},
        }
        if writer_drain.is_draining() {
            let deadline = tokio::time::Instant::now() + DRAIN_SEND_TIMEOUT;
            let close = async {
                let json = serde_json::to_string(&ServerMessage::ServerRestarting {
                    reason: "Server shutting down".to_string(),
                })?;
                write_message(
                    &mut ws_sender,
                    Message::Text(json.into()),
                    &send_metrics,
                    diagnostic_connection_id,
                    deadline,
                    OperationKind::ShutdownNotification,
                )
                .await
                .map_err(|_| anyhow::anyhow!("Shutdown notification write failed"))?;
                tokio::time::timeout_at(
                    deadline,
                    ws_sender.send(Message::Close(Some(CloseFrame {
                        code: close_code::AWAY,
                        reason: "Server shutting down".into(),
                    }))),
                )
                .await??;
                Ok::<(), anyhow::Error>(())
            };
            if close.await.is_err() {
                debug!(
                    connection_id = diagnostic_connection_id,
                    "Shutdown socket notification could not be delivered"
                );
            }
        }
        debug!(
            connection_id = diagnostic_connection_id,
            "Send task finished"
        );
    }));

    // Handle incoming messages
    let mut current_room_id: Option<String> = None;
    // True while the participant waits in a lobby. Shared (Arc) because admission
    // happens on the moderator's connection task — admit_from_lobby clears it via
    // the LobbyEntry so this task's guard opens without any local event.
    let in_lobby = Arc::new(AtomicBool::new(false));
    let mut stats_task: Option<OwnedTask> = None;
    let mut bwe_sender: Option<mpsc::Sender<u32>> = None;

    // Token bucket rate limiter state
    let max_frame_tokens_us = frame_capacity(consumer_mutation_capacity(
        crate::media::transport_manager::max_consumers_per_participant(),
    ));
    let mut frame_tokens_us = max_frame_tokens_us;
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
    let mut join_attempts = JoinAttemptState::default();
    let mut last_voice_request: Option<Instant> = None;
    let mut credentials_invalidated = false;
    let mut credential_continuity = CredentialContinuity::default();
    let mut next_auth_check = Instant::now();
    // The periodic account check is polled as its own arm below, so a slow
    // database delays only the verdict, never this socket's frame dispatch.
    let mut pending_validation: Option<
        std::pin::Pin<Box<dyn std::future::Future<Output = CredentialStatus> + Send>>,
    > = None;
    let mut pending_validation_deadline = Instant::now();
    let mut last_frame_received = Instant::now();
    let mut next_heartbeat = Instant::now() + timing.heartbeat_interval;
    let mut peer_close_received = false;
    let mut renewal_budget = RenewalBudget::new();

    loop {
        // The JWT is a connection credential, not only a handshake credential.
        // Cap every receive wait by its absolute expiry so active traffic cannot
        // keep an authenticated socket alive indefinitely.
        let now_unix = unix_seconds();
        if auth_exp.is_some_and(|exp| exp <= now_unix)
            || auth_deadline.is_some_and(|deadline| Instant::now() >= deadline)
            || credential_continuity.expired(Instant::now())
        {
            info!(
                participant_id,
                "Accepted authentication lifetime or validation allowance expired; closing WebSocket"
            );
            credentials_invalidated = true;
            break;
        }
        let idle_remaining = timing
            .idle_timeout
            .saturating_sub(last_frame_received.elapsed());
        if idle_remaining.is_zero() {
            warn!("Idle timeout for participant {}", participant_id);
            break;
        }
        let receive_timeout = auth_exp
            .map(|exp| Duration::from_secs(exp.saturating_sub(now_unix)))
            .map_or(idle_remaining, |remaining| remaining.min(idle_remaining));
        let receive_deadline = Instant::now() + receive_timeout;
        let receive_deadline =
            auth_deadline.map_or(receive_deadline, |deadline| deadline.min(receive_deadline));
        let receive_deadline = credential_continuity
            .deadline()
            .map_or(receive_deadline, |deadline| deadline.min(receive_deadline));
        let receive_deadline = tokio::time::Instant::from_std(receive_deadline);

        let receive_result = tokio::select! {
            biased;
            _ = drain.wait() => break,
            _ = tx.closed() => break,
            notice = auth_revocations.recv(), if is_authenticated => {
                match notice {
                    Ok((subject, minimum_version)) if authenticated_user.as_ref().is_some_and(|claims| claims.sub == subject && claims.auth_version < minimum_version) => {
                        credentials_invalidated = true;
                        break;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        // An in-flight query predates the lost notices and
                        // cannot satisfy the fresh-validation requirement.
                        pending_validation = None;
                        credential_continuity.require_revalidation();
                        next_auth_check = Instant::now();
                    },
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => { credentials_invalidated = true; break; }
                    _ => {}
                }
                continue;
            }
            _ = tokio::time::sleep_until(next_auth_check.into()), if is_authenticated && pending_validation.is_none() => {
                if let Some(claims) = authenticated_user.as_ref() {
                    let hard_deadline = auth_deadline.unwrap_or_else(Instant::now);
                    match validation_deadline(claims, &credential_continuity, hard_deadline) {
                        Some(deadline) => {
                            pending_validation_deadline = deadline;
                            pending_validation = Some(Box::pin(account_credentials_current_owned(
                                db_pool.clone(),
                                claims.clone(),
                                deadline,
                            )));
                        }
                        None => { credentials_invalidated = true; break; }
                    }
                }
                continue;
            }
            result = async { pending_validation.as_mut().expect("guarded by is_some").await }, if pending_validation.is_some() => {
                pending_validation = None;
                if let Some(claims) = authenticated_user.as_ref() {
                    match settle_account_validation(result, claims, &mut credential_continuity,
                        &mut auth_revocations, &drain, auth_deadline.unwrap_or_else(Instant::now),
                        pending_validation_deadline) {
                        Some(true) => {},
                        Some(false) => { credentials_invalidated = true; break; },
                        None => break,
                    }
                }
                next_auth_check = Instant::now() + AUTH_REVALIDATE_INTERVAL;
                continue;
            }
            result = tokio::time::timeout_at(receive_deadline, ws_receiver.next()) => result,
            _ = tokio::time::sleep_until(next_heartbeat.into()), if current_room_id.is_some() => {
                // Schedule from now, not a missed-tick backlog. A membership
                // lookup never renews liveness or delays drain; only a received
                // frame (normally the protocol Pong) renews the idle deadline.
                next_heartbeat = Instant::now() + timing.heartbeat_interval;
                if last_frame_received.elapsed() >= timing.heartbeat_interval
                    && let Some(room_id) = current_room_id.as_deref()
                {
                    // The timer may have consumed most of the receive budget.
                    // Preserve its absolute deadline instead of reusing the
                    // relative timeout calculated before that wait.
                    let mut lookup_deadline = (tokio::time::Instant::now()
                        + timing.membership_timeout).min(receive_deadline);
                    if is_authenticated {
                        lookup_deadline = lookup_deadline.min(next_auth_check.into());
                    }
                    let bound = tokio::select! {
                        biased;
                        _ = drain.wait() => break,
                        _ = tx.closed() => break,
                        notice = auth_revocations.recv(), if is_authenticated => {
                            match notice {
                                Ok((subject, minimum_version)) if authenticated_user.as_ref().is_some_and(|claims| claims.sub == subject && claims.auth_version < minimum_version) => {
                                    credentials_invalidated = true;
                                    break;
                                }
                                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                                    credential_continuity.require_revalidation();
                                    next_auth_check = Instant::now();
                                },
                                Err(tokio::sync::broadcast::error::RecvError::Closed) => { credentials_invalidated = true; break; }
                                _ => {}
                            }
                            continue;
                        }
                        result = tokio::time::timeout_at(
                            lookup_deadline,
                            room_manager.is_bound_participant(room_id, &participant_id, &tx),
                        ) => matches!(result, Ok(true)),
                    };
                    // Local room state can be stale after a kick, deletion, or
                    // sender rebind. The authoritative check also admits lobby
                    // members; it never creates membership or bypasses policy.
                    if bound && tokio::time::Instant::now() < lookup_deadline {
                        heartbeat.notify_one();
                    }
                }
                continue;
            }
        };
        let msg = match receive_result {
            Ok(Some(Ok(message))) => message,
            Ok(Some(Err(_))) | Ok(None) => break, // Stream error or closed
            Err(_) => {
                if auth_exp.is_some_and(|exp| exp <= unix_seconds())
                    || auth_deadline.is_some_and(|deadline| Instant::now() >= deadline)
                    || credential_continuity.expired(Instant::now())
                {
                    info!(
                        participant_id,
                        "Accepted authentication lifetime or validation allowance expired; closing WebSocket"
                    );
                    credentials_invalidated = true;
                } else {
                    warn!("Idle timeout for participant {}", participant_id);
                }
                break;
            }
        };
        // A ready frame can win against timeout_at at the exact deadline.
        // Never dispatch it using an expired accepted credential or allowance.
        if auth_exp.is_some_and(|exp| exp <= unix_seconds())
            || auth_deadline.is_some_and(|deadline| Instant::now() >= deadline)
            || credential_continuity.expired(Instant::now())
        {
            credentials_invalidated = true;
            break;
        }
        last_frame_received = Instant::now();
        // Record this before the rate gate, whose Close shortcut also exits.
        if matches!(&msg, Message::Close(_)) {
            peer_close_received = true;
        }

        // Every inbound frame consumes rate-limit capacity. Limiting only text
        // frames lets binary/control-frame floods bypass connection accounting.
        metrics.inc_messages_received();
        let now = Instant::now();
        if !consume_rate_token(
            &mut frame_tokens_us,
            &mut frame_last_refill,
            now,
            RATE_LIMIT_REFILL_RATE,
            max_frame_tokens_us,
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
                if metrics
                    .telemetry()
                    .warning_should_log(super::telemetry::WarningFamily::FrameRate)
                {
                    warn!(
                        connection_id = diagnostic_connection_id,
                        "WebSocket frame rate limit exceeded; further instances counted in metrics"
                    );
                }
                // Inspect at most one bounded rejected frame per violation
                // window so overload handling does not decode every payload.
                let request_id = match &msg {
                    Message::Text(text) if text.len() <= MAX_SIGNAL_MESSAGE_LEN => {
                        serde_json::from_str::<RequestHeader>(text)
                            .ok()
                            .and_then(|header| header.request_id)
                    }
                    _ => None,
                };
                let reply = ReplySender {
                    metrics: &metrics,
                    sender: &tx,
                    request_id: request_id.as_deref(),
                };
                let _ = reply.send(&ServerMessage::Error {
                    message: format!(
                        "Rate limit exceeded: max {} frames/second",
                        RATE_LIMIT_REFILL_RATE
                    ),
                });
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

                let header = match serde_json::from_str::<RequestHeader>(&text) {
                    Ok(header) => header,
                    Err(_) => {
                        metrics.inc_errors();
                        let _ = send_json(
                            &metrics,
                            &tx,
                            &ServerMessage::Error {
                                message: "Invalid message format".to_string(),
                            },
                        );
                        continue;
                    }
                };
                let reply = ReplySender {
                    metrics: &metrics,
                    sender: &tx,
                    request_id: header.request_id.as_deref(),
                };
                match serde_json::from_str::<ClientMessage>(&text) {
                    Ok(client_msg) => {
                        // Renewal belongs to this socket, not to a room or media
                        // session. Its dedicated response cannot consume a pending
                        // room operation's generic error response in the browser.
                        if let ClientMessage::RenewAuthentication { request_id, token } =
                            &client_msg
                        {
                            let outcome = if renewal_budget.allow(Instant::now()) {
                                match (renewal_authenticator.as_ref(), authenticated_user.as_ref())
                                {
                                    (Some(authenticator), Some(current)) => {
                                        let renewal = renew_authentication(
                                            authenticator,
                                            current,
                                            token,
                                            db_pool.as_ref(),
                                            &mut auth_revocations,
                                            &drain,
                                            &tx,
                                        );
                                        let deadline = auth_deadline.unwrap_or_else(Instant::now);
                                        let deadline = credential_continuity
                                            .deadline()
                                            .map_or(deadline, |limit| limit.min(deadline));
                                        let outcome =
                                            tokio::time::timeout_at(deadline.into(), renewal)
                                                .await
                                                .unwrap_or(RenewalOutcome::Close);
                                        if Instant::now() >= deadline {
                                            RenewalOutcome::Close
                                        } else {
                                            outcome
                                        }
                                    }
                                    _ => RenewalOutcome::Rejected,
                                }
                            } else {
                                RenewalOutcome::Rejected
                            };
                            let response = match outcome {
                                RenewalOutcome::Renewed(claims) => {
                                    if !retain_credentials(
                                        &mut credential_continuity,
                                        CredentialStatus::Current,
                                        Instant::now(),
                                    ) {
                                        credentials_invalidated = true;
                                        break;
                                    }
                                    let expires_at = claims.exp as u64;
                                    auth_exp = Some(expires_at);
                                    auth_deadline = Some(credential_expiry_deadline(expires_at));
                                    authenticated_user = Some(claims);
                                    next_auth_check = Instant::now() + AUTH_REVALIDATE_INTERVAL;
                                    ServerMessage::AuthenticationRenewed {
                                        request_id: request_id.clone(),
                                        expires_at,
                                    }
                                }
                                RenewalOutcome::Rejected => {
                                    ServerMessage::AuthenticationRenewalFailed {
                                        request_id: request_id.clone(),
                                    }
                                }
                                RenewalOutcome::Unavailable | RenewalOutcome::Busy => {
                                    if matches!(outcome, RenewalOutcome::Unavailable)
                                        && !retain_credentials(
                                            &mut credential_continuity,
                                            CredentialStatus::Unavailable,
                                            Instant::now(),
                                        )
                                    {
                                        credentials_invalidated = true;
                                        break;
                                    }
                                    ServerMessage::AuthenticationRenewalDeferred {
                                        request_id: request_id.clone(),
                                        retry_after_ms: 3000,
                                        expires_at: auth_exp.unwrap_or_default(),
                                    }
                                }
                                RenewalOutcome::Close => {
                                    credentials_invalidated = true;
                                    break;
                                }
                                RenewalOutcome::Interrupted => break,
                            };
                            if send_json(&metrics, &tx, &response).is_err() {
                                break;
                            }
                            continue;
                        }
                        let now = Instant::now();
                        let media_limit_exceeded =
                            media_rate_state.limit_exceeded(&client_msg, now);
                        if media_limit_exceeded {
                            let media_mutation_violations = media_rate_state.record_violation(now);
                            let _ = reply.send(&ServerMessage::Error {
                                message: "Media changes are rate limited".to_string(),
                            });
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
                            let _ = reply.send(
                                &client_msg
                                    .social_error("Room administration changes are rate limited")
                                    .unwrap_or(ServerMessage::Error {
                                        message: "Room administration changes are rate limited"
                                            .to_string(),
                                    }),
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

                        if matches!(
                            &client_msg,
                            ClientMessage::ChatMessage { .. }
                                | ClientMessage::PrivateMessage { .. }
                                | ClientMessage::RetryChatMessage(_)
                        ) && !consume_rate_token(
                            &mut chat_tokens_us,
                            &mut chat_last_refill,
                            Instant::now(),
                            CHAT_RATE_LIMIT_REFILL_RATE,
                            MAX_CHAT_TOKENS_US,
                        ) {
                            if metrics
                                .telemetry()
                                .warning_should_log(super::telemetry::WarningFamily::ChatRate)
                            {
                                warn!(
                                    connection_id = diagnostic_connection_id,
                                    "Closing WebSocket for chat flooding; further instances counted in metrics"
                                );
                            }
                            let _ = reply.send(
                                &client_msg
                                    .social_error("Chat rate limit exceeded")
                                    .unwrap_or(ServerMessage::Error {
                                        message: "Chat rate limit exceeded".to_string(),
                                    }),
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
                                let _ = reply.send(&ServerMessage::Error {
                                    message: "Room password updates are rate limited".to_string(),
                                });
                                continue;
                            }
                            last_room_password_hash = Some(now);
                        }

                        if let ClientMessage::JoinRoom {
                            room_id, password, ..
                        } = &client_msg
                            && !join_attempts.allow(room_id, password.is_some(), Instant::now())
                        {
                            let _ = reply.send(&ServerMessage::Error {
                                message: "Join attempts are rate limited".to_string(),
                            });
                            continue;
                        }

                        if matches!(&client_msg, ClientMessage::RequestVoice) {
                            let now = Instant::now();
                            if last_voice_request.is_some_and(|previous| {
                                now.duration_since(previous) < VOICE_REQUEST_COOLDOWN
                            }) {
                                let _ = reply.send(&ServerMessage::Error {
                                    message: "Voice requests are rate limited".to_string(),
                                });
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
                                let _ = reply.send(&ServerMessage::ReconnectResult {
                                    success: false,
                                    participant_id: reconnect_id.clone(),
                                    reconnect_token: None,
                                });
                                continue;
                            }

                            let reconnect_started = Instant::now();
                            let operation = metrics
                                .diagnostics()
                                .operation(OperationKind::Reconnect, diagnostic_connection_id);
                            let restored_media_rate_state = operation
                                .scope(diagnostics::measure(
                                    Stage::Dispatch,
                                    handle_reconnect(
                                        reconnect_id,
                                        reconnect_room,
                                        client_token,
                                        authenticated_user
                                            .as_ref()
                                            .map(|claims| claims.sub.as_str()),
                                        &grace_periods,
                                        &room_manager,
                                        &tx,
                                    ),
                                ))
                                .await;
                            operation.finish(if restored_media_rate_state.is_some() {
                                Outcome::Ok
                            } else {
                                Outcome::Rejected
                            });

                            let success = restored_media_rate_state.is_some();
                            if success {
                                metrics.inc_reconnect();
                            }
                            metrics.telemetry().record_signaling(
                                OperationKind::Reconnect,
                                success,
                                reconnect_started.elapsed(),
                            );
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

                            let response_sent = reply.send(&ServerMessage::ReconnectResult {
                                success,
                                participant_id: if success {
                                    participant_id.clone()
                                } else {
                                    reconnect_id.clone()
                                },
                                reconnect_token: fresh_reconnect_token.clone(),
                            });
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
                        let operation = metrics
                            .diagnostics()
                            .operation(diagnostic_operation(&client_msg), diagnostic_connection_id);
                        let result = operation
                            .scope(diagnostics::measure_result(
                                Stage::Dispatch,
                                handle_client_message(
                                    &client_msg,
                                    &participant_id,
                                    &mut current_room_id,
                                    &in_lobby,
                                    &reply,
                                    &room_manager,
                                    &turn_config,
                                    &mut reconnect_token,
                                    &bwe_sender,
                                    is_authenticated,
                                    authenticated_display_name.as_deref(),
                                    client_ip,
                                ),
                            ))
                            .instrument(tracing::info_span!("signaling_operation", connection_id = diagnostic_connection_id, operation = ?diagnostic_operation(&client_msg)))
                            .await;
                        operation.finish(match &result {
                            Ok(()) => Outcome::Ok,
                            Err(error) if error.is::<RoomPasswordRequired>() => Outcome::Rejected,
                            Err(_) => Outcome::Error,
                        });
                        metrics.observe_message_handling(start.elapsed());
                        metrics.telemetry().record_signaling(
                            diagnostic_operation(&client_msg),
                            result.is_ok(),
                            start.elapsed(),
                        );

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
                            if e.is::<RoomPasswordRequired>()
                                && let ClientMessage::JoinRoom { room_id, .. } = &client_msg
                            {
                                join_attempts.password_challenge_room = Some(room_id.clone());
                            }
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
                            let public_message = e
                                .downcast_ref::<crate::room::social::SocialFailure>()
                                .map_or("Request could not be completed", |error| error.0.as_str());
                            let response = client_msg
                                .social_error(public_message)
                                .unwrap_or_else(|| client_error_response(&e));
                            if reply.send(&response).is_err() {
                                break;
                            }
                        }

                        if is_join
                            && current_room_id.is_some()
                            && !in_lobby.load(Ordering::Acquire)
                            && let Some(task) = stats_task.take()
                        {
                            // Joined a new room directly — restart stats below
                            task.abort();
                        }

                        // Ensure a stats task runs whenever we are a full room member.
                        // Invariant-based (not join-triggered) so it also covers lobby
                        // admission: admit_from_lobby clears in_lobby, and the admitted
                        // client's first media-setup message lands here.
                        if let Some(room_id) = current_room_id.as_ref()
                            && !in_lobby.load(Ordering::Acquire)
                            && stats_task.is_none()
                        {
                            // Create BWE event channel (subscription happens later in CreateRecvTransport)
                            let (bwe_tx, bwe_rx) = mpsc::channel::<u32>(32);
                            bwe_sender = Some(bwe_tx);

                            stats_task = Some(spawn_stats_task(
                                room_manager.clone(),
                                room_id.clone(),
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
                        let _ = reply.send(&ServerMessage::Error {
                            message: "Invalid message format".to_string(),
                        });
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

    let disconnected_at = Instant::now();
    let disconnected_room_id = current_room_id.clone();
    // Stop stats task
    if let Some(task) = stats_task.take() {
        task.abort();
    }

    if peer_close_received {
        // Finish the transport before account validation can wait on the DB.
        // This does not change room-leave or retained reconnect semantics.
        let outcome = complete_peer_close(&mut ws_receiver, &mut send_task).await;
        debug!(
            connection_id = diagnostic_connection_id,
            ?outcome,
            "Peer WebSocket close finished"
        );
    }

    if !drain.is_draining()
        && !credentials_invalidated
        && let Some(claims) = authenticated_user.as_ref()
    {
        credentials_invalidated = revalidate_account(
            db_pool.as_ref(),
            claims,
            &mut credential_continuity,
            &mut auth_revocations,
            &drain,
            auth_deadline.unwrap_or_else(Instant::now),
        )
        .await
            == Some(false);
    }

    // On disconnect: lobby participants clean up immediately, room participants get grace period
    if let Some(room_id) = current_room_id.take() {
        if drain.is_draining() {
            // Global room drain clears both live and retained memberships. Do
            // not start a new grace timer or race its authoritative cleanup.
            debug!(room_id, participant_id, "Membership handed to server drain");
        } else if in_lobby.load(Ordering::Acquire) || credentials_invalidated {
            // Lobby and invalidated memberships do not receive reconnect grace.
            info!(
                "Participant {} disconnected from room {}; lobby or expired/invalid credentials require immediate cleanup",
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
            let timer_claims = authenticated_user.clone();
            let timer_pool = db_pool.clone();
            let mut timer_continuity = credential_continuity;
            let timer_drain = drain.clone();
            let grace_deadline = disconnected_at + Duration::from_secs(30);
            let grace_deadline =
                auth_deadline.map_or(grace_deadline, |expiry| expiry.min(grace_deadline));
            let (activate_timer, activation) = tokio::sync::oneshot::channel::<()>();

            let timer = tokio::spawn(async move {
                // Registration must precede even an already-expired timer's
                // cleanup. Failed insertion drops activation and aborts the task.
                if activation.await.is_err() {
                    return;
                }
                let mut next_check = Instant::now() + AUTH_REVALIDATE_INTERVAL;
                loop {
                    let deadline = timer_continuity
                        .deadline()
                        .map_or(grace_deadline, |limit| limit.min(grace_deadline));
                    tokio::select! {
                        biased;
                        _ = timer_drain.wait() => return,
                        _ = tokio::time::sleep_until(deadline.into()) => break,
                        _ = tokio::time::sleep_until(next_check.into()), if timer_claims.is_some() => {
                            if let Some(claims) = timer_claims.as_ref() {
                                match revalidate_account(timer_pool.as_ref(), claims, &mut timer_continuity,
                                    &mut auth_revocations, &timer_drain, grace_deadline).await {
                                    Some(true) => {},
                                    Some(false) => break,
                                    None => return,
                                }
                            }
                            next_check = Instant::now() + AUTH_REVALIDATE_INTERVAL;
                        },
                        notice = auth_revocations.recv(), if timer_claims.is_some() => {
                            match notice {
                                Ok((subject, version)) if timer_claims.as_ref().is_some_and(|claims|
                                    claims.sub == subject && claims.auth_version < version) => break,
                                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                                    timer_continuity.require_revalidation();
                                    next_check = Instant::now();
                                },
                                _ => {},
                            }
                        }
                    }
                }
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
                    if tracing::enabled!(target: "simplestChat::lifecycle", tracing::Level::DEBUG)
                        && let Ok(lifecycle_id) = Uuid::parse_str(&pid)
                    {
                        debug!(
                            target: "simplestChat::lifecycle",
                            event = "grace_cleanup_started",
                            participant_id = %lifecycle_id,
                            "lifecycle"
                        );
                    }
                    if let Err(e) = rm
                        .remove_participant_for_sender(&rid, &pid, &timer_sender)
                        .await
                    {
                        error!("Error removing participant after grace period: {}", e);
                    }
                }
            });

            let retained_for_reconnect = grace_periods.insert_activated(
                room_id.clone(),
                participant_id.clone(),
                GraceEntry {
                    reconnect_token,
                    authenticated_subject,
                    authenticated_version: authenticated_user
                        .as_ref()
                        .map(|claims| claims.auth_version),
                    media_rate_state,
                    sender: tx.clone(),
                    timer,
                },
                activate_timer,
            );
            if retained_for_reconnect
                && tracing::enabled!(target: "simplestChat::lifecycle", tracing::Level::DEBUG)
                && let Ok(lifecycle_id) = Uuid::parse_str(&participant_id)
            {
                debug!(
                    target: "simplestChat::lifecycle",
                    event = "grace_started",
                    participant_id = %lifecycle_id,
                    "lifecycle"
                );
            }
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

    if peer_close_received {
        // The peer-close path has already consumed the writer's join result.
    } else if drain.is_draining() {
        // Give the independent writer its bounded close attempt before releasing
        // the connection permit. The RAII owner aborts it on cancellation.
        if tokio::time::timeout(
            DRAIN_SEND_TIMEOUT + Duration::from_millis(100),
            &mut send_task.0,
        )
        .await
        .is_err()
        {
            warn!(
                participant_id,
                "Shutdown writer did not finish within its deadline"
            );
            send_task.abort();
        }
    } else {
        send_task.abort();
        let _ = (&mut send_task.0).await;
    }

    // Availability is best effort and may contend with a room mutation. Finish
    // the socket first, and never delay server drain for this notification.
    if !drain.is_draining()
        && let Some(room_id) = disconnected_room_id.as_deref()
    {
        let _ = room_manager
            .mark_participant_disconnected(room_id, &participant_id, &tx)
            .await;
    }
    drop(tx);

    info!(
        "Connection handler finished for participant: {}",
        participant_id
    );
}

#[cfg(test)]
#[path = "connection_close_tests.rs"]
mod close_tests;

#[cfg(test)]
#[path = "connection_heartbeat_tests.rs"]
mod heartbeat_tests;

#[cfg(test)]
#[path = "connection_correlation_tests.rs"]
mod correlation_tests;

#[cfg(test)]
#[path = "connection_credential_tests.rs"]
mod credential_tests;

/// Attempt to reconnect a participant to their existing session
async fn handle_reconnect(
    participant_id: &str,
    room_id: &str,
    client_token: &str,
    authenticated_subject: Option<&str>,
    grace_periods: &GracePeriodMap,
    room_manager: &Arc<RoomManager>,
    new_sender: &mpsc::Sender<crate::OutboundJson>,
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
/// - A tier change (debounced at 2 s) becomes the participant's bandwidth
///   ceiling; the room manager merges it with each viewer ceiling and only
///   asks the worker for consumers whose layers actually change
/// - ConnectionStats sent to client every ~10s using last-known bitrate (no IPC, skipped if unchanged)
fn spawn_stats_task(
    room_manager: Arc<RoomManager>,
    room_id: String,
    participant_id: String,
    sender: mpsc::Sender<crate::OutboundJson>,
    mut bwe_rx: mpsc::Receiver<u32>,
) -> OwnedTask {
    OwnedTask(tokio::spawn(async move {
        let drain = room_manager.drain_signal();
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
                biased;
                _ = drain.wait() => break,
                // BWE event: bandwidth estimation changed
                bwe = bwe_rx.recv() => {
                    match bwe {
                        Some(bitrate) => {
                            last_bitrate = bitrate;

                            // Keep these tiers: mediasoup steps layers down only as the
                            // estimate falls below each layer's measured bitrate, and
                            // removing them slowed downgrades from about 1 s to 19–31 s.
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
                                match room_manager
                                    .set_bandwidth_ceiling(&room_id, &participant_id, &sender, target_layer)
                                    .await
                                {
                                    Ok(written) => debug!(
                                        "Bandwidth tier {} for {}: {} consumer layer requests",
                                        target_layer, participant_id, written
                                    ),
                                    Err(e) => debug!(
                                        "Failed to apply bandwidth tier for {}: {}",
                                        participant_id, e
                                    ),
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
                            let _ = sender.try_send(crate::OutboundJson::from(json));
                        }
                    }
                }
            }
        }
    }))
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
#[expect(
    clippy::too_many_arguments,
    reason = "dispatcher borrows independently owned connection state"
)]
async fn handle_client_message(
    message: &ClientMessage,
    participant_id: &str,
    current_room_id: &mut Option<String>,
    in_lobby: &Arc<AtomicBool>,
    reply: &ReplySender<'_>,
    room_manager: &Arc<RoomManager>,
    turn_config: &Option<Arc<TurnConfig>>,
    reconnect_token: &mut String,
    bwe_sender: &Option<mpsc::Sender<u32>>,
    is_authenticated: bool,
    authenticated_display_name: Option<&str>,
    client_ip: Option<std::net::IpAddr>,
) -> anyhow::Result<()> {
    let sender = reply.sender;
    let metrics = reply.metrics;
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
    ) && let Some(room_id) = current_room_id.as_ref()
        && !room_manager
            .is_bound_participant(room_id, participant_id, sender)
            .await
    {
        anyhow::bail!("Participant is no longer in this room");
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

                    reply.send(&ServerMessage::RoomJoined {
                        participant_id: participant_id.to_string(),
                        participants,
                        reconnect_token: session_reconnect_token,
                        your_role: role,
                        room_settings,
                    })?;
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
                let lifecycle_id = if tracing::enabled!(target: "simplestChat::lifecycle", tracing::Level::DEBUG)
                {
                    Uuid::parse_str(participant_id).ok()
                } else {
                    None
                };
                if let Some(lifecycle_id) = lifecycle_id {
                    debug!(
                        target: "simplestChat::lifecycle",
                        event = "explicit_leave_started",
                        participant_id = %lifecycle_id,
                        "lifecycle"
                    );
                }
                room_manager
                    .remove_participant_for_sender(&room_id, participant_id, sender)
                    .await?;
                // Completion is not proof that membership was removed: this
                // sender-scoped call can return Ok(false) for a stale sender.
                // Media cleanup has separate markers for actual handle drops.
                if let Some(lifecycle_id) = lifecycle_id {
                    debug!(
                        target: "simplestChat::lifecycle",
                        event = "explicit_leave_finished",
                        participant_id = %lifecycle_id,
                        "lifecycle"
                    );
                }
                in_lobby.store(false, Ordering::Release);
                metrics.inc_leaves();
            }
        }

        ClientMessage::GetRouterRtpCapabilities => {
            if let Some(room_id) = current_room_id.as_ref() {
                let capabilities = room_manager
                    .get_router_rtp_capabilities(room_id, participant_id, sender)
                    .await?;
                reply.send(&ServerMessage::RouterRtpCapabilities {
                    rtp_capabilities: capabilities,
                })?;
            } else {
                anyhow::bail!("Not in a room");
            }
        }

        ClientMessage::CreateSendTransport => {
            if let Some(room_id) = current_room_id.as_ref() {
                let transport_info = room_manager
                    .create_send_transport(room_id, participant_id, sender)
                    .await?;

                reply.send(&ServerMessage::TransportCreated {
                    transport_id: transport_info.id,
                    ice_parameters: transport_info.ice_parameters,
                    ice_candidates: transport_info.ice_candidates,
                    dtls_parameters: transport_info.dtls_parameters,
                    ice_servers: make_ice_servers(turn_config),
                })?;
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
                if let Some(bwe_tx) = bwe_sender
                    && let Err(e) = room_manager
                        .subscribe_bwe_events(room_id, participant_id, sender, bwe_tx.clone())
                        .await
                {
                    debug!(
                        "Failed to subscribe BWE events for {}: {}",
                        participant_id, e
                    );
                }

                reply.send(&ServerMessage::TransportCreated {
                    transport_id: transport_info.id,
                    ice_parameters: transport_info.ice_parameters,
                    ice_candidates: transport_info.ice_candidates,
                    dtls_parameters: transport_info.dtls_parameters,
                    ice_servers: make_ice_servers(turn_config),
                })?;
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

                reply.send(&ServerMessage::TransportConnected {
                    transport_id: transport_id.clone(),
                })?;
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
                reply.send(&ServerMessage::ProducerCreated { producer_id })?;
            } else {
                anyhow::bail!("Not in a room");
            }
        }

        ClientMessage::Consume {
            producer_id,
            rtp_capabilities,
        } => {
            if let Some(room_id) = current_room_id.as_ref() {
                let consumer_info = room_manager
                    .create_consumer(
                        room_id,
                        participant_id,
                        sender,
                        producer_id.parse()?,
                        rtp_capabilities.clone(),
                        Some(sender.clone()),
                    )
                    .await?;

                metrics.inc_consumers_created();
                reply.send(&ServerMessage::ConsumerCreated {
                    consumer_id: consumer_info.id,
                    producer_id: consumer_info.producer_id.clone(),
                    kind: consumer_info.kind,
                    rtp_parameters: consumer_info.rtp_parameters,
                })?;

                // If the producer is already paused, immediately notify the consuming client
                // so it can hide the video tile instead of showing a black square.
                if consumer_info.producer_paused {
                    send_json(
                        metrics,
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

                reply.send(&ServerMessage::ConsumerResumed {
                    consumer_id: consumer_id.clone(),
                })?;
            } else {
                anyhow::bail!("Not in a room");
            }
        }

        ClientMessage::PauseConsumer { consumer_id } => {
            if let Some(room_id) = current_room_id.as_ref() {
                room_manager
                    .pause_consumer(room_id, participant_id, sender, consumer_id)
                    .await?;

                reply.send(&ServerMessage::ConsumerPaused {
                    consumer_id: consumer_id.clone(),
                })?;
            } else {
                anyhow::bail!("Not in a room");
            }
        }

        ClientMessage::CloseConsumer { consumer_id } => {
            if let Some(room_id) = current_room_id.as_ref() {
                room_manager
                    .close_consumer(room_id, participant_id, sender, consumer_id)
                    .await?;
                reply.acknowledge_control()?;
            } else {
                anyhow::bail!("Not in a room");
            }
        }

        ClientMessage::CloseProducer { producer_id } => {
            if let Some(room_id) = current_room_id.as_ref() {
                room_manager
                    .close_producer(room_id, participant_id, sender, producer_id)
                    .await?;
                reply.acknowledge_control()?;
            } else {
                anyhow::bail!("Not in a room");
            }
        }

        ClientMessage::PauseProducer { producer_id } => {
            if let Some(room_id) = current_room_id.as_ref() {
                room_manager
                    .pause_producer(room_id, participant_id, sender, producer_id)
                    .await?;
                reply.send(&ServerMessage::ProducerPaused {
                    producer_id: producer_id.clone(),
                })?;
            } else {
                anyhow::bail!("Not in a room");
            }
        }

        ClientMessage::ResumeProducer { producer_id } => {
            if let Some(room_id) = current_room_id.as_ref() {
                room_manager
                    .resume_producer(room_id, participant_id, sender, producer_id)
                    .await?;
                reply.send(&ServerMessage::ProducerResumed {
                    producer_id: producer_id.clone(),
                })?;
            } else {
                anyhow::bail!("Not in a room");
            }
        }

        ClientMessage::Reconnect { .. } | ClientMessage::RenewAuthentication { .. } => {
            // Handled in the main message loop before dispatching here
            // This branch should never be reached
        }

        ClientMessage::RestartIce { transport_id } => {
            if let Some(room_id) = current_room_id.as_ref() {
                let ice_parameters = room_manager
                    .restart_ice(room_id, participant_id, sender, transport_id)
                    .await?;

                reply.send(&ServerMessage::IceRestarted {
                    transport_id: transport_id.clone(),
                    ice_parameters,
                    // Relay credentials minted at transport creation may
                    // have expired; candidate gathering needs fresh ones.
                    ice_servers: make_ice_servers(turn_config),
                })?;
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
                reply.acknowledge_control()?;
            } else {
                anyhow::bail!("Not in a room");
            }
        }

        ClientMessage::ChatMessage { .. }
        | ClientMessage::PrivateMessage { .. }
        | ClientMessage::RetryChatMessage(_) => {
            let room_id = current_room_id
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("Not in a room"))?;
            let target = match message {
                ClientMessage::PrivateMessage {
                    target_participant_id,
                    ..
                } => Some(target_participant_id.as_str()),
                ClientMessage::RetryChatMessage(retry) => retry.target_participant_id.as_deref(),
                _ => None,
            };
            if let Some(target) = target {
                validate_target_id(target)?;
            }
            room_manager
                .handle_chat_command(room_id, participant_id, sender, message)
                .await?;
        }
        ClientMessage::SetChatPreferences { .. }
        | ClientMessage::ChangeNickname { .. }
        | ClientMessage::GetRoomSnapshot { .. }
        | ClientMessage::ListRoomBans { .. }
        | ClientMessage::RemoveRoomBan { .. }
        | ClientMessage::ListRoomMembers { .. }
        | ClientMessage::SetMemberRole { .. }
        | ClientMessage::ReportParticipant { .. }
        | ClientMessage::ListRoomReports { .. }
        | ClientMessage::ResolveRoomReport { .. } => {
            let room_id = current_room_id
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("Not in a room"))?;
            room_manager
                .handle_social_request(room_id, participant_id, sender, message)
                .await?;
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

    if matches!(
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
            | ClientMessage::RequestVoice
            | ClientMessage::UpdateRoomSettings { .. }
            | ClientMessage::SetTopic { .. }
            | ClientMessage::AdmitFromLobby { .. }
            | ClientMessage::DenyFromLobby { .. }
    ) {
        reply.acknowledge_room_control()?;
    }

    Ok(())
}

#[cfg(test)]
mod security_tests {
    use super::*;

    #[tokio::test]
    async fn shutdown_aborts_owned_tasks_on_drop() {
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (dropped_tx, dropped_rx) = tokio::sync::oneshot::channel();
        struct NotifyDrop(Option<tokio::sync::oneshot::Sender<()>>);
        impl Drop for NotifyDrop {
            fn drop(&mut self) {
                let _ = self.0.take().unwrap().send(());
            }
        }
        let task = OwnedTask(tokio::spawn(async move {
            let _guard = NotifyDrop(Some(dropped_tx));
            let _ = entered_tx.send(());
            std::future::pending::<()>().await;
        }));
        entered_rx.await.unwrap();
        drop(task);
        tokio::time::timeout(Duration::from_secs(1), dropped_rx)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn shutdown_cancels_retained_grace_and_rejects_late_timers() {
        let map = GracePeriodMap::new();
        let retained = grace_entry("retained");
        let retained_abort = retained.timer.abort_handle();
        assert!(map.insert("room".into(), "peer".into(), retained));
        assert_eq!(map.close(), 1);
        assert_eq!(map.close(), 0);
        let late = grace_entry("late");
        let late_abort = late.timer.abort_handle();
        assert!(!map.clone().insert("room".into(), "late-peer".into(), late));
        tokio::time::timeout(Duration::from_secs(1), async {
            while !retained_abort.is_finished() || !late_abort.is_finished() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(map.inner.read().unwrap().is_empty());
    }

    #[test]
    fn control_acknowledgement_preserves_legacy_silence_and_echoes_modern_ids() {
        let metrics = ServerMetrics::new();
        let (sender, mut receiver) = mpsc::channel(2);
        for request_id in [None, Some("control-1")] {
            ReplySender {
                metrics: &metrics,
                sender: &sender,
                request_id,
            }
            .acknowledge_control()
            .unwrap();
            if let Some(request_id) = request_id {
                let reply: serde_json::Value =
                    serde_json::from_str(&receiver.try_recv().unwrap()).unwrap();
                assert_eq!(
                    reply,
                    serde_json::json!({ "type": "mediaControlApplied", "requestId": request_id })
                );
            } else {
                assert!(matches!(
                    receiver.try_recv(),
                    Err(mpsc::error::TryRecvError::Empty)
                ));
            }
        }
    }

    #[test]
    fn room_control_acknowledgement_requires_and_echoes_request_id() {
        let metrics = ServerMetrics::new();
        let (sender, mut receiver) = mpsc::channel(2);
        for request_id in [None, Some("room-control-1")] {
            ReplySender {
                metrics: &metrics,
                sender: &sender,
                request_id,
            }
            .acknowledge_room_control()
            .unwrap();
            if let Some(request_id) = request_id {
                let reply: serde_json::Value =
                    serde_json::from_str(&receiver.try_recv().unwrap()).unwrap();
                assert_eq!(
                    reply,
                    serde_json::json!({"type":"roomControlApplied", "requestId": request_id})
                );
            } else {
                assert!(matches!(
                    receiver.try_recv(),
                    Err(mpsc::error::TryRecvError::Empty)
                ));
            }
        }
    }

    #[test]
    fn password_challenge_is_typed_and_internal_errors_stay_generic() {
        let error = anyhow::Error::new(RoomPasswordRequired).context("joining a room");
        assert_eq!(
            serde_json::to_value(client_error_response(&error)).unwrap(),
            serde_json::json!({ "type": "roomPasswordRequired" }),
        );
        for error in [
            anyhow::anyhow!("database connection details"),
            anyhow::anyhow!("This room requires a password"),
        ] {
            assert_eq!(
                serde_json::to_value(client_error_response(&error)).unwrap(),
                serde_json::json!({
                    "type": "error",
                    "message": "Request could not be completed",
                }),
            );
        }
    }

    #[tokio::test]
    async fn socket_counters_distinguish_completed_writes_errors_and_timeouts() {
        let metrics = ServerMetrics::new();
        let mut successful = futures_util::sink::drain();
        assert!(
            write_message(
                &mut successful,
                Message::Text("fixture".into()),
                &metrics,
                None,
                tokio::time::Instant::now() + Duration::from_secs(1),
                OperationKind::SocketWrite
            )
            .await
            .is_ok()
        );
        let mut failed = Box::pin(futures_util::sink::unfold((), |(), _: Message| async {
            Err::<(), ()>(())
        }));
        assert!(matches!(
            write_message(
                &mut failed,
                Message::Text("fixture".into()),
                &metrics,
                None,
                tokio::time::Instant::now() + Duration::from_secs(1),
                OperationKind::SocketWrite
            )
            .await,
            Err(Outcome::Error)
        ));
        let mut stalled = Box::pin(futures_util::sink::unfold((), |(), _: Message| {
            std::future::pending::<Result<(), ()>>()
        }));
        assert!(matches!(
            write_message(
                &mut stalled,
                Message::Text("fixture".into()),
                &metrics,
                None,
                tokio::time::Instant::now(),
                OperationKind::SocketWrite
            )
            .await,
            Err(Outcome::Timeout)
        ));
        let rendered = metrics.render_prometheus(0, 0, 0);
        assert!(rendered.contains("simplestchat_messages_sent_total 1\n"));
        assert!(rendered.contains("simplestchat_message_send_failed_total 2\n"));
    }

    #[tokio::test]
    async fn cancelled_socket_write_is_not_a_completed_write_or_error() {
        let metrics = ServerMetrics::new();
        let mut stalled = Box::pin(futures_util::sink::unfold((), |(), _: Message| {
            std::future::pending::<Result<(), ()>>()
        }));
        let mut future = Box::pin(write_message(
            &mut stalled,
            Message::Text("fixture".into()),
            &metrics,
            None,
            tokio::time::Instant::now() + Duration::from_secs(10),
            OperationKind::SocketWrite,
        ));
        assert!(futures_util::poll!(&mut future).is_pending());
        drop(future);
        let rendered = metrics.render_prometheus(0, 0, 0);
        assert!(rendered.contains("simplestchat_messages_sent_total 0\n"));
        assert!(rendered.contains("simplestchat_message_send_failed_total 0\n"));
    }

    #[tokio::test]
    async fn shutdown_notice_uses_one_deadline_and_counts_a_timeout() {
        let metrics = ServerMetrics::new();
        let mut stalled = Box::pin(futures_util::sink::unfold((), |(), _: Message| {
            std::future::pending::<Result<(), ()>>()
        }));
        let result = write_message(
            &mut stalled,
            Message::Text("fixture shutdown notice".into()),
            &metrics,
            None,
            tokio::time::Instant::now(),
            OperationKind::ShutdownNotification,
        )
        .await;
        assert!(matches!(result, Err(Outcome::Timeout)));
        let rendered = metrics.render_prometheus(0, 0, 0);
        assert!(rendered.contains("simplestchat_messages_sent_total 0\n"));
        assert!(rendered.contains("simplestchat_message_send_failed_total 1\n"));
    }

    #[test]
    fn direct_signaling_queue_rejections_are_counted_without_payloads() {
        let metrics = ServerMetrics::new();
        let (sender, receiver) = mpsc::channel(1);
        let message = ServerMessage::Error {
            message: "PRIVATE_FIXTURE_CONTENT".into(),
        };
        send_json(&metrics, &sender, &message).unwrap();
        assert!(
            !send_json(&metrics, &sender, &message)
                .unwrap_err()
                .to_string()
                .contains("PRIVATE")
        );
        drop(receiver);
        assert!(send_json(&metrics, &sender, &message).is_err());
        let rendered = metrics.render_prometheus(0, 0, 0);
        assert!(rendered.contains("simplestchat_outbound_queue_full_total 1\n"));
        assert!(rendered.contains("simplestchat_outbound_queue_closed_total 1\n"));
        assert!(rendered.contains("simplestchat_messages_sent_total 0\n"));
    }

    #[test]
    fn password_challenge_allows_one_immediate_answer_for_the_same_room() {
        let now = Instant::now();
        let mut attempts = JoinAttemptState::default();
        assert!(attempts.allow("private-room", false, now));
        attempts.password_challenge_room = Some("private-room".to_string());
        assert!(attempts.allow("private-room", true, now));
        assert!(!attempts.allow("private-room", true, now));
        assert!(attempts.allow("private-room", true, now + Duration::from_secs(1)));
    }

    #[test]
    fn unrelated_joins_do_not_use_the_password_challenge_allowance() {
        let now = Instant::now();
        for (room_id, has_password) in [("another-room", true), ("private-room", false)] {
            let mut attempts = JoinAttemptState::default();
            assert!(attempts.allow("private-room", false, now));
            attempts.password_challenge_room = Some("private-room".to_string());
            assert!(!attempts.allow(room_id, has_password, now));
            assert!(!attempts.allow("private-room", true, now));
        }
    }

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
        assert!(is_consumer_mutation(&ClientMessage::CloseConsumer {
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
            client_message_id: None,
            sequence: None,
        }));
    }

    #[test]
    fn consumer_and_frame_bursts_admit_the_configured_media_startup() {
        let now = Instant::now();
        for limit in [16, 64, 128] {
            let mut session = MediaSessionRateState::with_consumer_limit(now, limit);
            let capacity = frame_capacity(session.consumer_mutation_capacity_us);
            let mut frame_tokens = capacity;
            let mut frame_refill = now;
            // Full producer and transport setup plus join and capabilities.
            for _ in 0..MEDIA_MUTATION_RATE_LIMIT_MAX_TOKENS
                + TRANSPORT_MUTATION_RATE_LIMIT_MAX_TOKENS
                + 2
            {
                assert!(consume_rate_token(
                    &mut frame_tokens,
                    &mut frame_refill,
                    now,
                    RATE_LIMIT_REFILL_RATE,
                    capacity,
                ));
            }
            for index in 0..limit {
                for message in [
                    ClientMessage::Consume {
                        producer_id: format!("producer-{index}"),
                        rtp_capabilities: Default::default(),
                    },
                    ClientMessage::ResumeConsumer {
                        consumer_id: format!("consumer-{index}"),
                    },
                    ClientMessage::SetConsumerPreferredLayers {
                        consumer_id: format!("consumer-{index}"),
                        spatial_layer: 1,
                        temporal_layer: None,
                    },
                ] {
                    assert!(!session.limit_exceeded(&message, now));
                    assert!(consume_rate_token(
                        &mut frame_tokens,
                        &mut frame_refill,
                        now,
                        RATE_LIMIT_REFILL_RATE,
                        capacity,
                    ));
                }
            }
            let extra = ClientMessage::ResumeConsumer {
                consumer_id: "extra".into(),
            };
            assert!(session.limit_exceeded(&extra, now), "burst remains bounded");
            let later = now + Duration::from_secs(1);
            for _ in 0..CONSUMER_MUTATION_RATE_LIMIT_REFILL_RATE {
                assert!(!session.limit_exceeded(&extra, later));
            }
            assert!(
                session.limit_exceeded(&extra, later),
                "sustained rate remains bounded"
            );
        }
        if let Ok(overflowing_limit) = usize::try_from(u64::MAX / (3 * TOKEN_US) + 1) {
            assert_eq!(consumer_mutation_capacity(overflowing_limit), u64::MAX);
        }
        assert_eq!(frame_capacity(u64::MAX), u64::MAX);
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

        for _ in 0..entry.media_rate_state.consumer_mutation_capacity_us / TOKEN_US {
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
            client_message_id: None,
            sequence: None,
        }));
    }

    fn grace_entry(token: &str) -> GraceEntry {
        let (sender, _receiver) = mpsc::channel(1);
        GraceEntry {
            reconnect_token: token.to_string(),
            authenticated_subject: None,
            authenticated_version: None,
            media_rate_state: MediaSessionRateState::new(),
            sender,
            timer: tokio::spawn(std::future::pending()),
        }
    }

    #[tokio::test]
    async fn credential_revocation_only_removes_older_account_grace_entries() {
        let map = GracePeriodMap::new();
        for (token, subject, version) in [
            ("old", Some("alice"), Some(0)),
            ("new", Some("alice"), Some(1)),
            ("other", Some("bob"), Some(0)),
            ("guest", None, None),
        ] {
            let mut entry = grace_entry(token);
            entry.authenticated_subject = subject.map(str::to_owned);
            entry.authenticated_version = version;
            assert!(map.insert("room".into(), token.into(), entry));
        }
        let removed = map.take_revoked("alice", 1);
        assert_eq!(removed.len(), 1);
        assert_eq!(removed[0].1, "old");
        for (token, subject) in [
            ("new", Some("alice")),
            ("other", Some("bob")),
            ("guest", None),
        ] {
            map.remove_if_token_matches("room", token, token, subject)
                .expect("unaffected session retained")
                .timer
                .abort();
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
