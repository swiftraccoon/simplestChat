//! Every per-connection admission budget a frame and its decoded command pass
//! before dispatch: the frame rate, the media session's mutation buckets, the
//! administration and chat budgets, the room password hashing cooldown, join
//! attempts and voice requests. The loop asks for one verdict per frame and one
//! per command; the budgets' sizes and the reasons behind them live here.

pub(super) use super::token_bucket::{TOKEN_US, TokenBucket, ViolationWindow};
use super::*;

/// Token bucket rate limiter: max tokens (burst capacity).
pub(super) const RATE_LIMIT_MAX_TOKENS: u64 = 100;
/// Token bucket: refill rate in tokens per second.
pub(super) const RATE_LIMIT_REFILL_RATE: u64 = 100;
/// Internal: max tokens in microseconds.
pub(super) const MAX_TOKENS_US: u64 = RATE_LIMIT_MAX_TOKENS * TOKEN_US;
/// Repeated overruns indicate a sender that is ignoring backpressure. Stop
/// reading it instead of spending an unbounded amount of CPU discarding frames.
pub(super) const RATE_LIMIT_VIOLATION_WINDOW: Duration = Duration::from_secs(10);
pub(super) const RATE_LIMIT_MAX_VIOLATIONS: u32 = 3;

/// Chat is broadcast work, so it has a much smaller per-connection budget than
/// ordinary point-to-point signaling.
pub(super) const CHAT_RATE_LIMIT_MAX_TOKENS: u64 = 5;
pub(super) const CHAT_RATE_LIMIT_REFILL_RATE: u64 = 2;
pub(super) const MAX_CHAT_TOKENS_US: u64 = CHAT_RATE_LIMIT_MAX_TOKENS * TOKEN_US;

/// Producer mutations cross the mediasoup IPC boundary and may fan out to
/// every participant. Keep their budget well below ordinary signaling while
/// retaining enough burst capacity for microphone/camera/screen-share setup.
pub(super) const MEDIA_MUTATION_RATE_LIMIT_MAX_TOKENS: u64 = 8;
pub(super) const MEDIA_MUTATION_RATE_LIMIT_REFILL_RATE: u64 = 2;
pub(super) const MAX_MEDIA_MUTATION_TOKENS_US: u64 =
    MEDIA_MUTATION_RATE_LIMIT_MAX_TOKENS * TOKEN_US;
pub(super) const MEDIA_MUTATION_VIOLATION_WINDOW: Duration = Duration::from_secs(10);
pub(super) const MEDIA_MUTATION_MAX_VIOLATIONS: u32 = 3;

/// A media session needs two transport creations and two DTLS connects during
/// normal startup. Leave room for immediate ICE recovery, but prevent repeated
/// transport/ICE requests from turning one socket into an IPC work generator.
pub(super) const TRANSPORT_MUTATION_RATE_LIMIT_MAX_TOKENS: u64 = 6;
pub(super) const TRANSPORT_MUTATION_RATE_LIMIT_REFILL_RATE: u64 = 1;
pub(super) const MAX_TRANSPORT_MUTATION_TOKENS_US: u64 =
    TRANSPORT_MUTATION_RATE_LIMIT_MAX_TOKENS * TOKEN_US;

/// Permit create, resume and one layer selection for every configured consumer.
/// Keep sustained churn bounded independently of the initial join burst.
pub(super) const CONSUMER_MUTATION_RATE_LIMIT_REFILL_RATE: u64 = 4;

pub(super) fn consumer_mutation_capacity(consumer_limit: usize) -> u64 {
    u64::try_from(consumer_limit)
        .unwrap_or(u64::MAX)
        .saturating_mul(3 * TOKEN_US)
}

