#![forbid(unsafe_code)]

// WebSocket connection handler for individual clients

use super::protocol::{
    ClientMessage, ErrorCode, RequestHeader, ServerMessage, ServerReply, SocialResponse,
};
use crate::auth::types::Claims;
use crate::diagnostics::{self, OperationKind, Outcome, Stage};
use crate::ids::{ParticipantId, RoomId};
use crate::metrics::ServerMetrics;
use crate::room::{JoinResult, RoomManager, RoomPasswordRequired, SettingsPatch, settings};
use crate::secret::SecretString;
use crate::turn::TurnConfig;
use axum::extract::ws::{CloseFrame, Message, WebSocket, close_code};
use futures_util::{Sink, SinkExt, Stream, StreamExt};
use mediasoup::prelude::TransportId;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio::sync::{Notify, OwnedSemaphorePermit};
use tracing::{Instrument, debug, error, info, warn};
use uuid::Uuid;

#[path = "connection_authentication.rs"]
mod authentication;
pub(crate) use authentication::RenewalAuthenticator;
#[path = "connection_credentials.rs"]
mod credentials;
use credentials::*;
#[path = "connection_bandwidth.rs"]
mod bandwidth;
#[path = "connection_gates.rs"]
mod gates;
#[path = "connection_grace.rs"]
mod grace;
#[path = "connection_media.rs"]
mod media;
#[path = "connection_moderation.rs"]
mod moderation;
#[path = "connection_token_bucket.rs"]
mod token_bucket;
use super::telemetry::WarningFamily;
use bandwidth::BandwidthTierUpdate;
use gates::*;
pub(crate) use grace::GracePeriodMap;
use grace::*;

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

/// Say why a socket is being closed for exceeding a command budget.
fn log_command_close(
    why: CloseReason,
    participant_id: &ParticipantId,
    metrics: &ServerMetrics,
    connection_id: Option<u64>,
) {
    match why {
        CloseReason::MediaMutations(violations) => warn!(
            %participant_id,
            media_mutation_violations = violations,
            "Closing WebSocket after repeated media-mutation rate violations"
        ),
        CloseReason::AdminMutations(violations) => warn!(
            %participant_id,
            admin_mutation_violations = violations,
            "Closing WebSocket after repeated room-administration rate violations"
        ),
        CloseReason::ChatFlood => {
            if metrics
                .telemetry()
                .warning_should_log(WarningFamily::ChatRate)
            {
                warn!(
                    connection_id,
                    "Closing WebSocket for chat flooding; further instances counted in metrics"
                );
            }
        }
    }
}

