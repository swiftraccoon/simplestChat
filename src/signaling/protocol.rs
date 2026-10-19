#![forbid(unsafe_code)]

// Signaling protocol - Message types for WebSocket communication

use crate::secret::SecretString;
use crate::turn::IceServer;
use chrono::{DateTime, Utc};

use crate::ids::{MessageId, ParticipantId, RoomId, UserId};
use crate::room::moderation::{ReportResolution, ReportStatus};
use crate::room::settings::{RoomSettings, SettingsPatch};
use mediasoup::prelude::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[cfg(test)]
#[path = "protocol_property_tests.rs"]
mod property_tests;

/// IDs are opaque correlation tokens, not credentials or idempotency keys.
pub(crate) fn valid_correlation_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

/// Read the envelope independently so a malformed command can still receive a
/// correlated error. Serde skips the payload without allocating a JSON tree;
/// the subsequent typed decode retains duplicate-field and payload validation.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RequestHeader {
    #[serde(default, deserialize_with = "deserialize_request_id")]
    pub request_id: Option<String>,
}

fn deserialize_request_id<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = String::deserialize(deserializer)?;
    if !valid_correlation_id(&value) {
        return Err(serde::de::Error::custom("Invalid request ID"));
    }
    Ok(Some(value))
}

/// Direct responses echo the request envelope. Broadcasts have no request ID.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ServerReply<'a> {
    #[serde(flatten)]
    pub message: &'a ServerMessage,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<&'a str>,
}

/// A signaling bearer is serialized only for transport, never for diagnostics.
/// Roles travel by name; see [`Role`].
pub use crate::room::roles::Role;

/// Why the server refused a frame or request. Clients switch on the code;
/// `message` beside it is for people and may change at any time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// The frame could not be parsed or named an unknown operation.
    InvalidMessage,
    /// A field was missing, malformed, out of range or already used.
    Invalid,
    /// The caller may not perform this action, or the room's state forbids it.
    Forbidden,
    /// The target does not exist or is no longer available.
    NotFound,
    /// Too many requests; retry after a pause.
    RateLimited,
    /// A dependency the request needs is not available; retry later.
    Unavailable,
    /// The operation did not finish in time; it may or may not have applied.
    TimedOut,
    /// The server could not complete the request.
    RequestFailed,
}