pub(super) fn frame_capacity(consumer_capacity: u64) -> u64 {
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
pub(super) const ADMIN_MUTATION_RATE_LIMIT_MAX_TOKENS: u64 = 20;
pub(super) const ADMIN_MUTATION_RATE_LIMIT_REFILL_RATE: u64 = 5;
pub(super) const MAX_ADMIN_MUTATION_TOKENS_US: u64 =
    ADMIN_MUTATION_RATE_LIMIT_MAX_TOKENS * TOKEN_US;
pub(super) const ADMIN_MUTATION_VIOLATION_WINDOW: Duration = Duration::from_secs(10);
pub(super) const ADMIN_MUTATION_MAX_VIOLATIONS: u32 = 3;

/// A voice request creates action UI for every moderator. It represents state
/// a human must act on, so sending it more often than this is never useful.
pub(super) const VOICE_REQUEST_COOLDOWN: Duration = Duration::from_secs(5);

/// Hashing a new room password is intentionally expensive. A connection may
/// request it only occasionally, even if it recreates ad-hoc rooms to evade a
/// per-room cooldown.
pub(super) const ROOM_PASSWORD_HASH_COOLDOWN: Duration = Duration::from_secs(30);

#[derive(Default)]
pub(super) struct JoinAttemptState {
    last_attempt: Option<Instant>,
    pub(super) password_challenge_room: Option<RoomId>,
}

impl JoinAttemptState {
    pub(super) fn allow(&mut self, room_id: &RoomId, has_password: bool, now: Instant) -> bool {
        // One response to an issued password challenge belongs to the same
        // interaction, even when a password manager answers immediately.
        // RoomManager's shared IP and room/IP budgets still count both requests.
        let answering_challenge = self
            .password_challenge_room
            .take()
            .is_some_and(|pending| pending == *room_id && has_password);
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

pub(super) fn is_media_mutation(message: &ClientMessage) -> bool {
    matches!(
        message,
        ClientMessage::Produce { .. }
            | ClientMessage::CloseProducer { .. }
            | ClientMessage::PauseProducer { .. }
            | ClientMessage::ResumeProducer { .. }
    )
}

pub(super) fn is_transport_mutation(message: &ClientMessage) -> bool {
    matches!(
        message,
        ClientMessage::CreateSendTransport
            | ClientMessage::CreateRecvTransport
            | ClientMessage::ReplaceRecvTransport { .. }
            | ClientMessage::ConnectTransport { .. }
            | ClientMessage::RestartIce { .. }
    )
}

pub(super) fn is_consumer_mutation(message: &ClientMessage) -> bool {
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
pub(super) struct MediaSessionRateState {
    pub(super) media_mutations: TokenBucket,
    pub(super) transport_mutations: TokenBucket,
    pub(super) consumer_mutations: TokenBucket,
    pub(super) violations: ViolationWindow,
}

impl MediaSessionRateState {
    /// A session that starts now; the connection builds its own at a known instant.
    #[cfg(test)]
    pub(super) fn new(consumer_limit: usize) -> Self {
        Self::with_consumer_limit(Instant::now(), consumer_limit)
    }

    pub(super) fn with_consumer_limit(now: Instant, consumer_limit: usize) -> Self {
        Self {
            media_mutations: TokenBucket::full(
                now,
                MEDIA_MUTATION_RATE_LIMIT_REFILL_RATE,
                MAX_MEDIA_MUTATION_TOKENS_US,
            ),
            transport_mutations: TokenBucket::full(
                now,
                TRANSPORT_MUTATION_RATE_LIMIT_REFILL_RATE,
                MAX_TRANSPORT_MUTATION_TOKENS_US,
            ),
            consumer_mutations: TokenBucket::full(
                now,
                CONSUMER_MUTATION_RATE_LIMIT_REFILL_RATE,
                consumer_mutation_capacity(consumer_limit),
            ),
            violations: ViolationWindow::default(),
        }
    }

    pub(super) fn limit_exceeded(&mut self, message: &ClientMessage, now: Instant) -> bool {
        if is_media_mutation(message) {
            !self.media_mutations.try_consume(now)
        } else if is_transport_mutation(message) {
            !self.transport_mutations.try_consume(now)
        } else if is_consumer_mutation(message) {
            !self.consumer_mutations.try_consume(now)
        } else {
            false
        }
    }

    pub(super) fn record_violation(&mut self, now: Instant) -> u32 {
        self.violations.record(now, MEDIA_MUTATION_VIOLATION_WINDOW)
    }
}

pub(super) fn is_admin_mutation(message: &ClientMessage) -> bool {
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

/// Why a client command closes its socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CloseReason {
    /// Media mutations kept arriving after this many refusals in one window.
    MediaMutations(u32),
    /// Administration changes kept arriving after this many refusals in one window.
    AdminMutations(u32),
    /// Chat exceeded its budget; a flood is closed at once.
    ChatFlood,
}

/// The verdict on one decoded client command.
pub(super) enum Admission {
    Allowed,
    /// Answer with this and read the next frame.
    Refused(ServerMessage),
    /// Answer with this and close the socket.
    Close {
        response: ServerMessage,
        why: CloseReason,
    },
}

/// The verdict on one inbound frame before it is decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FrameAdmission {
    Allowed,
    /// Refused as the `violations`th frame over budget in the current window.
    Refused {
        violations: u32,
    },
}

/// The budgets of one connection.
pub(super) struct CommandGates {
    frames: TokenBucket,
    frame_refusals: ViolationWindow,
    media: MediaSessionRateState,
    admin: TokenBucket,
    admin_refusals: ViolationWindow,
    chat: TokenBucket,
    last_room_password_hash: Option<Instant>,
    join_attempts: JoinAttemptState,
    last_voice_request: Option<Instant>,
}

impl CommandGates {
    pub(super) fn new(now: Instant, consumer_limit: usize) -> Self {
        Self {
            frames: TokenBucket::full(
                now,
                RATE_LIMIT_REFILL_RATE,
                frame_capacity(consumer_mutation_capacity(consumer_limit)),
            ),
            frame_refusals: ViolationWindow::default(),
            media: MediaSessionRateState::with_consumer_limit(now, consumer_limit),
            admin: TokenBucket::full(
                now,
                ADMIN_MUTATION_RATE_LIMIT_REFILL_RATE,
                MAX_ADMIN_MUTATION_TOKENS_US,
            ),
            admin_refusals: ViolationWindow::default(),
            chat: TokenBucket::full(now, CHAT_RATE_LIMIT_REFILL_RATE, MAX_CHAT_TOKENS_US),
            last_room_password_hash: None,
            join_attempts: JoinAttemptState::default(),
            last_voice_request: None,
        }
    }

    /// Charge one inbound frame of any kind to the connection's frame budget.
    pub(super) fn admit_frame(&mut self, now: Instant) -> FrameAdmission {
        if self.frames.try_consume(now) {
            FrameAdmission::Allowed
        } else {
            FrameAdmission::Refused {
                violations: self.frame_refusals.record(now, RATE_LIMIT_VIOLATION_WINDOW),
            }
        }
    }

    /// Charge a decoded command to the budgets it belongs to; the most
    /// expensive work is refused first.
    pub(super) fn admit(&mut self, message: &ClientMessage, now: Instant) -> Admission {
        if self.media.limit_exceeded(message, now) {
            let violations = self.media.record_violation(now);
            let response = rate_limited("Media changes are rate limited");
            return if violations >= MEDIA_MUTATION_MAX_VIOLATIONS {
                Admission::Close {
                    response,
                    why: CloseReason::MediaMutations(violations),
                }
            } else {
                Admission::Refused(response)
            };
        }
        if is_admin_mutation(message) && !self.admin.try_consume(now) {
            let violations = self
                .admin_refusals
                .record(now, ADMIN_MUTATION_VIOLATION_WINDOW);
            let response =
                social_rate_limited(message, "Room administration changes are rate limited");
            return if violations >= ADMIN_MUTATION_MAX_VIOLATIONS {
                Admission::Close {
                    response,
                    why: CloseReason::AdminMutations(violations),
                }
            } else {
                Admission::Refused(response)
            };
        }
        if matches!(
            message,
            ClientMessage::ChatMessage { .. }
                | ClientMessage::PrivateMessage { .. }
                | ClientMessage::RetryChatMessage(_)
        ) && !self.chat.try_consume(now)
        {
            return Admission::Close {
                response: social_rate_limited(message, "Chat rate limit exceeded"),
                why: CloseReason::ChatFlood,
            };
        }
        if matches!(
            message,
            ClientMessage::UpdateRoomSettings {
                patch: SettingsPatch {
                    password: Some(Some(_)),
                    ..
                }
            }
        ) {
            if self
                .last_room_password_hash
                .is_some_and(|previous| now.duration_since(previous) < ROOM_PASSWORD_HASH_COOLDOWN)
            {
                return Admission::Refused(rate_limited("Room password updates are rate limited"));
            }
            self.last_room_password_hash = Some(now);
        }
        if let ClientMessage::JoinRoom {
            room_id, password, ..
        } = message
            && !self.join_attempts.allow(room_id, password.is_some(), now)
        {
            return Admission::Refused(rate_limited("Join attempts are rate limited"));
        }
        if matches!(message, ClientMessage::RequestVoice) {
            if self
                .last_voice_request
                .is_some_and(|previous| now.duration_since(previous) < VOICE_REQUEST_COOLDOWN)
            {
                return Admission::Refused(rate_limited("Voice requests are rate limited"));
            }
            self.last_voice_request = Some(now);
        }
        Admission::Allowed
    }

    /// A join that was answered with a password challenge lets one reply
    /// through at once.
    pub(super) fn note_password_challenge(&mut self, room_id: RoomId) {
        self.join_attempts.password_challenge_room = Some(room_id);
    }

    /// A fresh media session (a successful join) starts with a full budget.
    pub(super) fn reset_media(&mut self, now: Instant, consumer_limit: usize) {
        self.media = MediaSessionRateState::with_consumer_limit(now, consumer_limit);
    }

    /// A reconnect continues the session's spent budget.
    pub(super) fn restore_media(&mut self, media: MediaSessionRateState) {
        self.media = media;
    }

    /// The media session's budget, retained for a reconnect.
    pub(super) fn into_media_state(self) -> MediaSessionRateState {
        self.media
    }
}

fn rate_limited(message: &str) -> ServerMessage {
    ServerMessage::Error {
        code: ErrorCode::RateLimited,
        message: message.to_string(),
    }
}

/// The social form of a refusal when the command has one, else the plain error.
fn social_rate_limited(message: &ClientMessage, text: &str) -> ServerMessage {
    message
        .social_error(ErrorCode::RateLimited, text)
        .unwrap_or_else(|| rate_limited(text))
}
