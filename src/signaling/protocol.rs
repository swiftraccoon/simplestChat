#![forbid(unsafe_code)]

// Signaling protocol - Message types for WebSocket communication

use crate::turn::IceServer;
use mediasoup::prelude::*;
use serde::{Deserialize, Serialize};

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
#[derive(Clone, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AuthenticationToken(String);

impl AuthenticationToken {
    pub(super) fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for AuthenticationToken {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("AuthenticationToken([REDACTED])")
    }
}

// Missing fields use the serde default (None); an explicit null must instead
// survive as Some(None) so nullable settings can be cleared.
fn deserialize_nullable_patch<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

/// Client-to-Server messages
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ClientMessage {
    /// Renew this authenticated socket without replacing its room membership.
    #[serde(rename_all = "camelCase")]
    RenewAuthentication {
        request_id: String,
        token: AuthenticationToken,
    },
    /// Join a room
    #[serde(rename_all = "camelCase")]
    JoinRoom {
        room_id: String,
        participant_name: String,
        /// Required for password-protected rooms (Admin+ bypass)
        #[serde(default)]
        password: Option<String>,
    },
    /// Leave the current room
    LeaveRoom,
    /// Get RTP capabilities from server
    GetRouterRtpCapabilities,
    /// Create WebRTC send transport
    CreateSendTransport,
    /// Create WebRTC receive transport
    CreateRecvTransport,
    /// Connect transport with DTLS parameters
    #[serde(rename_all = "camelCase")]
    ConnectTransport {
        transport_id: String,
        dtls_parameters: DtlsParameters,
    },
    /// Produce media (audio/video)
    #[serde(rename_all = "camelCase")]
    Produce {
        transport_id: String,
        kind: MediaKind,
        rtp_parameters: RtpParameters,
        #[serde(default)]
        source: Option<String>, // "camera", "microphone", "screen", "screen-audio"
    },
    /// Consume media from another participant
    #[serde(rename_all = "camelCase")]
    Consume {
        producer_id: String,
        rtp_capabilities: RtpCapabilities,
    },
    /// Resume a consumer
    #[serde(rename_all = "camelCase")]
    ResumeConsumer {
        consumer_id: String,
    },
    /// Pause a consumer
    #[serde(rename_all = "camelCase")]
    PauseConsumer {
        consumer_id: String,
    },
    /// Release this receiver's consumer; repeated closure is a no-op.
    #[serde(rename_all = "camelCase")]
    CloseConsumer {
        consumer_id: String,
    },
    /// Close a producer
    #[serde(rename_all = "camelCase")]
    CloseProducer {
        producer_id: String,
    },
    /// Pause a producer (mute)
    #[serde(rename_all = "camelCase")]
    PauseProducer {
        producer_id: String,
    },
    /// Resume a producer (unmute)
    #[serde(rename_all = "camelCase")]
    ResumeProducer {
        producer_id: String,
    },
    /// Reconnect to an existing session after WS disconnect
    #[serde(rename_all = "camelCase")]
    Reconnect {
        participant_id: String,
        room_id: String,
        reconnect_token: String,
    },
    /// Request ICE restart on a transport
    #[serde(rename_all = "camelCase")]
    RestartIce {
        transport_id: String,
    },
    /// Set preferred simulcast layers for a consumer
    #[serde(rename_all = "camelCase")]
    SetConsumerPreferredLayers {
        consumer_id: String,
        spatial_layer: u8,
        temporal_layer: Option<u8>,
    },
    /// Send a chat message to the room
    #[serde(rename_all = "camelCase")]
    ChatMessage {
        content: String,
        #[serde(default)]
        client_message_id: Option<String>,
        #[serde(default)]
        sequence: Option<u64>,
    },
    #[serde(rename_all = "camelCase")]
    PrivateMessage {
        target_participant_id: String,
        content: String,
        client_message_id: String,
        #[serde(default)]
        sequence: Option<u64>,
    },
    RetryChatMessage(RetryChatMessage),
    #[serde(rename_all = "camelCase")]
    SetChatPreferences {
        request_id: String,
        allow_private_messages: bool,
        ignored_participant_ids: Vec<String>,
    },
    #[serde(rename_all = "camelCase")]
    ChangeNickname {
        request_id: String,
        nickname: String,
    },
    #[serde(rename_all = "camelCase")]
    GetRoomSnapshot {
        request_id: String,
    },
    #[serde(rename_all = "camelCase")]
    ListRoomBans {
        request_id: String,
        #[serde(default)]
        offset: Option<u32>,
    },
    #[serde(rename_all = "camelCase")]
    RemoveRoomBan {
        request_id: String,
        ban_id: String,
    },
    #[serde(rename_all = "camelCase")]
    ListRoomMembers {
        request_id: String,
        #[serde(default)]
        offset: Option<u32>,
    },
    #[serde(rename_all = "camelCase")]
    SetMemberRole {
        request_id: String,
        target_user_id: String,
        role: u8,
    },
    #[serde(rename_all = "camelCase")]
    ReportParticipant {
        request_id: String,
        target_participant_id: String,
        reason: String,
    },
    #[serde(rename_all = "camelCase")]
    ListRoomReports {
        request_id: String,
        #[serde(default)]
        offset: Option<u32>,
    },
    #[serde(rename_all = "camelCase")]
    ResolveRoomReport {
        request_id: String,
        report_id: String,
        status: String,
    },

    // === Moderation ===
    /// Force-close a participant's camera/screen producer
    #[serde(rename_all = "camelCase")]
    CloseCam {
        target_participant_id: String,
    },
    /// Ban a participant from producing video/screen
    #[serde(rename_all = "camelCase")]
    CamBan {
        target_participant_id: String,
        reason: Option<String>,
    },
    /// Unban a participant from producing video/screen
    #[serde(rename_all = "camelCase")]
    CamUnban {
        target_participant_id: String,
    },
    /// Mute a participant's text chat
    #[serde(rename_all = "camelCase")]
    TextMute {
        target_participant_id: String,
    },
    /// Unmute a participant's text chat
    #[serde(rename_all = "camelCase")]
    TextUnmute {
        target_participant_id: String,
    },
    /// Kick a participant from the room
    #[serde(rename_all = "camelCase")]
    Kick {
        target_participant_id: String,
        reason: Option<String>,
    },
    /// Ban a participant from the room
    #[serde(rename_all = "camelCase")]
    Ban {
        target_participant_id: String,
        reason: Option<String>,
        duration: Option<u64>,
    },
    /// Unban a user (stub — no persistent ban list yet)
    #[serde(rename_all = "camelCase")]
    Unban {
        target_user_id: String,
    },
    /// Set a participant's role
    #[serde(rename_all = "camelCase")]
    SetRole {
        target_participant_id: String,
        role: u8,
    },
    /// Request voice in a moderated room (sent by User/Guest)
    RequestVoice,

    // === Room management ===
    /// Update room settings (Admin+)
    #[serde(rename_all = "camelCase")]
    UpdateRoomSettings {
        moderated: Option<bool>,
        lobby_enabled: Option<bool>,
        guests_allowed: Option<bool>,
        guests_can_broadcast: Option<bool>,
        #[serde(
            default,
            deserialize_with = "deserialize_nullable_patch",
            skip_serializing_if = "Option::is_none"
        )]
        max_broadcasters: Option<Option<i32>>,
        #[serde(
            default,
            deserialize_with = "deserialize_nullable_patch",
            skip_serializing_if = "Option::is_none"
        )]
        max_participants: Option<Option<i32>>,
        allow_screen_sharing: Option<bool>,
        allow_chat: Option<bool>,
        allow_video: Option<bool>,
        require_registration: Option<bool>,
        invite_only: Option<bool>,
        push_to_talk: Option<bool>,
        secret: Option<bool>,
        #[serde(
            default,
            deserialize_with = "deserialize_nullable_patch",
            skip_serializing_if = "Option::is_none"
        )]
        password: Option<Option<String>>,
    },
    /// Set the room topic
    SetTopic {
        topic: String,
    },

    // === Lobby ===
    /// Admit a participant from the lobby
    #[serde(rename_all = "camelCase")]
    AdmitFromLobby {
        target_participant_id: String,
    },
    /// Deny a participant from the lobby
    #[serde(rename_all = "camelCase")]
    DenyFromLobby {
        target_participant_id: String,
    },
}