impl ErrorCode {
    /// The wire name.
    pub fn name(self) -> &'static str {
        match self {
            Self::InvalidMessage => "invalid_message",
            Self::Invalid => "invalid",
            Self::Forbidden => "forbidden",
            Self::NotFound => "not_found",
            Self::RateLimited => "rate_limited",
            Self::Unavailable => "unavailable",
            Self::TimedOut => "timed_out",
            Self::RequestFailed => "request_failed",
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AuthenticationToken(SecretString);

impl AuthenticationToken {
    pub(super) fn expose(&self) -> &str {
        self.0.expose_secret()
    }
}

impl std::fmt::Debug for AuthenticationToken {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("AuthenticationToken([REDACTED])")
    }
}

/// Client-to-Server messages
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ClientMessage {
    /// Renew this authenticated socket without replacing its room membership.
    RenewAuthentication {
        request_id: String,
        token: AuthenticationToken,
    },
    /// Join a room
    JoinRoom {
        room_id: RoomId,
        participant_name: String,
        /// Required for password-protected rooms (Admin+ bypass)
        #[serde(default)]
        password: Option<SecretString>,
        /// A guest's chosen look; an account's saved one takes its place.
        #[serde(default)]
        chat_style: Option<ChatStyle>,
    },
    /// Leave the current room
    LeaveRoom,
    /// Get RTP capabilities from server
    GetRouterRtpCapabilities,
    /// Create WebRTC send transport
    CreateSendTransport,
    /// Create WebRTC receive transport
    CreateRecvTransport,
    /// Replace only this member's expected receive transport; retries replay its replacement.
    ReplaceRecvTransport {
        transport_id: String,
    },
    /// Connect transport with DTLS parameters
    ConnectTransport {
        transport_id: String,
        dtls_parameters: DtlsParameters,
    },
    /// Produce media (audio/video)
    Produce {
        transport_id: String,
        kind: MediaKind,
        rtp_parameters: RtpParameters,
        #[serde(default)]
        source: Option<String>, // "camera", "microphone", "screen", "screen-audio"
    },
    /// Consume media from another participant
    Consume {
        producer_id: String,
        rtp_capabilities: RtpCapabilities,
    },
    /// Resume a consumer
    ResumeConsumer {
        consumer_id: String,
    },
    /// Pause a consumer
    PauseConsumer {
        consumer_id: String,
    },
    /// Release this receiver's consumer; repeated closure is a no-op.
    CloseConsumer {
        consumer_id: String,
    },
    /// Close a producer
    CloseProducer {
        producer_id: String,
    },
    /// Pause a producer (mute)
    PauseProducer {
        producer_id: String,
    },
    /// Resume a producer (unmute)
    ResumeProducer {
        producer_id: String,
    },
    /// Reconnect to an existing session after WS disconnect
    Reconnect {
        participant_id: ParticipantId,
        room_id: RoomId,
        reconnect_token: SecretString,
    },
    /// Request ICE restart on a transport
    RestartIce {
        transport_id: String,
    },
    /// Set preferred simulcast layers for a consumer
    SetConsumerPreferredLayers {
        consumer_id: String,
        spatial_layer: u8,
        temporal_layer: Option<u8>,
    },
    /// Send a chat message to the room
    ChatMessage {
        content: String,
        #[serde(default)]
        attachment_ids: Vec<uuid::Uuid>,
        #[serde(default)]
        client_message_id: Option<String>,
        #[serde(default)]
        sequence: Option<u64>,
        /// The retained message this one answers, in the same conversation.
        #[serde(default)]
        reply_to: Option<MessageId>,
    },
    PrivateMessage {
        target_participant_id: ParticipantId,
        content: String,
        #[serde(default)]
        attachment_ids: Vec<uuid::Uuid>,
        client_message_id: String,
        #[serde(default)]
        sequence: Option<u64>,
        #[serde(default)]
        reply_to: Option<MessageId>,
    },
    RetryChatMessage(RetryChatMessage),
    GetAttachmentAccess {
        request_id: String,
        attachment_id: uuid::Uuid,
    },
    GetChatHistory {
        request_id: String,
        before: Option<String>,
        after: Option<String>,
        around: Option<MessageId>,
        resume: Option<bool>,
        q: Option<String>,
        limit: Option<u32>,
    },
    MarkChatRead {
        request_id: String,
        message_id: MessageId,
    },
    SetRoomHistory {
        request_id: String,
        retention_days: i32,
    },
    SetChatPreferences {
        request_id: String,
        allow_private_messages: bool,
        ignored_participant_ids: Vec<ParticipantId>,
    },
    /// Change how this participant's name and messages look to everyone.
    SetChatStyle {
        request_id: String,
        chat_style: ChatStyle,
    },
    /// Edit an authored message without overwriting a newer revision.
    EditChatMessage {
        request_id: String,
        message_id: MessageId,
        content: String,
        expected_revision: u32,
    },
    GetPinnedMessages {
        request_id: String,
    },
    SetPinnedMessage {
        request_id: String,
        message_id: MessageId,
        pinned: bool,
    },
    RemoveChatMessage {
        request_id: String,
        message_id: MessageId,
    },
    /// Add this participant's reaction to a retained message, or take it back.
    ReactToMessage {
        request_id: String,
        message_id: MessageId,
        emoji: String,
    },
    ChangeNickname {
        request_id: String,
        nickname: String,
    },
    GetRoomSnapshot {
        request_id: String,
    },
    ListRoomBans {
        request_id: String,
        #[serde(default)]
        offset: Option<u32>,
    },
    RemoveRoomBan {
        request_id: String,
        ban_id: String,
    },
    ListRoomMembers {
        request_id: String,
        #[serde(default)]
        offset: Option<u32>,
    },
    SetMemberRole {
        request_id: String,
        target_user_id: UserId,
        role: Role,
    },
    ReportParticipant {
        request_id: String,
        target_participant_id: ParticipantId,
        reason: String,
    },
    ListRoomReports {
        request_id: String,
        #[serde(default)]
        offset: Option<u32>,
    },
    ResolveRoomReport {
        request_id: String,
        report_id: String,
        status: ReportResolution,
    },
    /// The room's moderation history, newest first (Moderator+; the owner also
    /// sees each target's address).
    ListModerationEvents {
        request_id: String,
        #[serde(default)]
        offset: Option<u32>,
    },

    /// The sender is composing a message, for a private conversation when
    /// `target_participant_id` is set; relayed to those who would receive it.
    Typing {
        #[serde(default)]
        target_participant_id: Option<ParticipantId>,
    },

    // === Moderation ===
    /// Force-close a participant's camera/screen producer
    CloseCam {
        target_participant_id: ParticipantId,
    },
    /// Ban a participant from producing video/screen
    CamBan {
        target_participant_id: ParticipantId,
        reason: Option<String>,
    },
    /// Unban a participant from producing video/screen
    CamUnban {
        target_participant_id: ParticipantId,
    },
    /// Mute a participant's text chat
    TextMute {
        target_participant_id: ParticipantId,
    },
    /// Unmute a participant's text chat
    TextUnmute {
        target_participant_id: ParticipantId,
    },
    /// Kick a participant from the room; `report_id` names the open report
    /// this answers, which is resolved with it.
    Kick {
        target_participant_id: ParticipantId,
        reason: Option<String>,
        #[serde(default)]
        report_id: Option<String>,
    },
    /// Ban a participant from the room; `report_id` as for `Kick`.
    Ban {
        target_participant_id: ParticipantId,
        reason: Option<String>,
        duration: Option<u64>,
        #[serde(default)]
        report_id: Option<String>,
    },
    /// Unban a user (stub — no persistent ban list yet)
    Unban {
        target_user_id: UserId,
    },
    /// Set a participant's role
    SetRole {
        target_participant_id: ParticipantId,
        role: Role,
    },
    /// Request voice in a moderated room (sent by User/Guest)
    RequestVoice,

    // === Room management ===
    /// Update room settings (Admin+). The patch's fields sit beside `type`; the
    /// envelope's other keys (`requestId`) are ignored as on every command, and
    /// only the HTTP route, which reads `SettingsPatch` directly, rejects unknown
    /// fields.
    UpdateRoomSettings {
        #[serde(flatten)]
        patch: SettingsPatch,
    },
    /// Set the room topic
    SetTopic {
        topic: String,
    },

    // === Lobby ===
    /// Admit a participant from the lobby
    AdmitFromLobby {
        target_participant_id: ParticipantId,
    },
    /// Deny a participant from the lobby
    DenyFromLobby {
        target_participant_id: ParticipantId,
    },
}

/// Server-to-Client messages
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ServerMessage {
    /// Room joined successfully
    RoomJoined {
        participant_id: ParticipantId,
        participants: Vec<ParticipantInfo>,
        reconnect_token: SecretString,
        your_role: Role,
        /// The name the room knows this participant by; a guest's may carry a number.
        your_name: String,
        /// This participant's look: an account's saved one, or the guest's.
        #[serde(default)]
        your_chat_style: ChatStyle,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        room_settings: Option<RoomSettings>,
    },
    /// The existing socket accepted a same-identity credential renewal.
    AuthenticationRenewed {
        request_id: String,
        expires_at: u64,
    },
    /// A renewal did not replace or extend the existing socket credential.
    AuthenticationRenewalFailed {
        request_id: String,
    },
    /// Temporary validation failure; the existing credential remains unchanged.
    AuthenticationRenewalDeferred {
        request_id: String,
        retry_after_ms: u32,
        expires_at: u64,
    },
    /// Error response
    /// A request or frame the server refused; `code` is what clients act on.
    Error {
        code: ErrorCode,
        message: String,
    },
    /// The client should prompt for a password and retry this room join.
    RoomPasswordRequired,
    /// Terminal room lifecycle event, distinct from recoverable request errors.
    RoomClosed {
        reason: String,
    },
    /// Temporary process shutdown; clients may retain room intent and rejoin.
    /// Existing media transports do not survive the replacement process.
    ServerRestarting {
        reason: String,
    },
    /// Router RTP capabilities
    RouterRtpCapabilities {
        rtp_capabilities: RtpCapabilitiesFinalized,
    },
    /// Transport created
    TransportCreated {
        transport_id: String,
        ice_parameters: IceParameters,
        ice_candidates: Vec<IceCandidate>,
        dtls_parameters: DtlsParameters,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        ice_servers: Vec<IceServer>,
    },
    /// Transport connected
    TransportConnected {
        transport_id: String,
    },
    /// Producer created
    ProducerCreated {
        producer_id: String,
    },
    /// Consumer created
    ConsumerCreated {
        consumer_id: String,
        producer_id: String,
        kind: MediaKind,
        rtp_parameters: RtpParameters,
    },
    /// New participant joined the room
    ParticipantJoined {
        participant_id: ParticipantId,
        participant_name: String,
        role: Role,
        authenticated: bool,
        #[serde(default)]
        chat_style: ChatStyle,
    },
    /// Participant left the room
    ParticipantLeft {
        participant_id: ParticipantId,
    },
    /// New producer available from another participant
    NewProducer {
        participant_id: ParticipantId,
        producer_id: String,
        kind: MediaKind,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source: Option<String>,
    },
    /// Producer closed by another participant
    ProducerClosed {
        producer_id: String,
    },
    /// Producer paused (muted) by its owner
    ProducerPaused {
        producer_id: String,
    },
    /// Producer resumed (unmuted) by its owner
    ProducerResumed {
        producer_id: String,
    },
    /// Consumer resumed
    ConsumerResumed {
        consumer_id: String,
    },
    /// Consumer paused
    ConsumerPaused {
        consumer_id: String,
    },
    /// A correlated closure or preferred-layer command completed successfully.
    /// Sent only inside a reply envelope with the caller's request ID.
    MediaControlApplied,
    /// A room mutation completed. The request ID identifies only this command;
    /// state broadcasts remain independent and a timeout is not a rollback.
    RoomControlApplied,
    /// Result of reconnection attempt
    ReconnectResult {
        success: bool,
        participant_id: ParticipantId,
        #[serde(skip_serializing_if = "Option::is_none")]
        reconnect_token: Option<SecretString>,
    },
    /// ICE restarted — new ICE parameters plus fresh TURN credentials, since
    /// the ones minted at transport creation expire after `TURN_TTL`.
    IceRestarted {
        transport_id: String,
        ice_parameters: IceParameters,
        ice_servers: Vec<IceServer>,
    },
    /// Connection quality stats
    ConnectionStats {
        available_bitrate: Option<u32>,
        rtt: Option<f64>,
    },
    /// Consumer simulcast layers changed
    ConsumerLayersChanged {
        consumer_id: String,
        spatial_layer: Option<u8>,
        temporal_layer: Option<u8>,
    },
    /// Chat message received from another participant
    ChatReceived {
        participant_id: ParticipantId,
        participant_name: String,
        content: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<ChatAttachment>,
        message_id: MessageId,
        client_message_id: String,
        sent_at: DateTime<Utc>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        removed_at: Option<DateTime<Utc>>,
        #[serde(default)]
        revision: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        edited_at: Option<DateTime<Utc>>,
        #[serde(default)]
        chat_style: ChatStyle,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reply_to: Option<ChatReplyRef>,
    },
    MessageAck {
        client_message_id: String,
        message: ChatEntry,
    },
    MessageRetryResult {
        client_message_id: String,
        outcome: ChatRetryOutcome,
        reason: ChatRetryReason,
    },
    PrivateMessageReceived {
        message: ChatEntry,
    },
    /// The reply to a social request: `action` and `data` come from [`SocialResponse`].
    SocialResponse {
        request_id: String,
        #[serde(flatten)]
        response: SocialResponse,
    },
    SocialError {
        #[serde(skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        client_message_id: Option<String>,
        code: ErrorCode,
        message: String,
    },
    NicknameChanged {
        participant_id: ParticipantId,
        nickname: String,
    },
    ChatStyleChanged {
        participant_id: ParticipantId,
        chat_style: ChatStyle,
    },
    /// A participant is composing a message; private when `target_participant_id`
    /// is set (then only that target hears it).
    ParticipantTyping {
        participant_id: ParticipantId,
        #[serde(skip_serializing_if = "Option::is_none")]
        target_participant_id: Option<ParticipantId>,
    },
    ChatMessageEdited {
        message: ChatEntry,
    },
    PinnedMessagesChanged {
        messages: Vec<ChatEntry>,
    },
    /// A public message and any quotes of it must be replaced by a tombstone.
    ChatMessageRemoved {
        message_id: MessageId,
        removed_at: DateTime<Utc>,
    },
    /// A message's reactions changed; sent to everyone who can see it.
    MessageReactions {
        message_id: MessageId,
        reactions: Vec<ChatReaction>,
    },
    /// Active/dominant speaker changed
    ActiveSpeaker {
        participant_id: ParticipantId,
    },
    /// Audio levels for all speaking participants; an empty list means nobody
    /// exceeded the threshold during the last interval
    AudioLevels {
        levels: Vec<AudioLevelEntry>,
    },

    // === Moderation broadcasts ===
    /// A producer was force-closed by a moderator
    ForceClosedProducer {
        producer_id: String,
        reason: String,
    },
    /// Participant's camera was banned
    CamBanned {
        participant_id: ParticipantId,
    },
    /// Participant's camera ban was lifted
    CamUnbanned {
        participant_id: ParticipantId,
    },
    /// Participant was text-muted
    TextMuted {
        participant_id: ParticipantId,
    },
    /// Participant was text-unmuted
    TextUnmuted {
        participant_id: ParticipantId,
    },
    /// Participant was kicked from the room
    ParticipantKicked {
        participant_id: ParticipantId,
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// Participant was banned from the room
    ParticipantBanned {
        participant_id: ParticipantId,
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// Participant's role was changed
    RoleChanged {
        participant_id: ParticipantId,
        new_role: Role,
        granted_by: String,
    },
    /// A participant requested voice (sent to Moderator+)
    VoiceRequested {
        participant_id: ParticipantId,
        display_name: String,
    },

    // === Room state ===
    /// Room settings were changed
    RoomSettingsChanged {
        settings: RoomSettings,
    },
    /// Room topic was changed
    TopicChanged {
        topic: String,
        changed_by: String,
    },

    // === Lobby ===
    /// Client is waiting in the lobby
    LobbyWaiting {
        room_name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        topic: Option<String>,
        participant_count: u32,
        moderator_count: u32,
    },
    LobbyStatus {
        participant_count: u32,
        moderator_count: u32,
    },
    /// A participant joined the lobby (sent to Moderator+)
    LobbyJoin {
        participant_id: ParticipantId,
        display_name: String,
        authenticated: bool,
    },
    /// Lobby admission was denied
    LobbyDenied {
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// Client was admitted from the lobby
    LobbyAdmitted,
}

/// Audio level entry for a speaking participant
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioLevelEntry {
    pub participant_id: ParticipantId,
    pub volume: i8, // dBov (0 = loudest, -127 = silence)
}

/// Participant information for room state
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParticipantInfo {
    pub id: ParticipantId,
    pub name: String,
    pub producers: Vec<ProducerMetadata>,
    pub role: Role,
    #[serde(default)]
    pub authenticated: bool,
    #[serde(default)]
    pub chat_style: ChatStyle,
}

/// The palette chat colors come from. Clients map each name to a color that
/// stays readable on the dark chat; anything else is refused.
pub const CHAT_COLORS: [&str; 16] = [
    "rose", "red", "orange", "amber", "lime", "green", "emerald", "teal", "cyan", "sky", "blue",
    "indigo", "violet", "purple", "fuchsia", "pink",
];

/// How a participant's color shows: a stripe beside their messages, their
/// message text, or their whole bubble.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChatStyleKind {
    #[default]
    Accent,
    Text,
    Bubble,
    /// A treatment a newer client knows and this server does not.
    #[serde(other)]
    Unknown,
}

impl ChatStyleKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Accent | Self::Unknown => "accent",
            Self::Text => "text",
            Self::Bubble => "bubble",
        }
    }
}

