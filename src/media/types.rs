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
    pub id: TransportId,
    pub ice_parameters: IceParameters,
    pub ice_candidates: Vec<IceCandidate>,
    pub dtls_parameters: DtlsParameters,
}

impl From<&WebRtcTransport> for TransportInfo {
    fn from(transport: &WebRtcTransport) -> Self {
        Self {
            id: transport.id(),
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
    pub id: ProducerId,
    pub kind: MediaKind,
    pub rtp_parameters: RtpParameters,
    pub paused: bool,
}

impl From<&Producer> for ProducerInfo {
    fn from(producer: &Producer) -> Self {
        Self {
            id: producer.id(),
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
    pub id: ConsumerId,
    pub producer_id: ProducerId,
    pub kind: MediaKind,
    pub rtp_parameters: RtpParameters,
    pub paused: bool,
    pub producer_paused: bool,
}

impl ConsumerInfo {
    pub fn from_consumer(consumer: &Consumer) -> Self {
        Self {
            id: consumer.id(),
            producer_id: consumer.producer_id(),
            kind: consumer.kind(),
            rtp_parameters: consumer.rtp_parameters().clone(),
            paused: consumer.paused(),
            producer_paused: consumer.producer_paused(),
        }
    }
}

/// Layer ceilings of one consumer that has layers. `top` is what the stream
/// can deliver; `client` is the viewer's own ceiling (tile size or manual
/// choice). The bandwidth tier lives on the participant, one per receive
/// transport. The worker receives the minimum of all three, so neither writer
/// can undo the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConsumerLayerState {
    pub top: ConsumerLayers,
    pub client: Option<ConsumerLayers>,
}

/// An estimate belongs to one receiver lifetime, not merely its participant.
#[derive(Debug, Clone, Copy)]
pub struct ReceiveBandwidthEstimate {
    pub transport_id: TransportId,
    pub bitrate: u32,
}

/// Participant media state
#[derive(Debug, Clone)]
pub struct ParticipantMedia {
    pub id: String,
    /// Unique lifetime identifier. The public participant ID can be reused
    /// after disconnect, so asynchronous cleanup must not key solely on it.
    pub generation: uuid::Uuid,
    pub send_transport: Option<WebRtcTransport>,
    pub recv_transport: Option<WebRtcTransport>,
    /// Native connect acknowledged for this exact transport lifetime. ICE can
    /// start DTLS before remote parameters are supplied, so DTLS state is not
    /// evidence that connect was applied. These IDs do not imply media readiness.
    pub(super) send_connect_applied: Option<TransportId>,
    pub(super) recv_connect_applied: Option<TransportId>,
    /// Only the most recent replacement is replayable, while its target is current.
    pub(super) recv_replacement: Option<(TransportId, TransportId)>,
    pub producers: HashMap<ProducerId, Producer>,
    pub consumers: HashMap<ConsumerId, Consumer>,
    /// Spatial layer ceiling from the receive transport's bandwidth estimate;
    /// applied to every layered consumer, including ones created later.
    pub bandwidth_spatial_ceiling: Option<u8>,
    /// Ceilings of the consumers that have layers at all. Audio and
    /// single-stream consumers have no entry and never receive layer requests.
    pub consumer_layers: HashMap<ConsumerId, ConsumerLayerState>,
    /// Where the receive transport was placed when it is not on the room's
    /// primary worker; shared so clones of this state keep one placement.
    pub viewer_lease: Option<std::sync::Arc<crate::media::router_manager::ViewerLease>>,
}

impl ParticipantMedia {
    pub fn new(id: String) -> Self {
        Self {
            id,
            generation: uuid::Uuid::new_v4(),
            send_transport: None,
            recv_transport: None,
            send_connect_applied: None,
            recv_connect_applied: None,
            recv_replacement: None,
            producers: HashMap::new(),
            consumers: HashMap::new(),
            bandwidth_spatial_ceiling: None,
            consumer_layers: HashMap::new(),
            viewer_lease: None,
        }
    }

    /// Closes all media resources for this participant.
    ///
    /// Dropping a mediasoup handle is synchronous and non-blocking: the crate
    /// fires its close handlers and hands the native close request to its own
    /// executor. Spawning a task per handle would only add scheduler work.
    pub async fn close_all(&mut self) {
        // Consumers depend on transports, so they go first.
        for (id, consumer) in self.consumers.drain() {
            drop(consumer);
            tracing::debug!("Closed consumer {}", id);
        }
        self.consumer_layers.clear();
        for (id, producer) in self.producers.drain() {
            drop(producer);
            tracing::debug!("Closed producer {}", id);
        }

        // Close transports last (after consumers/producers are gone)
        if let Some(transport) = self.send_transport.take() {
            drop(transport);
        }
        if let Some(transport) = self.recv_transport.take() {
            drop(transport);
        }
        self.send_connect_applied = None;
        self.recv_connect_applied = None;
        self.recv_replacement = None;
        self.bandwidth_spatial_ceiling = None;
        self.viewer_lease = None;
    }
}