/// Server-to-Client messages
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ServerMessage {
    /// Room joined successfully
    #[serde(rename_all = "camelCase")]
    RoomJoined {
        participant_id: String,
        participants: Vec<ParticipantInfo>,
        reconnect_token: String,
        your_role: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        room_settings: Option<serde_json::Value>,
    },
    /// The existing socket accepted a same-identity credential renewal.
    #[serde(rename_all = "camelCase")]
    AuthenticationRenewed { request_id: String, expires_at: u64 },
    /// A renewal did not replace or extend the existing socket credential.
    #[serde(rename_all = "camelCase")]
    AuthenticationRenewalFailed { request_id: String },
    /// Temporary validation failure; the existing credential remains unchanged.
    #[serde(rename_all = "camelCase")]
    AuthenticationRenewalDeferred {
        request_id: String,
        retry_after_ms: u32,
        expires_at: u64,
    },
    /// Error response
    Error { message: String },
    /// The client should prompt for a password and retry this room join.
    RoomPasswordRequired,
    /// Terminal room lifecycle event, distinct from recoverable request errors.
    RoomClosed { reason: String },
    /// Temporary process shutdown; clients may retain room intent and rejoin.
    /// Existing media transports do not survive the replacement process.
    ServerRestarting { reason: String },
    /// Router RTP capabilities
    #[serde(rename_all = "camelCase")]
    RouterRtpCapabilities {
        rtp_capabilities: RtpCapabilitiesFinalized,
    },
    /// Transport created
    #[serde(rename_all = "camelCase")]
    TransportCreated {
        transport_id: String,
        ice_parameters: IceParameters,
        ice_candidates: Vec<IceCandidate>,
        dtls_parameters: DtlsParameters,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        ice_servers: Vec<IceServer>,
    },
    /// Transport connected
    #[serde(rename_all = "camelCase")]
    TransportConnected { transport_id: String },
    /// Producer created
    #[serde(rename_all = "camelCase")]
    ProducerCreated { producer_id: String },
    /// Consumer created
    #[serde(rename_all = "camelCase")]
    ConsumerCreated {
        consumer_id: String,
        producer_id: String,
        kind: MediaKind,
        rtp_parameters: RtpParameters,
    },
    /// New participant joined the room
    #[serde(rename_all = "camelCase")]
    ParticipantJoined {
        participant_id: String,
        participant_name: String,
        role: String,
        authenticated: bool,
    },
    /// Participant left the room
    #[serde(rename_all = "camelCase")]
    ParticipantLeft { participant_id: String },
    /// New producer available from another participant
    #[serde(rename_all = "camelCase")]
    NewProducer {
        participant_id: String,
        producer_id: String,
        kind: MediaKind,
        #[serde(default)]
        source: Option<String>,
    },
    /// Producer closed by another participant
    #[serde(rename_all = "camelCase")]
    ProducerClosed { producer_id: String },
    /// Producer paused (muted) by its owner
    #[serde(rename_all = "camelCase")]
    ProducerPaused { producer_id: String },
    /// Producer resumed (unmuted) by its owner
    #[serde(rename_all = "camelCase")]
    ProducerResumed { producer_id: String },
    /// Consumer resumed
    #[serde(rename_all = "camelCase")]
    ConsumerResumed { consumer_id: String },
    /// Consumer paused
    #[serde(rename_all = "camelCase")]
    ConsumerPaused { consumer_id: String },
    /// A correlated closure or preferred-layer command completed successfully.
    /// Sent only inside a reply envelope with the caller's request ID.
    MediaControlApplied,
    /// A room mutation completed. The request ID identifies only this command;
    /// state broadcasts remain independent and a timeout is not a rollback.
    RoomControlApplied,
    /// Result of reconnection attempt
    #[serde(rename_all = "camelCase")]
    ReconnectResult {
        success: bool,
        participant_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        reconnect_token: Option<String>,
    },
    /// ICE restarted — new ICE parameters plus fresh TURN credentials, since
    /// the ones minted at transport creation expire after `TURN_TTL`.
    #[serde(rename_all = "camelCase")]
    IceRestarted {
        transport_id: String,
        ice_parameters: IceParameters,
        ice_servers: Vec<IceServer>,
    },
    /// Connection quality stats
    #[serde(rename_all = "camelCase")]
    ConnectionStats {
        available_bitrate: Option<u32>,
        rtt: Option<f64>,
    },
    /// Consumer simulcast layers changed
    #[serde(rename_all = "camelCase")]
    ConsumerLayersChanged {
        consumer_id: String,
        spatial_layer: Option<u8>,
        temporal_layer: Option<u8>,
    },
    /// Chat message received from another participant
    #[serde(rename_all = "camelCase")]
    ChatReceived {
        participant_id: String,
        participant_name: String,
        content: String,
        message_id: String,
        client_message_id: String,
        sent_at: String,
    },
    #[serde(rename_all = "camelCase")]
    MessageAck {
        client_message_id: String,
        message: ChatEntry,
    },
    #[serde(rename_all = "camelCase")]
    MessageRetryResult {
        client_message_id: String,
        outcome: ChatRetryOutcome,
        reason: ChatRetryReason,
    },
    #[serde(rename_all = "camelCase")]
    PrivateMessageReceived { message: ChatEntry },
    #[serde(rename_all = "camelCase")]
    SocialResponse {
        request_id: String,
        action: String,
        data: serde_json::Value,
    },
    #[serde(rename_all = "camelCase")]
    SocialError {
        #[serde(skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        client_message_id: Option<String>,
        message: String,
    },
    #[serde(rename_all = "camelCase")]
    NicknameChanged {
        participant_id: String,
        nickname: String,
    },
    /// Active/dominant speaker changed
    #[serde(rename_all = "camelCase")]
    ActiveSpeaker { participant_id: String },
    /// Audio levels for all speaking participants; an empty list means nobody
    /// exceeded the threshold during the last interval
    #[serde(rename_all = "camelCase")]
    AudioLevels { levels: Vec<AudioLevelEntry> },

    // === Moderation broadcasts ===
    /// A producer was force-closed by a moderator
    #[serde(rename_all = "camelCase")]
    ForceClosedProducer { producer_id: String, reason: String },
    /// Participant's camera was banned
    #[serde(rename_all = "camelCase")]
    CamBanned { participant_id: String },
    /// Participant's camera ban was lifted
    #[serde(rename_all = "camelCase")]
    CamUnbanned { participant_id: String },
    /// Participant was text-muted
    #[serde(rename_all = "camelCase")]
    TextMuted { participant_id: String },
    /// Participant was text-unmuted
    #[serde(rename_all = "camelCase")]
    TextUnmuted { participant_id: String },
    /// Participant was kicked from the room
    #[serde(rename_all = "camelCase")]
    ParticipantKicked {
        participant_id: String,
        reason: Option<String>,
    },
    /// Participant was banned from the room
    #[serde(rename_all = "camelCase")]
    ParticipantBanned {
        participant_id: String,
        reason: Option<String>,
    },
    /// Participant's role was changed
    #[serde(rename_all = "camelCase")]
    RoleChanged {
        participant_id: String,
        new_role: String,
        granted_by: String,
    },
    /// A participant requested voice (sent to Moderator+)
    #[serde(rename_all = "camelCase")]
    VoiceRequested {
        participant_id: String,
        display_name: String,
    },

    // === Room state ===
    /// Room settings were changed
    #[serde(rename_all = "camelCase")]
    RoomSettingsChanged { settings: serde_json::Value },
    /// Room topic was changed
    #[serde(rename_all = "camelCase")]
    TopicChanged { topic: String, changed_by: String },

    // === Lobby ===
    /// Client is waiting in the lobby
    #[serde(rename_all = "camelCase")]
    LobbyWaiting {
        room_name: String,
        topic: Option<String>,
        participant_count: u32,
        moderator_count: u32,
    },
    #[serde(rename_all = "camelCase")]
    LobbyStatus {
        participant_count: u32,
        moderator_count: u32,
    },
    /// A participant joined the lobby (sent to Moderator+)
    #[serde(rename_all = "camelCase")]
    LobbyJoin {
        participant_id: String,
        display_name: String,
        authenticated: bool,
    },
    /// Lobby admission was denied
    #[serde(rename_all = "camelCase")]
    LobbyDenied { reason: Option<String> },
    /// Client was admitted from the lobby
    LobbyAdmitted,
}