/// A participant's chosen look. No color means the automatic one every client
/// derives from the participant's name.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatStyle {
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub style: ChatStyleKind,
}

impl ChatStyle {
    /// The style with a known treatment (an unknown one shows as the accent),
    /// or `None` when its color is outside the palette.
    #[must_use]
    pub fn validated(&self) -> Option<Self> {
        if self
            .color
            .as_deref()
            .is_some_and(|color| !CHAT_COLORS.contains(&color))
        {
            return None;
        }
        let style = match self.style {
            ChatStyleKind::Unknown => ChatStyleKind::Accent,
            known => known,
        };
        Some(Self {
            color: self.color.clone(),
            style,
        })
    }
}

/// Parse an independently stored profile or room appearance. Unlike chat messages,
/// new appearance settings must name a known palette color and treatment.
pub fn deserialize_appearance_style<'de, D>(deserializer: D) -> Result<ChatStyle, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Appearance {
        color: Option<String>,
        style: ChatStyleKind,
    }
    let appearance = Appearance::deserialize(deserializer)?;
    let style = ChatStyle {
        color: appearance.color,
        style: appearance.style,
    };
    if style.style == ChatStyleKind::Unknown || style.validated().is_none() {
        return Err(serde::de::Error::custom(
            "Unknown appearance color or treatment",
        ));
    }
    Ok(style)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatEntry {
    pub message_id: MessageId,
    pub client_message_id: String,
    pub participant_id: ParticipantId,
    pub participant_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recipient_id: Option<ParticipantId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recipient_name: Option<String>,
    /// True only for a retained conversation between two accounts, across rooms.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_conversation: Option<bool>,
    pub content: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<ChatAttachment>,
    pub sent_at: DateTime<Utc>,
    /// Moderator removal erases content while retaining its place in the conversation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub removed_at: Option<DateTime<Utc>>,
    /// Monotonic body revision; removal remains terminal regardless of revision.
    #[serde(default)]
    pub revision: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edited_at: Option<DateTime<Utc>>,
    /// The sender's look when they sent it, so history keeps it after they leave.
    #[serde(default)]
    pub chat_style: ChatStyle,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<ChatReplyRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reactions: Vec<ChatReaction>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct ChatAttachment {
    pub id: uuid::Uuid,
    pub name: String,
    pub content_type: String,
    pub size: i64,
}

/// The reactions people may add to a message.
pub const REACTIONS: [&str; 8] = ["👍", "❤️", "😂", "😮", "😢", "🎉", "🔥", "👏"];

/// The message a reply answers, quoted by the server when the reply was sent, so
/// the quote survives the original leaving history and cannot be forged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatReplyRef {
    pub message_id: MessageId,
    pub participant_id: ParticipantId,
    pub participant_name: String,
    pub excerpt: String,
}

