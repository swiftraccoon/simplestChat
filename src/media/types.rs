#![forbid(unsafe_code)]

// Common types and error handling for the media module

use mediasoup::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use thiserror::Error;

/// Custom error type for media operations
#[derive(Error, Debug)]
pub enum MediaError {
    #[error("Worker error: {0}")]
    WorkerError(String),
    
    #[error("Router error: {0}")]
    RouterError(String),
    
    #[error("Transport error: {0}")]
    TransportError(String),
    
    #[error("Producer error: {0}")]
    ProducerError(String),
    
    #[error("Consumer error: {0}")]
    ConsumerError(String),
    
    #[error("Room not found: {0}")]
    RoomNotFound(String),
    
    #[error("Participant not found: {0}")]
    ParticipantNotFound(String),
    
    #[error("Resource not found: {0}")]
    ResourceNotFound(String),
    
    #[error("Invalid state: {0}")]
    InvalidState(String),
    
    #[error("Configuration error: {0}")]
    ConfigurationError(String),
    
    #[error("Mediasoup error: {0}")]
    MediasoupError(#[from] mediasoup::worker::RequestError),
    
    #[error("Other error: {0}")]
    Other(#[from] anyhow::Error),
}

/// Result type alias for media operations
pub type MediaResult<T> = Result<T, MediaError>;

/// Transport information for signaling
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransportInfo {
    pub id: String,
    pub ice_parameters: IceParameters,
    pub ice_candidates: Vec<IceCandidate>,
    pub dtls_parameters: DtlsParameters,
}

impl From<&WebRtcTransport> for TransportInfo {
    fn from(transport: &WebRtcTransport) -> Self {
        Self {
            id: transport.id().to_string(),
            ice_parameters: transport.ice_parameters().clone(),
            ice_candidates: transport.ice_candidates().clone(),
            dtls_parameters: transport.dtls_parameters(),
        }
    }
}

/// Producer information for signaling
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProducerInfo {
    pub id: String,
    pub kind: MediaKind,
    pub rtp_parameters: RtpParameters,
    pub paused: bool,
}

impl From<&Producer> for ProducerInfo {
    fn from(producer: &Producer) -> Self {
        Self {
            id: producer.id().to_string(),
            kind: producer.kind(),
            rtp_parameters: producer.rtp_parameters().clone(),
            paused: producer.paused(),
        }
    }
}

/// Consumer information for signaling
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConsumerInfo {
    pub id: String,
    pub producer_id: String,
    pub kind: MediaKind,
    pub rtp_parameters: RtpParameters,
    pub paused: bool,
    pub producer_paused: bool,
}

impl ConsumerInfo {
    pub fn from_consumer(consumer: &Consumer, producer_paused: bool) -> Self {
        Self {
            id: consumer.id().to_string(),
            producer_id: consumer.producer_id().to_string(),
            kind: consumer.kind(),
            rtp_parameters: consumer.rtp_parameters().clone(),
            paused: consumer.paused(),
            producer_paused,
        }
    }
}

/// Participant media state
#[derive(Debug, Clone)]
pub struct ParticipantMedia {
    pub id: String,
    pub send_transport: Option<WebRtcTransport>,
    pub recv_transport: Option<WebRtcTransport>,
    pub producers: HashMap<String, Producer>,
    pub consumers: HashMap<String, Consumer>,
}

impl ParticipantMedia {
    pub fn new(id: String) -> Self {
        Self {
            id,
            send_transport: None,
            recv_transport: None,
            producers: HashMap::new(),
            consumers: HashMap::new(),
        }
    }
    
    /// Closes all media resources for this participant
    pub async fn close_all(&mut self) {
        // Close all consumers
        for (_, _consumer) in self.consumers.drain() {
            // Consumers are automatically closed when dropped
        }

        // Close all producers
        for (_, _producer) in self.producers.drain() {
            // Producers are automatically closed when dropped
        }

        // Close transports
        if let Some(_transport) = self.send_transport.take() {
            // Transports are automatically closed when dropped
        }

        if let Some(_transport) = self.recv_transport.take() {
            // Transports are automatically closed when dropped
        }
    }
}

/// Transport statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransportStats {
    pub transport_id: String,
    pub timestamp: i64,
    pub bytes_sent: u64,
    pub bytes_received: u64,
    pub rtp_bytes_sent: u64,
    pub rtp_bytes_received: u64,
    pub rtcp_bytes_sent: u64,
    pub rtcp_bytes_received: u64,
    pub available_outgoing_bitrate: Option<u32>,
}

/// Producer statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProducerStats {
    pub producer_id: String,
    pub timestamp: i64,
    pub kind: MediaKind,
    pub bytes_sent: u64,
    pub packets_sent: u64,
    pub packets_lost: u32,
    pub jitter: u32,
    pub bitrate: u32,
}

/// Consumer statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConsumerStats {
    pub consumer_id: String,
    pub timestamp: i64,
    pub kind: MediaKind,
    pub bytes_received: u64,
    pub packets_received: u64,
    pub packets_lost: u32,
    pub jitter: u32,
    pub bitrate: u32,
}