/// Audio level entry for a speaking participant
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioLevelEntry {
    pub participant_id: String,
    pub volume: i8, // dBov (0 = loudest, -127 = silence)
}

/// Participant information for room state
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParticipantInfo {
    pub id: String,
    pub name: String,
    pub producers: Vec<ProducerMetadata>,
    pub role: String,
    #[serde(default)]
    pub authenticated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatEntry {
    pub message_id: String,
    pub client_message_id: String,
    pub participant_id: String,
    pub participant_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recipient_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recipient_name: Option<String>,
    pub content: String,
    pub sent_at: String,
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RetryChatMessage {
    pub client_message_id: String,
    pub sequence: u64,
    pub chat_session_id: String,
    pub content: String,
    pub target_participant_id: Option<String>,
}

/// Producer metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProducerMetadata {
    pub id: String,
    pub kind: MediaKind,
    #[serde(default)]
    pub source: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::{ClientMessage, RequestHeader, ServerMessage};
    use serde_json::{Value, json};

    #[test]
    fn request_headers_preserve_ids_without_weakening_payload_validation() {
        for request_id in ["request-1", "A_0-z", &"x".repeat(64)] {
            let wire = json!({"type": "consume", "requestId": request_id}).to_string();
            let header: RequestHeader = serde_json::from_str(&wire).unwrap();
            assert_eq!(header.request_id.as_deref(), Some(request_id));
            assert!(serde_json::from_str::<ClientMessage>(&wire).is_err());
        }
        let legacy: RequestHeader =
            serde_json::from_str(r#"{"type":"createSendTransport"}"#).unwrap();
        assert!(legacy.request_id.is_none());
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
            max_broadcasters,
            max_participants,
            password,
            ..
        } = message
        else {
            panic!("expected a room settings patch");
        };
        assert_eq!(*max_broadcasters, expected_broadcasters);
        assert_eq!(*max_participants, expected_participants);
        assert_eq!(
            password.as_ref().map(|value| value.as_deref()),
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
}