/// One reaction on a message and who added it, oldest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatReaction {
    pub emoji: String,
    pub participant_ids: Vec<ParticipantId>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatRetryOutcome {
    Unknown,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatRetryReason {
    SessionChanged,
    ReceiptExpired,
    SequenceSuperseded,
    Capacity,
    Conflict,
    RecipientUnconfirmed,
    StorageUnconfirmed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RetryChatMessage {
    pub client_message_id: String,
    pub sequence: u64,
    pub chat_session_id: String,
    pub content: String,
    #[serde(default)]
    pub attachment_ids: Vec<uuid::Uuid>,
    pub target_participant_id: Option<ParticipantId>,
    #[serde(default)]
    pub reply_to: Option<MessageId>,
}

/// Producer metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProducerMetadata {
    pub id: String,
    pub kind: MediaKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

/// The reply to one social request, serialized beside the request id as
/// `action` (the variant name) and `data` (the variant's fields).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "action",
    content = "data",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum SocialResponse {
    SetChatPreferences {
        allow_private_messages: bool,
        ignored_participant_ids: Vec<ParticipantId>,
    },
    ChangeNickname {
        nickname: String,
    },
    SetChatStyle {
        chat_style: ChatStyle,
    },
    RemoveChatMessage {
        message_id: MessageId,
        removed_at: DateTime<Utc>,
    },
    EditChatMessage {
        message: ChatEntry,
    },
    GetAttachmentAccess {
        token: String,
        expires_at: DateTime<Utc>,
    },
    GetPinnedMessages {
        messages: Vec<ChatEntry>,
    },
    SetPinnedMessage {
        messages: Vec<ChatEntry>,
    },
    ReactToMessage {
        message_id: MessageId,
        reactions: Vec<ChatReaction>,
    },
    GetRoomSnapshot(RoomSnapshot),
    GetChatHistory(crate::room::history::HistoryPage),
    MarkChatRead {
        read_message_id: Option<MessageId>,
    },
    SetRoomHistory {
        retention_days: i32,
    },
    ListRoomBans {
        bans: Vec<BanEntry>,
        has_more: bool,
    },
    RemoveRoomBan {
        removed: bool,
    },
    ListRoomMembers {
        members: Vec<MemberEntry>,
        has_more: bool,
    },
    SetMemberRole {
        updated: bool,
    },
    ReportParticipant {
        report_id: String,
        status: ReportStatus,
    },
    ListRoomReports {
        reports: Vec<ReportEntry>,
        has_more: bool,
    },
    ResolveRoomReport {
        report_id: String,
        status: ReportResolution,
    },
    ListModerationEvents {
        events: Vec<ModerationEventView>,
        has_more: bool,
    },
}

/// One consistent view of a room for a joining or resuming participant,
/// captured under the room lock and serialized after releasing it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoomSnapshot {
    pub participants: Vec<ParticipantInfo>,
    pub messages: Vec<ChatEntry>,
    pub lobby: Vec<SnapshotLobbyEntry>,
    pub your_role: Role,
    pub chat_session_id: Uuid,
    pub room_settings: Option<RoomSettings>,
    pub nickname: String,
    pub allow_private_messages: bool,
    pub ignored_participant_ids: Vec<ParticipantId>,
    pub paused_producer_ids: Vec<String>,
    pub text_muted: bool,
    pub cam_banned: bool,
    pub can_chat: bool,
    pub local_producer_ids: Vec<String>,
    pub can_broadcast: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotLobbyEntry {
    pub participant_id: ParticipantId,
    pub display_name: String,
    pub authenticated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BanEntry {
    pub ban_id: String,
    pub display_name: String,
    pub authenticated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReportEntry {
    pub report_id: String,
    pub reporter_id: ParticipantId,
    pub reporter_name: String,
    pub target_participant_id: ParticipantId,
    pub target_name: String,
    pub reason: String,
    pub status: ReportStatus,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_at: Option<String>,
    /// The newest history entry that answered this report, if any did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<ReportOutcome>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReportOutcome {
    pub action: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemberEntry {
    pub user_id: UserId,
    pub display_name: String,
    pub role: Role,
    pub online: bool,
    pub authenticated: bool,
}

/// A moderation history entry as a viewer receives it; only an owner's view
/// carries the target's address.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModerationEventView {
    #[serde(flatten)]
    pub event: crate::room::moderation::ModerationEvent,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_ip: Option<String>,
}

impl ModerationEventView {
    /// The entry for one viewer: owners see the target's address.
    pub(crate) fn new(event: crate::room::moderation::ModerationEvent, owner: bool) -> Self {
        let target_ip = owner
            .then(|| event.target_ip.map(|ip| ip.to_string()))
            .flatten();
        Self { event, target_ip }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CHAT_COLORS, ChatEntry, ChatReaction, ChatReplyRef, ChatStyle, ChatStyleKind,
        ClientMessage, REACTIONS, RequestHeader, ServerMessage,
    };
    use crate::ids::fixtures::{mid, pid};
    use serde_json::{Value, json};

    #[test]
    fn replies_and_reactions_keep_their_wire_names() {
        let react: ClientMessage = serde_json::from_value(json!({
            "type": "reactToMessage", "requestId": "r1", "messageId": mid("m1").to_string(), "emoji": "🎉"
        }))
        .unwrap();
        assert_eq!(react.social_request(), Some(("r1", "reactToMessage")));
        let chat: ClientMessage = serde_json::from_value(json!({
            "type": "chatMessage", "content": "hi", "clientMessageId": "c1", "replyTo": mid("m1").to_string()
        }))
        .unwrap();
        assert!(
            matches!(chat, ClientMessage::ChatMessage { reply_to: Some(id), .. } if id == mid("m1"))
        );
        let event = serde_json::to_value(ServerMessage::MessageReactions {
            message_id: mid("m1"),
            reactions: vec![ChatReaction {
                emoji: "🎉".into(),
                participant_ids: vec![pid("p")],
            }],
        })
        .unwrap();
        assert_eq!(
            event,
            json!({"type": "messageReactions", "messageId": mid("m1").to_string(),
                   "reactions": [{"emoji": "🎉", "participantIds": [pid("p").to_string()]}]})
        );
        let mut entry: ChatEntry = serde_json::from_value(json!({
            "messageId": mid("m2").to_string(), "clientMessageId": "c2", "participantId": pid("p").to_string(),
            "participantName": "P", "content": "hi", "sentAt": "2026-09-28T00:00:00Z"
        }))
        .unwrap();
        let bare = serde_json::to_value(&entry).unwrap();
        assert!(bare.get("replyTo").is_none() && bare.get("reactions").is_none());
        entry.reply_to = Some(ChatReplyRef {
            message_id: mid("m1"),
            participant_id: pid("q"),
            participant_name: "Q".into(),
            excerpt: "hello".into(),
        });
        assert_eq!(
            serde_json::to_value(&entry).unwrap()["replyTo"],
            json!({"messageId": mid("m1").to_string(), "participantId": pid("q").to_string(), "participantName": "Q", "excerpt": "hello"})
        );
        assert_eq!(REACTIONS.len(), 8);
    }

    #[test]
    fn chat_styles_keep_to_the_palette_and_tolerate_newer_treatments() {
        let style: ChatStyle =
            serde_json::from_value(json!({"color":"rose","style":"bubble"})).unwrap();
        assert_eq!(style.validated(), Some(style.clone()));
        assert_eq!(
            serde_json::to_value(&style).unwrap(),
            json!({"color":"rose","style":"bubble"})
        );
        let automatic: ChatStyle = serde_json::from_value(json!({})).unwrap();
        assert_eq!(
            (automatic.color.as_deref(), automatic.style),
            (None, ChatStyleKind::Accent)
        );
        let newer: ChatStyle =
            serde_json::from_value(json!({"color":"sky","style":"neon"})).unwrap();
        assert_eq!(newer.validated().unwrap().style, ChatStyleKind::Accent);
        for outside in ["#ff0000", "Rose", "red; background:url(x)", ""] {
            let style = ChatStyle {
                color: Some(outside.into()),
                style: ChatStyleKind::Text,
            };
            assert_eq!(style.validated(), None, "{outside}");
        }
        assert_eq!(CHAT_COLORS.len(), 16);
    }

    #[test]
    fn chat_styles_travel_with_joins_changes_and_broadcasts() {
        let join: ClientMessage = serde_json::from_value(json!({
            "type":"joinRoom","roomId":"r","participantName":"Alice",
            "chatStyle":{"color":"teal","style":"text"}
        }))
        .unwrap();
        assert!(
            matches!(join, ClientMessage::JoinRoom { chat_style: Some(ref s), .. } if s.color.as_deref() == Some("teal"))
        );
        let default_style: ClientMessage =
            serde_json::from_value(json!({"type":"joinRoom","roomId":"r","participantName":"Bob"}))
                .unwrap();
        assert!(matches!(
            default_style,
            ClientMessage::JoinRoom {
                chat_style: None,
                ..
            }
        ));
        let change: ClientMessage = serde_json::from_value(json!({
            "type":"setChatStyle","requestId":"r1","chatStyle":{"color":null,"style":"accent"}
        }))
        .unwrap();
        assert_eq!(change.social_request(), Some(("r1", "setChatStyle")));
        let changed = ServerMessage::ChatStyleChanged {
            participant_id: pid("p"),
            chat_style: ChatStyle {
                color: Some("sky".into()),
                style: ChatStyleKind::Accent,
            },
        };
        assert_eq!(
            serde_json::to_value(&changed).unwrap(),
            json!({"type":"chatStyleChanged","participantId": pid("p").to_string(),"chatStyle":{"color":"sky","style":"accent"}})
        );
    }

    #[test]
    fn request_headers_preserve_ids_without_weakening_payload_validation() {
        for request_id in ["request-1", "A_0-z", &"x".repeat(64)] {
            let wire = json!({"type": "consume", "requestId": request_id}).to_string();
            let header: RequestHeader = serde_json::from_str(&wire).unwrap();
            assert_eq!(header.request_id.as_deref(), Some(request_id));
            assert!(serde_json::from_str::<ClientMessage>(&wire).is_err());
        }
        let uncorrelated: RequestHeader =
            serde_json::from_str(r#"{"type":"createSendTransport"}"#).unwrap();
        assert!(uncorrelated.request_id.is_none());
        for invalid in [
            Value::Null,
            json!(""),
            json!("x".repeat(65)),
            json!("unsafe\n"),
            json!("é"),
            json!(1),
            json!({}),
            json!([]),
            json!(true),
        ] {
            let wire = json!({"type": "createSendTransport", "requestId": invalid}).to_string();
            assert!(serde_json::from_str::<RequestHeader>(&wire).is_err());
        }
        assert!(
            serde_json::from_str::<RequestHeader>(
                r#"{"type":"createSendTransport","requestId":"one","requestId":"two"}"#
            )
            .is_err()
        );
        // The payload's typed deserializer still detects duplicate fields.
        let wire =
            r#"{"type":"resumeConsumer","requestId":"one","consumerId":"a","consumerId":"b"}"#;
        assert_eq!(
            serde_json::from_str::<RequestHeader>(wire)
                .unwrap()
                .request_id
                .as_deref(),
            Some("one")
        );
        assert!(serde_json::from_str::<ClientMessage>(wire).is_err());
    }

    #[test]
    fn authentication_renewal_is_correlated_and_debug_redacts_the_bearer() {
        let wire = json!({
            "type": "renewAuthentication", "requestId": "renewal-1", "token": "fixture-bearer"
        });
        let message: ClientMessage = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(serde_json::to_value(&message).unwrap(), wire);
        let diagnostic = format!("{message:?}");
        assert!(diagnostic.contains("[REDACTED]"));
        assert!(!diagnostic.contains("fixture-bearer"));
        assert_eq!(
            serde_json::to_value(ServerMessage::AuthenticationRenewed {
                request_id: "renewal-1".into(),
                expires_at: 1234,
            })
            .unwrap(),
            json!({"type":"authenticationRenewed", "requestId":"renewal-1", "expiresAt":1234}),
        );
        assert_eq!(
            serde_json::to_value(ServerMessage::AuthenticationRenewalFailed {
                request_id: "renewal-1".into(),
            })
            .unwrap(),
            json!({"type":"authenticationRenewalFailed", "requestId":"renewal-1"}),
        );
        assert_eq!(
            serde_json::to_value(ServerMessage::AuthenticationRenewalDeferred {
                request_id: "renewal-1".into(),
                retry_after_ms: 3000,
                expires_at: 1234,
            })
            .unwrap(),
            json!({"type":"authenticationRenewalDeferred", "requestId":"renewal-1", "retryAfterMs":3000, "expiresAt":1234}),
        );
    }

    #[test]
    fn ice_restart_carries_fresh_turn_credentials() {
        let message = ServerMessage::IceRestarted {
            transport_id: "transport-1".into(),
            ice_parameters: serde_json::from_value(
                json!({"usernameFragment":"u","password":"p","iceLite":true}),
            )
            .unwrap(),
            ice_servers: vec![crate::turn::IceServer {
                urls: vec!["turn:relay.example:3478".into()],
                username: Some("1700000000:credential".into()),
                credential: Some("mac".into()),
            }],
        };
        let wire = serde_json::to_value(&message).unwrap();
        assert_eq!(wire["type"], "iceRestarted");
        assert_eq!(wire["transportId"], "transport-1");
        assert_eq!(wire["iceParameters"]["usernameFragment"], "u");
        assert_eq!(wire["iceServers"][0]["urls"][0], "turn:relay.example:3478");
        assert_eq!(wire["iceServers"][0]["username"], "1700000000:credential");
    }

    #[test]
    fn temporary_restart_and_permanent_room_closure_have_distinct_wire_events() {
        let temporary = serde_json::to_value(ServerMessage::ServerRestarting {
            reason: "Server shutting down".into(),
        })
        .unwrap();
        let terminal = serde_json::to_value(ServerMessage::RoomClosed {
            reason: "Room deleted".into(),
        })
        .unwrap();
        assert_eq!(
            temporary,
            json!({"type": "serverRestarting", "reason": "Server shutting down"})
        );
        assert_eq!(terminal["type"], "roomClosed");
    }

    fn assert_nullable_settings(
        message: &ClientMessage,
        expected_broadcasters: Option<Option<i32>>,
        expected_participants: Option<Option<i32>>,
        expected_password: Option<Option<&str>>,
    ) {
        let ClientMessage::UpdateRoomSettings {
            patch:
                super::SettingsPatch {
                    max_broadcasters,
                    max_participants,
                    password,
                    ..
                },
        } = message
        else {
            panic!("expected a room settings patch");
        };
        assert_eq!(*max_broadcasters, expected_broadcasters);
        assert_eq!(*max_participants, expected_participants);
        assert_eq!(
            password.as_ref().map(|value| value
                .as_ref()
                .map(crate::secret::SecretString::expose_secret)),
            expected_password
        );
    }

    #[test]
    fn missing_nullable_settings_stay_omitted_on_roundtrip() {
        let message: ClientMessage = serde_json::from_value(json!({
            "type": "updateRoomSettings", "allowChat": false,
        }))
        .unwrap();
        assert_nullable_settings(&message, None, None, None);
        let serialized = serde_json::to_value(&message).unwrap();
        for field in ["maxBroadcasters", "maxParticipants", "password"] {
            assert!(serialized.get(field).is_none(), "{field} must be omitted");
        }
        assert_eq!(serialized["allowChat"], false);
        let roundtrip = serde_json::from_value(serialized).unwrap();
        assert_nullable_settings(&roundtrip, None, None, None);
    }

    #[test]
    fn explicit_null_clears_nullable_settings_on_roundtrip() {
        let value = json!({
            "type": "updateRoomSettings",
            "maxBroadcasters": null, "maxParticipants": null, "password": null,
        });
        let message: ClientMessage = serde_json::from_value(value).unwrap();
        assert_nullable_settings(&message, Some(None), Some(None), Some(None));
        let serialized = serde_json::to_value(&message).unwrap();
        for field in ["maxBroadcasters", "maxParticipants", "password"] {
            assert_eq!(serialized.get(field), Some(&Value::Null));
        }
        let roundtrip = serde_json::from_value(serialized).unwrap();
        assert_nullable_settings(&roundtrip, Some(None), Some(None), Some(None));
    }

    #[test]
    fn values_set_nullable_settings_on_roundtrip() {
        let value = json!({
            "type": "updateRoomSettings",
            "maxBroadcasters": 4, "maxParticipants": 12, "password": "room-passphrase",
        });
        let message: ClientMessage = serde_json::from_value(value).unwrap();
        assert_nullable_settings(
            &message,
            Some(Some(4)),
            Some(Some(12)),
            Some(Some("room-passphrase")),
        );
        let roundtrip = serde_json::from_str(&serde_json::to_string(&message).unwrap()).unwrap();
        assert_nullable_settings(
            &roundtrip,
            Some(Some(4)),
            Some(Some(12)),
            Some(Some("room-passphrase")),
        );
    }

    #[test]
    fn nullable_settings_have_independent_patch_states() {
        let message: ClientMessage = serde_json::from_value(json!({
            "type": "updateRoomSettings", "maxBroadcasters": null, "password": "room-passphrase",
        }))
        .unwrap();
        let serialized = serde_json::to_value(&message).unwrap();
        assert_eq!(serialized.get("maxBroadcasters"), Some(&Value::Null));
        assert!(serialized.get("maxParticipants").is_none());
        assert_eq!(serialized["password"], "room-passphrase");
        let roundtrip = serde_json::from_value(serialized).unwrap();
        assert_nullable_settings(&roundtrip, Some(None), None, Some(Some("room-passphrase")));
    }

    #[test]
    fn nullable_settings_reject_wrong_wire_types() {
        for (field, value) in [
            ("maxBroadcasters", json!("4")),
            ("maxParticipants", json!(1.5)),
            ("password", json!(false)),
        ] {
            let mut message = json!({"type": "updateRoomSettings"});
            message[field] = value;
            assert!(serde_json::from_value::<ClientMessage>(message).is_err());
        }
    }

    #[test]
    fn room_settings_frames_tolerate_the_envelope_request_id() {
        let message: ClientMessage = serde_json::from_value(json!({
            "type": "updateRoomSettings", "requestId": "settings-1", "moderated": true,
        }))
        .unwrap();
        let ClientMessage::UpdateRoomSettings { patch } = message else {
            panic!("expected a room settings patch");
        };
        assert_eq!(patch.moderated, Some(true));
    }

    #[test]
    fn room_settings_patch_bodies_reject_unknown_fields() {
        let error = serde_json::from_value::<super::SettingsPatch>(json!({
            "requestId": "settings-1", "moderated": true,
        }))
        .expect_err("the HTTP body is strict");
        assert!(error.to_string().contains("unknown field"), "{error}");
    }
}
