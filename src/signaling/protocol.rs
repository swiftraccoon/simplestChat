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