fn client_error_response(error: &anyhow::Error) -> ServerMessage {
    if error.is::<RoomPasswordRequired>() {
        ServerMessage::RoomPasswordRequired
    } else {
        ServerMessage::Error {
            code: ErrorCode::RequestFailed,
            message: "Request could not be completed".to_string(),
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
            mpsc::error::TrySendError::Full(_) => {
                metrics.inc_outbound_queue_full();
                super::outbound::request_resync(sender);
            }
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
    /// Closure/layer commands without an ID are fire-and-forget: the native
    /// generator and late browser teardown use this form. Correlated browser
    /// controls request acknowledgement to detect an unapplied change.
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
        ClientMessage::CreateRecvTransport | ClientMessage::ReplaceRecvTransport { .. } => {
            OperationKind::CreateRecvTransport
        }
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
        ClientMessage::Typing { .. } => OperationKind::Chat,
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
        | ClientMessage::SetChatStyle { .. }
        | ClientMessage::ReactToMessage { .. }
        | ClientMessage::RemoveChatMessage { .. }
        | ClientMessage::EditChatMessage { .. }
        | ClientMessage::GetPinnedMessages { .. }
        | ClientMessage::GetAttachmentAccess { .. }
        | ClientMessage::SetPinnedMessage { .. }
        | ClientMessage::GetRoomSnapshot { .. }
        | ClientMessage::GetChatHistory { .. }
        | ClientMessage::MarkChatRead { .. }
        | ClientMessage::SetRoomHistory { .. }
        | ClientMessage::ListRoomBans { .. }
        | ClientMessage::RemoveRoomBan { .. }
        | ClientMessage::ListRoomMembers { .. }
        | ClientMessage::SetMemberRole { .. }
        | ClientMessage::ReportParticipant { .. }
        | ClientMessage::ListRoomReports { .. }
        | ClientMessage::ResolveRoomReport { .. }
        | ClientMessage::ListModerationEvents { .. }
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

fn reconnect_attempt_allowed(current_room_id: Option<&RoomId>, in_lobby: bool) -> bool {
    current_room_id.is_none() && !in_lobby
}

/// Handles a single WebSocket connection
#[expect(
    clippy::too_many_arguments,
    reason = "connection boundary keeps owned session resources explicit"
)]
pub(crate) async fn handle_connection(
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
    authenticated_user: Option<Claims>,
    client_ip: Option<std::net::IpAddr>,
    db_pool: Option<sqlx::PgPool>,
    mut auth_revocations: tokio::sync::broadcast::Receiver<(String, i64)>,
    renewal_authenticator: Option<RenewalAuthenticator>,
    timing: ConnectionTiming,
) {
    // Use authenticated user ID if available, otherwise generate anonymous UUID
    let mut participant_id = match authenticated_user.as_ref() {
        Some(claims) => match claims.sub.parse::<ParticipantId>() {
            Ok(id) => id,
            Err(_) => {
                tracing::warn!("Rejecting a connection whose account subject is not a UUID");
                return;
            }
        },
        None => ParticipantId::random(),
    };

    let is_authenticated = authenticated_user.is_some();
    let mut session = CredentialSession::new(authenticated_user);

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
    let overflow_registration = super::outbound::register(&tx);
    let mut writer_overflow = overflow_registration.subscribe();
    let mut reader_overflow = overflow_registration.subscribe();

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
            _ = super::outbound::wait_for_resync(&mut writer_overflow) => {},
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
        if *writer_overflow.borrow() && !writer_drain.is_draining() {
            // This control path has no dependency on the full application queue.
            let _ = tokio::time::timeout(
                DRAIN_SEND_TIMEOUT,
                ws_sender.send(Message::Close(Some(CloseFrame {
                    code: close_code::AGAIN,
                    reason: "Room updates missed; reconnect to synchronize".into(),
                }))),
            )
            .await;
        }
        debug!(
            connection_id = diagnostic_connection_id,
            "Send task finished"
        );
    }));

    // Handle incoming messages
    let mut current_room_id: Option<RoomId> = None;
    // True while the participant waits in a lobby. Shared (Arc) because admission
    // happens on the moderator's connection task — admit_from_lobby clears it via
    // the LobbyEntry so this task's guard opens without any local event.
    let in_lobby = Arc::new(AtomicBool::new(false));
    let mut stats_task: Option<OwnedTask> = None;
    let mut bwe_sender: Option<mpsc::Sender<crate::media::types::ReceiveBandwidthEstimate>> = None;

    // Token bucket rate limiter state
    let consumer_limit = room_manager
        .media_server()
        .config()
        .max_consumers_per_participant;
    let mut gates = CommandGates::new(Instant::now(), consumer_limit);
    let mut last_frame_received = Instant::now();
    let mut next_heartbeat = Instant::now() + timing.heartbeat_interval;
    let mut peer_close_received = false;

    loop {
        // The JWT is a connection credential, not only a handshake credential.
        // Cap every receive wait by its absolute expiry so active traffic cannot
        // keep an authenticated socket alive indefinitely.
        if session.expired() {
            info!(
                %participant_id,
                "Accepted authentication lifetime or validation allowance expired; closing WebSocket"
            );
            session.invalidate();
            break;
        }
        let idle_remaining = timing
            .idle_timeout
            .saturating_sub(last_frame_received.elapsed());
        if idle_remaining.is_zero() {
            // A joined socket gets heartbeat pings, so its idle close means the
            // peer stopped answering; an unjoined socket idling out is expected.
            if current_room_id.is_some() {
                warn!("Idle timeout for participant {}", participant_id);
            } else {
                info!("Idle timeout for unjoined socket {}", participant_id);
            }
            break;
        }
        let receive_deadline =
            tokio::time::Instant::from_std(session.receive_deadline(idle_remaining));

        let receive_result = tokio::select! {
            biased;
            _ = drain.wait() => break,
            _ = super::outbound::wait_for_resync(&mut reader_overflow) => break,
            _ = tx.closed() => break,
            notice = auth_revocations.recv(), if is_authenticated => {
                if session.revoked_by(notice) {
                    break;
                }
                continue;
            }
            _ = tokio::time::sleep_until(session.next_check.into()), if is_authenticated && session.pending.is_none() => {
                if !session.begin_validation(db_pool.clone()) {
                    break;
                }
                continue;
            }
            result = async { session.pending.as_mut().expect("guarded by is_some").await }, if session.pending.is_some() => {
                if !session.settle(result, &mut auth_revocations, &drain) {
                    break;
                }
                continue;
            }
            result = tokio::time::timeout_at(receive_deadline, ws_receiver.next()) => result,
            _ = tokio::time::sleep_until(next_heartbeat.into()), if current_room_id.is_some() => {
                // Schedule from now, not a missed-tick backlog. A membership
                // lookup never renews liveness or delays drain; only a received
                // frame (normally the protocol Pong) renews the idle deadline.
                next_heartbeat = Instant::now() + timing.heartbeat_interval;
                if last_frame_received.elapsed() >= timing.heartbeat_interval
                    && let Some(room_id) = current_room_id.as_ref()
                {
                    // The timer may have consumed most of the receive budget.
                    // Preserve its absolute deadline instead of reusing the
                    // relative timeout calculated before that wait.
                    let mut lookup_deadline = (tokio::time::Instant::now()
                        + timing.membership_timeout).min(receive_deadline);
                    if is_authenticated {
                        lookup_deadline = lookup_deadline.min(session.next_check.into());
                    }
                    let bound = tokio::select! {
                        biased;
                        _ = drain.wait() => break,
                        _ = tx.closed() => break,
                        notice = auth_revocations.recv(), if is_authenticated => {
                            if session.revoked_by(notice) {
                                break;
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
                if session.expired() {
                    info!(
                        %participant_id,
                        "Accepted authentication lifetime or validation allowance expired; closing WebSocket"
                    );
                    session.invalidate();
                } else if current_room_id.is_some() {
                    warn!("Idle timeout for participant {}", participant_id);
                } else {
                    info!("Idle timeout for unjoined socket {}", participant_id);
                }
                break;
            }
        };
        // A ready frame can win against timeout_at at the exact deadline.
        // Never dispatch it using an expired accepted credential or allowance.
        if session.expired() {
            session.invalidate();
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
        if let FrameAdmission::Refused {
            violations: rate_limit_violations,
        } = gates.admit_frame(Instant::now())
        {
            if matches!(&msg, Message::Close(_)) {
                break;
            }
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
                    code: ErrorCode::RateLimited,
                    message: format!(
                        "Rate limit exceeded: max {} frames/second",
                        RATE_LIMIT_REFILL_RATE
                    ),
                });
            }
            if rate_limit_violations >= RATE_LIMIT_MAX_VIOLATIONS {
                warn!(
                    %participant_id,
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
                        %participant_id,
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
                                code: ErrorCode::InvalidMessage,
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
                            let outcome = session
                                .renew(
                                    renewal_authenticator.as_ref(),
                                    token,
                                    db_pool.as_ref(),
                                    &mut auth_revocations,
                                    &drain,
                                    &tx,
                                )
                                .await;
                            let Some(response) = session.apply_renewal(outcome, request_id) else {
                                break;
                            };
                            if send_json(&metrics, &tx, &response).is_err() {
                                break;
                            }
                            continue;
                        }
                        match gates.admit(&client_msg, Instant::now()) {
                            Admission::Allowed => {}
                            Admission::Refused(response) => {
                                let _ = reply.send(&response);
                                continue;
                            }
                            Admission::Close { response, why } => {
                                log_command_close(
                                    why,
                                    &participant_id,
                                    &metrics,
                                    diagnostic_connection_id,
                                );
                                let _ = reply.send(&response);
                                break;
                            }
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
                                current_room_id.as_ref(),
                                in_lobby.load(Ordering::Acquire),
                            ) {
                                warn!(
                                    %participant_id,
                                    "Rejected reconnect from a socket with an active room session"
                                );
                                let _ = reply.send(&ServerMessage::ReconnectResult {
                                    success: false,
                                    participant_id: *reconnect_id,
                                    reconnect_token: None,
                                });
                                continue;
                            }

                            // An owned subject: the session's check in flight must not be
                            // shared across this await.
                            let reconnect_subject =
                                session.claims().map(|claims| claims.sub.clone());
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
                                        client_token.expose_secret(),
                                        reconnect_subject.as_deref(),
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
                                gates.restore_media(restored_media_rate_state);
                                participant_id = *reconnect_id;
                                current_room_id = Some(reconnect_room.clone());

                                // Restart stats task for reconnected session
                                if let Some(task) = stats_task.take() {
                                    task.abort();
                                }

                                let (bwe_tx, bwe_rx) = mpsc::channel::<
                                    crate::media::types::ReceiveBandwidthEstimate,
                                >(32);
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
                                    participant_id,
                                    tx.clone(),
                                    bwe_rx,
                                ));
                            }

                            let response_sent = reply.send(&ServerMessage::ReconnectResult {
                                success,
                                participant_id: if success {
                                    participant_id
                                } else {
                                    *reconnect_id
                                },
                                reconnect_token: fresh_reconnect_token
                                    .clone()
                                    .map(SecretString::from),
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
                        // Only an attachment grant reads the claims inside the owned
                        // dispatch future, and it takes its own copy.
                        let attachment_claims =
                            matches!(&client_msg, ClientMessage::GetAttachmentAccess { .. })
                                .then(|| session.claims().cloned())
                                .flatten();

                        let start = Instant::now();
                        let operation = metrics
                            .diagnostics()
                            .operation(diagnostic_operation(&client_msg), diagnostic_connection_id);
                        let result = operation
                            .scope(diagnostics::measure_result(
                                Stage::Dispatch,
                                async {
                                    if let ClientMessage::GetAttachmentAccess { request_id, attachment_id } = &client_msg {
                                        if in_lobby.load(Ordering::Acquire) {
                                            anyhow::bail!("Cannot perform this action while waiting in lobby");
                                        }
                                        let room_id = current_room_id.as_ref().ok_or_else(|| anyhow::anyhow!("Not in a room"))?;
                                        let pool = db_pool.as_ref().ok_or_else(|| anyhow::anyhow!("Database unavailable"))?;
                                        let access = crate::attachments::grant_access(
                                            &room_manager, pool, room_id, &participant_id, &tx,
                                            attachment_claims.as_ref(), *attachment_id,
                                        ).await.map_err(|_| anyhow::anyhow!("Attachment unavailable"))?;
                                        reply.send(&ServerMessage::SocialResponse {
                                            request_id: request_id.clone(),
                                            response: SocialResponse::GetAttachmentAccess {
                                                token: access.token,
                                                expires_at: access.expires_at,
                                            },
                                        })
                                    } else {
                                        handle_client_message(
                                            &client_msg,
                                            Dispatch {
                                                participant_id: &participant_id,
                                                current_room_id: &mut current_room_id,
                                                in_lobby: &in_lobby,
                                                reply: &reply,
                                                room_manager: &room_manager,
                                                turn_config: &turn_config,
                                                reconnect_token: &mut reconnect_token,
                                                bwe_sender: &bwe_sender,
                                                is_authenticated,
                                                client_ip,
                                            },
                                        ).await
                                    }
                                },
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
                            gates.reset_media(Instant::now(), consumer_limit);
                        }

                        if let Err(e) = result {
                            if e.is::<RoomPasswordRequired>()
                                && let ClientMessage::JoinRoom { room_id, .. } = &client_msg
                            {
                                gates.note_password_challenge(room_id.clone());
                            }
                            // Invalid IDs, stale media state, and authorization
                            // failures are routine client-controlled outcomes.
                            // Keep them observable without allowing a socket to
                            // amplify them into production error logs.
                            debug!(
                                %participant_id,
                                error = %e,
                                "Client signaling request was rejected"
                            );
                            metrics.inc_errors();
                            // If channel is closed, send task has exited — break
                            if tx.is_closed() {
                                break;
                            }
                            let (code, public_message) = e
                                .downcast_ref::<crate::room::social::SocialFailure>()
                                .map_or(
                                    (ErrorCode::RequestFailed, "Request could not be completed"),
                                    |error| (error.code, error.message.as_str()),
                                );
                            let response = client_msg
                                .social_error(code, public_message)
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
                            let (bwe_tx, bwe_rx) =
                                mpsc::channel::<crate::media::types::ReceiveBandwidthEstimate>(32);
                            bwe_sender = Some(bwe_tx);

                            stats_task = Some(spawn_stats_task(
                                room_manager.clone(),
                                room_id.clone(),
                                participant_id,
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
                            %participant_id,
                            error = %e,
                            "Invalid client signaling message"
                        );
                        metrics.inc_errors();
                        let _ = reply.send(&ServerMessage::Error {
                            code: ErrorCode::InvalidMessage,
                            message: "Invalid message format".to_string(),
                        });
                    }
                }
            }
            Message::Binary(payload) => {
                warn!(
                    %participant_id,
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

    if !drain.is_draining() {
        session
            .revalidate_on_disconnect(db_pool.as_ref(), &mut auth_revocations, &drain)
            .await;
    }

    // On disconnect: lobby participants clean up immediately, room participants get grace period
    if let Some(room_id) = current_room_id.take() {
        if drain.is_draining() {
            // Global room drain clears both live and retained memberships. Do
            // not start a new grace timer or race its authoritative cleanup.
            debug!(%room_id, %participant_id, "Membership handed to server drain");
        } else if in_lobby.load(Ordering::Acquire) || session.invalidated() {
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

            retain_for_reconnect(
                DisconnectedMember {
                    room_id,
                    participant_id,
                    reconnect_token,
                    media_rate_state: gates.into_media_state(),
                    sender: tx.clone(),
                    disconnected_at,
                    claims: session.claims().cloned(),
                    continuity: session.continuity(),
                    credential_deadline: session.deadline(),
                },
                &grace_periods,
                &room_manager,
                db_pool,
                auth_revocations,
                &drain,
            )
            .await;
        }
    }

    // _conn_guard dropped here → dec_connections_active
    // _permit dropped here → release semaphore

    if peer_close_received {
        // The peer-close path has already consumed the writer's join result.
    } else if drain.is_draining() || *reader_overflow.borrow() {
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
                %participant_id,
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
        && let Some(room_id) = disconnected_room_id.as_ref()
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
#[path = "connection_authorization_tests.rs"]
mod authorization_tests;

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
    room_id: RoomId,
    participant_id: ParticipantId,
    sender: mpsc::Sender<crate::OutboundJson>,
    mut bwe_rx: mpsc::Receiver<crate::media::types::ReceiveBandwidthEstimate>,
) -> OwnedTask {
    OwnedTask(tokio::spawn(async move {
        let drain = room_manager.drain_signal();
        let mut last_transport: Option<TransportId> = None;
        let mut last_bitrate: u32 = 0;
        let mut last_sent_bitrate: u32 = 0;
        let mut layer_update = BandwidthTierUpdate::new(Instant::now());

        // ConnectionStats interval — no IPC call, just uses last-known bitrate
        let mut stats_interval = tokio::time::interval(Duration::from_secs(10));
        stats_interval.tick().await; // Skip first immediate tick

        loop {
            let layer_deadline = layer_update.deadline();
            tokio::select! {
                biased;
                _ = drain.wait() => break,
                _ = sender.closed() => break,
                // BWE event: bandwidth estimation changed
                bwe = bwe_rx.recv() => {
                    match bwe {
                        Some(estimate) => {
                            if !room_manager.is_current_recv_transport(
                                &room_id, &participant_id, &sender, estimate.transport_id,
                            ).await { continue; }
                            if last_transport != Some(estimate.transport_id) {
                                last_transport = Some(estimate.transport_id);
                                last_sent_bitrate = 0;
                                layer_update = BandwidthTierUpdate::new(Instant::now());
                            }
                            last_bitrate = estimate.bitrate;
                            layer_update.observe(estimate.bitrate);
                        }
                        None => break, // Channel closed — participant disconnected
                    }
                }

                // The newest tier must eventually apply even if its only event
                // arrived inside the two-second interval. Failed/partial writes
                // remain pending, with at most one retry per interval.
                _ = async {
                    match layer_deadline {
                        Some(deadline) => tokio::time::sleep_until(deadline.into()).await,
                        None => std::future::pending::<()>().await,
                    }
                } => {
                    if bwe_rx.is_closed() {
                        break;
                    }
                    if let Some(target_layer) = layer_update.begin_attempt(Instant::now()) {
                        let Some(transport_id) = last_transport else { continue; };
                        let result = tokio::select! {
                            biased;
                            _ = drain.wait() => break,
                            _ = sender.closed() => break,
                            result = room_manager.set_bandwidth_ceiling(
                                &room_id, &participant_id, &sender, transport_id, target_layer
                            ) => result,
                        };
                        layer_update.complete_attempt(target_layer, result.is_ok());
                        match result {
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

                // Timer: send ConnectionStats every ~10s using last-known bitrate (zero IPC)
                _ = stats_interval.tick() => {
                    let Some(transport_id) = last_transport else { continue; };
                    if !room_manager.is_current_recv_transport(
                        &room_id, &participant_id, &sender, transport_id,
                    ).await {
                        last_transport = None;
                        last_bitrate = 0;
                        last_sent_bitrate = 0;
                        layer_update = BandwidthTierUpdate::new(Instant::now());
                        continue;
                    }
                    if last_bitrate > 0 && last_bitrate != last_sent_bitrate {
                        last_sent_bitrate = last_bitrate;
                        if let Ok(json) = serde_json::to_string(&ServerMessage::ConnectionStats {
                            available_bitrate: Some(last_bitrate),
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

pub(crate) const MAX_PARTICIPANT_NAME_LEN: usize = 64;

/// The report a sanction answers must at least look like one of ours.
fn parse_report_id(report_id: Option<&str>) -> anyhow::Result<Option<Uuid>> {
    report_id
        .map(|id| {
            id.parse::<Uuid>()
                .map_err(|_| anyhow::anyhow!("Invalid report"))
        })
        .transpose()
}

fn validate_reason(reason: Option<&str>) -> anyhow::Result<()> {
    if reason.is_some_and(|reason| reason.len() > 1_024 || reason.chars().any(char::is_control)) {
        anyhow::bail!("Reason must be at most 1024 characters without control characters");
    }
    Ok(())
}

fn validate_durable_reason(reason: Option<&str>) -> anyhow::Result<()> {
    if reason.is_some_and(|reason| !crate::room::moderation::valid_reason(reason)) {
        anyhow::bail!("Reason must be at most 256 characters without control characters");
    }
    Ok(())
}

/// What one client command may read and change: this connection's identity and
/// room membership, its reply channel and the services the dispatcher reaches.
/// Every field borrows state the connection task owns for the whole socket.
struct Dispatch<'a, 'r> {
    participant_id: &'a ParticipantId,
    current_room_id: &'a mut Option<RoomId>,
    in_lobby: &'a Arc<AtomicBool>,
    reply: &'a ReplySender<'r>,
    room_manager: &'a Arc<RoomManager>,
    turn_config: &'a Option<Arc<TurnConfig>>,
    reconnect_token: &'a mut String,
    bwe_sender: &'a Option<mpsc::Sender<crate::media::types::ReceiveBandwidthEstimate>>,
    is_authenticated: bool,
    client_ip: Option<std::net::IpAddr>,
}

/// Handle a single client message
async fn handle_client_message(
    message: &ClientMessage,
    connection: Dispatch<'_, '_>,
) -> anyhow::Result<()> {
    let Dispatch {
        participant_id,
        current_room_id,
        in_lobby,
        reply,
        room_manager,
        turn_config,
        reconnect_token,
        bwe_sender,
        is_authenticated,
        client_ip,
    } = connection;
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
            chat_style,
        } => {
            if participant_name.trim().is_empty()
                || participant_name.len() > MAX_PARTICIPANT_NAME_LEN
                || !crate::labels::is_plain(participant_name)
                || crate::labels::is_reserved_name(participant_name)
            {
                anyhow::bail!(
                    "Invalid participant_name: must be 1-{MAX_PARTICIPANT_NAME_LEN} characters without control characters"
                );
            }
            if password
                .as_ref()
                .map(SecretString::expose_secret)
                .is_some_and(|password| {
                    password.is_empty()
                        || password.len() > settings::MAX_PASSWORD_LEN
                        || password.chars().any(char::is_control)
                })
            {
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
                    *participant_id,
                    participant_name.to_string(),
                    sender.clone(),
                    is_authenticated,
                    in_lobby.clone(),
                    password.as_ref().map(SecretString::expose_secret),
                    &session_reconnect_token,
                    client_ip,
                    chat_style.clone(),
                )
                .await?;

            match join_result {
                JoinResult::Joined {
                    participants,
                    role,
                    room_settings,
                    chat_style,
                    name,
                } => {
                    *current_room_id = Some(room_id.clone());
                    in_lobby.store(false, Ordering::Release);
                    metrics.inc_joins();

                    reply.send(&ServerMessage::RoomJoined {
                        participant_id: *participant_id,
                        participants,
                        reconnect_token: session_reconnect_token.into(),
                        your_role: role,
                        your_name: name,
                        your_chat_style: chat_style,
                        room_settings: room_settings.map(|settings| *settings),
                    })?;
                    room_manager.note_room_visit(room_id, participant_id, is_authenticated);
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
                let lifecycle_id = if tracing::enabled!(target: "simplestchat::lifecycle", tracing::Level::DEBUG)
                {
                    Some(participant_id.as_uuid())
                } else {
                    None
                };
                if let Some(lifecycle_id) = lifecycle_id {
                    debug!(
                        target: "simplestchat::lifecycle",
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
                        target: "simplestchat::lifecycle",
                        event = "explicit_leave_finished",
                        participant_id = %lifecycle_id,
                        "lifecycle"
                    );
                }
                in_lobby.store(false, Ordering::Release);
                metrics.inc_leaves();
            }
        }

        ClientMessage::GetRouterRtpCapabilities
        | ClientMessage::CreateSendTransport
        | ClientMessage::CreateRecvTransport
        | ClientMessage::ReplaceRecvTransport { .. }
        | ClientMessage::ConnectTransport { .. }
        | ClientMessage::Produce { .. }
        | ClientMessage::Consume { .. }
        | ClientMessage::ResumeConsumer { .. }
        | ClientMessage::PauseConsumer { .. }
        | ClientMessage::CloseConsumer { .. }
        | ClientMessage::CloseProducer { .. }
        | ClientMessage::PauseProducer { .. }
        | ClientMessage::ResumeProducer { .. }
        | ClientMessage::RestartIce { .. }
        | ClientMessage::SetConsumerPreferredLayers { .. } => {
            media::handle(
                message,
                Dispatch {
                    participant_id,
                    current_room_id,
                    in_lobby,
                    reply,
                    room_manager,
                    turn_config,
                    reconnect_token,
                    bwe_sender,
                    is_authenticated,
                    client_ip,
                },
            )
            .await?;
        }
        ClientMessage::Reconnect { .. } | ClientMessage::RenewAuthentication { .. } => {
            // The connection loop answers these before dispatch; reaching this arm
            // is a dispatcher bug, so refuse the request instead of ignoring it.
            anyhow::bail!("Reconnect and renewal are answered before dispatch");
        }

        ClientMessage::ChatMessage { .. }
        | ClientMessage::PrivateMessage { .. }
        | ClientMessage::RetryChatMessage(_) => {
            let room_id = current_room_id
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("Not in a room"))?;
            room_manager
                .handle_chat_command(room_id, participant_id, sender, message)
                .await?;
        }
        ClientMessage::Typing {
            target_participant_id,
        } => {
            let room_id = current_room_id
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("Not in a room"))?;
            room_manager
                .relay_typing(
                    room_id,
                    participant_id,
                    sender,
                    target_participant_id.as_ref(),
                )
                .await?;
        }
        ClientMessage::SetChatPreferences { .. }
        | ClientMessage::ChangeNickname { .. }
        | ClientMessage::SetChatStyle { .. }
        | ClientMessage::ReactToMessage { .. }
        | ClientMessage::RemoveChatMessage { .. }
        | ClientMessage::EditChatMessage { .. }
        | ClientMessage::GetPinnedMessages { .. }
        | ClientMessage::SetPinnedMessage { .. }
        | ClientMessage::GetRoomSnapshot { .. }
        | ClientMessage::GetChatHistory { .. }
        | ClientMessage::MarkChatRead { .. }
        | ClientMessage::SetRoomHistory { .. }
        | ClientMessage::ListRoomBans { .. }
        | ClientMessage::RemoveRoomBan { .. }
        | ClientMessage::ListRoomMembers { .. }
        | ClientMessage::SetMemberRole { .. }
        | ClientMessage::ReportParticipant { .. }
        | ClientMessage::ListRoomReports { .. }
        | ClientMessage::ResolveRoomReport { .. }
        | ClientMessage::ListModerationEvents { .. } => {
            let room_id = current_room_id
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("Not in a room"))?;
            room_manager
                .handle_social_request(room_id, participant_id, sender, message)
                .await?;
        }

        // The socket owner handles grants with its current account claims.
        ClientMessage::GetAttachmentAccess { .. } => {
            anyhow::bail!("Attachment access requires the current socket credentials");
        }

        // === Moderation ===
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
        | ClientMessage::DenyFromLobby { .. } => {
            moderation::handle(
                message,
                Dispatch {
                    participant_id,
                    current_room_id,
                    in_lobby,
                    reply,
                    room_manager,
                    turn_config,
                    reconnect_token,
                    bwe_sender,
                    is_authenticated,
                    client_ip,
                },
            )
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
#[path = "connection_security_tests.rs"]
mod security_tests;

#[cfg(test)]
#[path = "connection_gates_tests.rs"]
mod gates_tests;
