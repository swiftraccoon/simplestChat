#![forbid(unsafe_code)]

// Signaling protocol - Message types for WebSocket communication

use crate::turn::IceServer;
use mediasoup::prelude::*;
use serde::{Deserialize, Serialize};

/// Client-to-Server messages
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ClientMessage {
    /// Join a room
    #[serde(rename_all = "camelCase")]
    JoinRoom {
        room_id: String,
        participant_name: String,
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
        source: Option<String>,  // "camera", "microphone", "screen", "screen-audio"
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
    ChatMessage {
        content: String,
    },

    // === Moderation ===
    /// Force-close a participant's camera/screen producer
    #[serde(rename_all = "camelCase")]
    CloseCam { target_participant_id: String },
    /// Ban a participant from producing video/screen
    #[serde(rename_all = "camelCase")]
    CamBan { target_participant_id: String, reason: Option<String> },
    /// Unban a participant from producing video/screen
    #[serde(rename_all = "camelCase")]
    CamUnban { target_participant_id: String },
    /// Mute a participant's text chat
    #[serde(rename_all = "camelCase")]
    TextMute { target_participant_id: String },
    /// Unmute a participant's text chat
    #[serde(rename_all = "camelCase")]
    TextUnmute { target_participant_id: String },
    /// Kick a participant from the room
    #[serde(rename_all = "camelCase")]
    Kick { target_participant_id: String, reason: Option<String> },
    /// Ban a participant from the room
    #[serde(rename_all = "camelCase")]
    Ban { target_participant_id: String, reason: Option<String>, duration: Option<u64> },
    /// Unban a user (stub — no persistent ban list yet)
    #[serde(rename_all = "camelCase")]
    Unban { target_user_id: String },
    /// Set a participant's role
    #[serde(rename_all = "camelCase")]
    SetRole { target_participant_id: String, role: u8 },
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
        max_broadcasters: Option<Option<i32>>,
        allow_screen_sharing: Option<bool>,
        allow_chat: Option<bool>,
        push_to_talk: Option<bool>,
        secret: Option<bool>,
        password: Option<Option<String>>,
    },
    /// Set the room topic
    SetTopic { topic: String },

    // === Lobby ===
    /// Admit a participant from the lobby
    #[serde(rename_all = "camelCase")]
    AdmitFromLobby { target_participant_id: String },
    /// Deny a participant from the lobby
    #[serde(rename_all = "camelCase")]
    DenyFromLobby { target_participant_id: String },
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
    },
    /// Error response
    Error {
        message: String,
    },
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
    TransportConnected {
        transport_id: String,
    },
    /// Producer created
    #[serde(rename_all = "camelCase")]
    ProducerCreated {
        producer_id: String,
    },
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
    },
    /// Participant left the room
    #[serde(rename_all = "camelCase")]
    ParticipantLeft {
        participant_id: String,
    },
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
    ProducerClosed {
        producer_id: String,
    },
    /// Producer paused (muted) by its owner
    #[serde(rename_all = "camelCase")]
    ProducerPaused {
        producer_id: String,
    },
    /// Producer resumed (unmuted) by its owner
    #[serde(rename_all = "camelCase")]
    ProducerResumed {
        producer_id: String,
    },
    /// Consumer resumed
    #[serde(rename_all = "camelCase")]
    ConsumerResumed {
        consumer_id: String,
    },
    /// Consumer paused
    #[serde(rename_all = "camelCase")]
    ConsumerPaused {
        consumer_id: String,
    },
    /// Result of reconnection attempt
    #[serde(rename_all = "camelCase")]
    ReconnectResult {
        success: bool,
        participant_id: String,
    },
    /// ICE restarted — new ICE parameters
    #[serde(rename_all = "camelCase")]
    IceRestarted {
        transport_id: String,
        ice_parameters: IceParameters,
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
    },
    /// Active/dominant speaker changed
    #[serde(rename_all = "camelCase")]
    ActiveSpeaker {
        participant_id: String,
    },
    /// Audio levels for all speaking participants
    #[serde(rename_all = "camelCase")]
    AudioLevels {
        levels: Vec<AudioLevelEntry>,
    },

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
    ParticipantKicked { participant_id: String, reason: Option<String> },
    /// Participant was banned from the room
    #[serde(rename_all = "camelCase")]
    ParticipantBanned { participant_id: String, reason: Option<String> },
    /// Participant's role was changed
    #[serde(rename_all = "camelCase")]
    RoleChanged { participant_id: String, new_role: String, granted_by: String },
    /// A participant requested voice (sent to Moderator+)
    #[serde(rename_all = "camelCase")]
    VoiceRequested { participant_id: String, display_name: String },

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
    LobbyWaiting { room_name: String, topic: Option<String>, participant_count: u32 },
    /// A participant joined the lobby (sent to Moderator+)
    #[serde(rename_all = "camelCase")]
    LobbyJoin { participant_id: String, display_name: String, authenticated: bool },
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
