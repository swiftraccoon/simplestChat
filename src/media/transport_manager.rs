#![forbid(unsafe_code)]

// Transport management for WebRTC connections

use crate::diagnostics::{Stage, measure, measure_result};
use crate::media::config::WebRtcTransportConfig;
use crate::media::types::{
    ConsumerLayerState, MediaError, MediaResult, ParticipantMedia, ReceiveBandwidthEstimate,
    TransportInfo,
};
use crate::signaling::protocol::ServerMessage;
use crate::sync::RwLock;
use anyhow::Result;
use mediasoup::consumer::ConsumerType;
use mediasoup::prelude::*;
use mediasoup::transport::{TransportTraceEventData, TransportTraceEventType};
use mediasoup_types::data_structures::DtlsState;
use std::any::Any;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::Mutex as TokioMutex;
use tokio::sync::mpsc;
use tracing::{debug, info};
use uuid::Uuid;

#[cfg(test)]
#[path = "receive_replacement_tests.rs"]
mod receive_replacement_tests;

fn producer_pause_transition_needed(currently_paused: bool, requested_paused: bool) -> bool {
    currently_paused != requested_paused
}

fn consumer_pause_transition_needed(currently_paused: bool, requested_paused: bool) -> bool {
    currently_paused != requested_paused
}

/// Set only from the room's validated producer registry, never request appData.
/// This remains attached to a consumer even when its producer is piped from
/// another worker, whose local producer metadata need not contain the source.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ConsumerMediaSource {
    ScreenVideo,
    Other,
}

impl ConsumerMediaSource {
    pub(crate) fn from_room_source(kind: MediaKind, source: Option<&str>) -> Self {
        if kind == MediaKind::Video && source == Some("screen") {
            Self::ScreenVideo
        } else {
            Self::Other
        }
    }
}

fn consumer_video_priority(kind: MediaKind, app_data: &AppData) -> u8 {
    if kind == MediaKind::Video
        && app_data.downcast_ref::<ConsumerMediaSource>() == Some(&ConsumerMediaSource::ScreenVideo)
    {
        4
    } else {
        1
    }
}

fn consumer_layers_transition_needed(
    current_layers: Option<ConsumerLayers>,
    requested_layers: ConsumerLayers,
) -> bool {
    current_layers != Some(requested_layers)
}

/// The layers the worker should receive for one consumer: the stream's top
/// layers bounded by the viewer's ceiling and by the transport's bandwidth
/// tier. An absent client temporal layer means the top one, so the result is
/// fully specified and compares exactly with the worker's applied value.
fn effective_layers(
    top: ConsumerLayers,
    client: Option<ConsumerLayers>,
    bandwidth_spatial: Option<u8>,
) -> ConsumerLayers {
    let mut spatial = top.spatial_layer;
    let mut temporal = top.temporal_layer;
    if let Some(client) = client {
        spatial = spatial.min(client.spatial_layer);
        if let (Some(requested), Some(limit)) = (client.temporal_layer, top.temporal_layer) {
            temporal = Some(requested.min(limit));
        }
    }
    if let Some(tier) = bandwidth_spatial {
        spatial = spatial.min(tier);
    }
    ConsumerLayers {
        spatial_layer: spatial,
        temporal_layer: temporal,
    }
}

/// The top spatial and temporal layer of a consumer that has layers, from
/// the scalability mode mediasoup assigned it; `None` for simple consumers
/// (audio, single-stream video), which the worker accepts layer requests for
/// without effect and which must therefore never be sent one.
fn layered_top(consumer: &Consumer) -> Option<ConsumerLayers> {
    if consumer.r#type() == ConsumerType::Simple {
        return None;
    }
    let mode = &consumer
        .rtp_parameters()
        .encodings
        .first()?
        .scalability_mode;
    Some(ConsumerLayers {
        spatial_layer: mode.spatial_layers().get() - 1,
        temporal_layer: Some(mode.temporal_layers().get() - 1),
    })
}

/// Extracts only the public UUID from a room-scoped media namespace. The
/// namespace also contains a room identifier, which must not enter lifecycle
/// markers. Reject unfamiliar shapes instead of falling back to the raw key.
fn lifecycle_participant_id(media_namespace: &str) -> Option<Uuid> {
    let mut parts = media_namespace.rsplitn(3, '\u{1f}');
    let participant_id = Uuid::parse_str(parts.next()?).ok()?;
    Uuid::parse_str(parts.next()?).ok()?;
    let room_id = parts.next()?;
    if room_id.is_empty() {
        return None;
    }
    Some(participant_id)
}

/// Keep application and wire bitrates within their 32-bit contract. The worker
/// reports 64-bit estimates; saturation preserves high-bandwidth tier selection
/// without truncating an oversized estimate into a low-bandwidth value.
fn application_bitrate(bitrate: u64) -> u32 {
    u32::try_from(bitrate).unwrap_or(u32::MAX)
}

/// Manages WebRTC transports for participants.
///
/// Uses per-participant locking: the outer HashMap is protected by a std::sync::RwLock
/// (held only for brief lookups, never across await points), while each participant's
/// media state is protected by its own tokio::sync::Mutex (held across async operations
/// but only blocking that specific participant).
pub struct TransportManager {
    participants: Arc<RwLock<HashMap<String, Arc<TokioMutex<ParticipantMedia>>>>>,
    /// The concrete callback handle type is intentionally erased here because
    /// mediasoup does not re-export it. Retaining one handle per media-state
    /// keeps the callback active; replacing/dropping it unregisters the old
    /// reconnect subscription instead of leaking detached callbacks. A unique
    /// generation prevents cleanup of an old participant ID from touching a
    /// newly-created media state that happens to reuse that public ID.
    bwe_trace_handlers: Arc<RwLock<HashMap<Uuid, Box<dyn Any + Send + Sync>>>>,
    /// `MediaConfig::key_frame_request_delay_ms`, applied to video producers.
    key_frame_request_delay: u32,
    /// `MediaConfig::max_producers_per_participant`.
    producer_cap: usize,
    /// `MediaConfig::max_consumers_per_participant`.
    consumer_cap: usize,
}

/// A placement selected before replacing the receiver. An unchanged placement
/// retains its lease; a newly selected one is installed only after native setup.
pub(crate) struct ReceiveReplacementPlacement {
    pub router: Router,
    pub webrtc_server: WebRtcServer,
    pub lease: Option<crate::media::router_manager::ViewerLease>,
    pub retain_existing_lease: bool,
}

impl Default for TransportManager {
    fn default() -> Self {
        Self::new()
    }
}

impl TransportManager {
    /// Collects bounded worker forwarding/intake observations without waiting
    /// for participant locks or retaining them across native statistics calls.
    pub async fn diagnostic_snapshot(
        &self,
        context: &super::diagnostics::SnapshotContext,
    ) -> super::diagnostics::MediaSnapshot {
        super::diagnostics::collect_snapshot(&self.participants, context).await
    }

    /// Creates a new TransportManager
    pub fn new() -> Self {
        Self {
            participants: Arc::new(RwLock::new(HashMap::new())),
            bwe_trace_handlers: Arc::new(RwLock::new(HashMap::new())),
            key_frame_request_delay: 0,
            producer_cap: super::config::DEFAULT_MAX_PRODUCERS_PER_PARTICIPANT,
            consumer_cap: super::config::DEFAULT_MAX_CONSUMERS_PER_PARTICIPANT,
        }
    }

    /// Bound every participant's producers and consumers.
    #[must_use]
    pub fn with_participant_caps(mut self, producers: usize, consumers: usize) -> Self {
        self.producer_cap = producers;
        self.consumer_cap = consumers;
        self
    }

    /// Coalesce keyframe requests to each video producer's sender to one per
    /// `milliseconds` (0 forwards every request).
    #[must_use]
    pub fn with_key_frame_request_delay(mut self, milliseconds: u32) -> Self {
        self.key_frame_request_delay = milliseconds;
        self
    }

    fn replace_bwe_trace_handler(&self, generation: Uuid, handler: Box<dyn Any + Send + Sync>) {
        self.bwe_trace_handlers.write().insert(generation, handler);
    }

    fn clear_bwe_trace_handler(&self, generation: Uuid) {
        self.bwe_trace_handlers.write().remove(&generation);
    }

    /// Gets or creates a participant entry (brief outer lock, no await)
    fn get_or_create_participant(&self, participant_id: &str) -> Arc<TokioMutex<ParticipantMedia>> {
        // Fast path: read lock
        {
            let participants = self.participants.read();
            if let Some(p) = participants.get(participant_id) {
                return Arc::clone(p);
            }
        }
        // Slow path: write lock to insert
        let mut participants = self.participants.write();
        participants
            .entry(participant_id.to_string())
            .or_insert_with(|| {
                Arc::new(TokioMutex::new(ParticipantMedia::new(
                    participant_id.to_string(),
                )))
            })
            .clone()
    }

    /// Gets an existing participant's lock (brief outer read lock, no await)
    fn get_participant_lock(
        &self,
        participant_id: &str,
    ) -> MediaResult<Arc<TokioMutex<ParticipantMedia>>> {
        let participants = self.participants.read();
        participants
            .get(participant_id)
            .cloned()
            .ok_or_else(|| MediaError::ParticipantNotFound(participant_id.to_string()))
    }

    /// Confirms that a previously cloned participant handle still represents
    /// the currently registered media session. Disconnect removes the map
    /// entry before waiting for its mutex, so a stale handle must be rejected
    /// before it can retain newly-created transports outside the map.
    fn participant_is_current(
        &self,
        participant_id: &str,
        participant_lock: &Arc<TokioMutex<ParticipantMedia>>,
    ) -> bool {
        let participants = self.participants.read();
        participants
            .get(participant_id)
            .is_some_and(|current| Arc::ptr_eq(current, participant_lock))
    }

    /// Creates a send transport for a participant
    pub async fn create_send_transport(
        &self,
        participant_id: String,
        router: &Router,
        webrtc_server: WebRtcServer,
        config: &WebRtcTransportConfig,
    ) -> MediaResult<TransportInfo> {
        config
            .validate()
            .map_err(|error| MediaError::ConfigurationError(error.to_string()))?;
        debug!(
            "Creating send transport for participant: {}",
            participant_id
        );

        let participant_lock = self.get_or_create_participant(&participant_id);
        let mut participant = measure(Stage::SessionLockWait, participant_lock.lock()).await;
        if !self.participant_is_current(&participant_id, &participant_lock) {
            return Err(MediaError::InvalidState(
                "Participant media session is no longer active".to_string(),
            ));
        }
        if participant
            .send_transport
            .as_ref()
            .is_some_and(|transport| !transport.closed())
        {
            return Err(MediaError::TransportError(
                "Send transport already exists".to_string(),
            ));
        }

        // Keep the per-participant lock across creation so disconnect cleanup
        // cannot complete and leave this transport in a detached media state.
        let transport_options = Self::transport_options(webrtc_server, config);
        let transport = measure_result(Stage::MediaCreateTransport, async {
            let transport = router
                .create_webrtc_transport(transport_options)
                .await
                .map_err(|e| {
                    MediaError::TransportError(format!("Failed to create send transport: {e}"))
                })?;

            if let Some(maximum) = config.max_incoming_bitrate {
                transport
                    .set_max_incoming_bitrate(u64::from(maximum))
                    .await
                    .map_err(|error| {
                        MediaError::TransportError(format!(
                            "Failed to apply incoming bitrate limit: {error}"
                        ))
                    })?;
            }

            Ok::<_, MediaError>(transport)
        })
        .await?;

        if !self.participant_is_current(&participant_id, &participant_lock) {
            drop(transport);
            return Err(MediaError::InvalidState(
                "Participant media session closed while creating send transport".to_string(),
            ));
        }

        let transport_info = TransportInfo::from(&transport);
        self.setup_transport_handlers(&transport, &participant_id, "send");
        participant.send_transport = Some(transport);
        participant.send_connect_applied = None;

        info!(
            "Created send transport {} for participant {}",
            transport_info.id, participant_id
        );
        Ok(transport_info)
    }

    /// Creates a receive transport for a participant
    pub async fn create_recv_transport(
        &self,
        participant_id: String,
        router: &Router,
        webrtc_server: WebRtcServer,
        config: &WebRtcTransportConfig,
    ) -> MediaResult<TransportInfo> {
        config
            .validate()
            .map_err(|error| MediaError::ConfigurationError(error.to_string()))?;
        debug!(
            "Creating receive transport for participant: {}",
            participant_id
        );

        let participant_lock = self.get_or_create_participant(&participant_id);
        let mut participant = measure(Stage::SessionLockWait, participant_lock.lock()).await;
        if !self.participant_is_current(&participant_id, &participant_lock) {
            return Err(MediaError::InvalidState(
                "Participant media session is no longer active".to_string(),
            ));
        }
        if participant
            .recv_transport
            .as_ref()
            .is_some_and(|transport| !transport.closed())
        {
            return Err(MediaError::TransportError(
                "Receive transport already exists".to_string(),
            ));
        }

        // Keep the per-participant lock across creation so disconnect cleanup
        // cannot complete and leave this transport in a detached media state.
        let transport = Self::build_recv_transport(router, webrtc_server, config).await?;

        if !self.participant_is_current(&participant_id, &participant_lock) {
            drop(transport);
            return Err(MediaError::InvalidState(
                "Participant media session closed while creating receive transport".to_string(),
            ));
        }

        let transport_info = TransportInfo::from(&transport);
        self.setup_transport_handlers(&transport, &participant_id, "recv");
        self.clear_bwe_trace_handler(participant.generation);
        participant.recv_transport = Some(transport);
        participant.recv_connect_applied = None;
        participant.recv_replacement = None;
        participant.bandwidth_spatial_ceiling = None;

        info!(
            "Created receive transport {} for participant {}",
            transport_info.id, participant_id
        );
        Ok(transport_info)
    }

    fn replay_recv_replacement(
        participant: &ParticipantMedia,
        expected: TransportId,
    ) -> MediaResult<Option<TransportInfo>> {
        let current = participant
            .recv_transport
            .as_ref()
            .ok_or_else(|| MediaError::TransportError("Receive transport not found".to_string()))?;
        if current.id() == expected {
            return Ok(None);
        }
        if !current.closed() && participant.recv_replacement == Some((expected, current.id())) {
            return Ok(Some(TransportInfo::from(current)));
        }
        Err(MediaError::InvalidState(
            "Receive transport changed; replacement request is stale".to_string(),
        ))
    }

    /// Check the expected receiver before any placement or native allocation.
    pub(crate) async fn recv_replacement_replay(
        &self,
        participant_id: &str,
        expected: TransportId,
    ) -> MediaResult<Option<TransportInfo>> {
        let participant_lock = self.get_participant_lock(participant_id)?;
        let participant = participant_lock.lock().await;
        if !self.participant_is_current(participant_id, &participant_lock) {
            return Err(MediaError::InvalidState(
                "Media session changed".to_string(),
            ));
        }
        Self::replay_recv_replacement(&participant, expected)
    }

    /// Build before retiring the current receiver, then atomically swap only its
    /// receive state. One old-to-new receipt makes a lost reply safely retryable.
    pub(crate) async fn replace_recv_transport(
        &self,
        participant_id: &str,
        expected: TransportId,
        placement: ReceiveReplacementPlacement,
        config: &WebRtcTransportConfig,
        bwe_sender: Option<mpsc::Sender<ReceiveBandwidthEstimate>>,
    ) -> MediaResult<TransportInfo> {
        config
            .validate()
            .map_err(|error| MediaError::ConfigurationError(error.to_string()))?;
        let participant_lock = self.get_participant_lock(participant_id)?;
        let mut participant = measure(Stage::SessionLockWait, participant_lock.lock()).await;
        if !self.participant_is_current(participant_id, &participant_lock) {
            return Err(MediaError::InvalidState(
                "Media session changed".to_string(),
            ));
        }
        if let Some(replay) = Self::replay_recv_replacement(&participant, expected)? {
            return Ok(replay);
        }
        let transport =
            Self::build_recv_transport(&placement.router, placement.webrtc_server, config).await?;
        let handler = match bwe_sender {
            Some(sender) => Some(Self::bwe_trace_handler(&transport, sender).await?),
            None => None,
        };
        if !self.participant_is_current(participant_id, &participant_lock) || transport.closed() {
            return Err(MediaError::InvalidState(
                "Media session changed".to_string(),
            ));
        }
        let info = TransportInfo::from(&transport);
        self.setup_transport_handlers(&transport, participant_id, "recv");
        self.clear_bwe_trace_handler(participant.generation);
        participant.consumers.clear();
        participant.consumer_layers.clear();
        participant.bandwidth_spatial_ceiling = None;
        participant.recv_connect_applied = None;
        participant.recv_replacement = Some((expected, transport.id()));
        let retired = participant.recv_transport.replace(transport);
        if let Some(lease) = placement.lease {
            participant.viewer_lease = Some(Arc::new(lease));
        } else if !placement.retain_existing_lease {
            participant.viewer_lease = None;
        }
        if let Some(handler) = handler {
            self.replace_bwe_trace_handler(participant.generation, handler);
        }
        drop(retired);
        Ok(info)
    }

    /// No IPC: used to fence queued bandwidth events and cached stats.
    pub(crate) async fn is_current_recv_transport(
        &self,
        participant_id: &str,
        expected: TransportId,
    ) -> bool {
        let Ok(participant_lock) = self.get_participant_lock(participant_id) else {
            return false;
        };
        let participant = participant_lock.lock().await;
        self.participant_is_current(participant_id, &participant_lock)
            && participant
                .recv_transport
                .as_ref()
                .is_some_and(|transport| transport.id() == expected && !transport.closed())
    }

    /// The options every transport of this server shares: the listener, the
    /// transports' families and preferences and the starting estimate.
    fn transport_options(
        webrtc_server: WebRtcServer,
        config: &WebRtcTransportConfig,
    ) -> WebRtcTransportOptions {
        let mut options = WebRtcTransportOptions::new_with_server(webrtc_server);
        options.initial_available_outgoing_bitrate =
            u64::from(config.initial_available_outgoing_bitrate);
        options.enable_udp = config.enable_udp;
        options.enable_tcp = config.enable_tcp;
        options.prefer_udp = config.prefer_udp;
        options.prefer_tcp = config.prefer_tcp;
        options
    }

    async fn build_recv_transport(
        router: &Router,
        webrtc_server: WebRtcServer,
        config: &WebRtcTransportConfig,
    ) -> MediaResult<WebRtcTransport> {
        let transport_options = Self::transport_options(webrtc_server, config);
        measure_result(Stage::MediaCreateTransport, async {
            let transport = router
                .create_webrtc_transport(transport_options)
                .await
                .map_err(|e| {
                    MediaError::TransportError(format!("Failed to create receive transport: {e}"))
                })?;

            transport
                .set_max_outgoing_bitrate(u64::from(config.max_outgoing_bitrate))
                .await
                .map_err(|error| {
                    MediaError::TransportError(format!(
                        "Failed to apply outgoing bitrate limit: {error}"
                    ))
                })?;
            transport
                .set_min_outgoing_bitrate(u64::from(config.min_outgoing_bitrate))
                .await
                .map_err(|error| {
                    MediaError::TransportError(format!(
                        "Failed to apply minimum outgoing bitrate: {error}"
                    ))
                })?;

            Ok::<_, MediaError>(transport)
        })
        .await
    }

    /// Connects a transport by ID (determines send vs recv automatically).
    /// Returns whether this call performed worker IPC; retries after a lost
    /// signaling acknowledgement are successful no-ops once native connect has
    /// acknowledged this transport's remote parameters. This is independent of
    /// ICE/DTLS readiness, including ICE that arrives before the first request.
    pub async fn connect_transport(
        &self,
        participant_id: &str,
        transport_id: TransportId,
        dtls_parameters: DtlsParameters,
    ) -> MediaResult<bool> {
        let participant_lock = self.get_participant_lock(participant_id)?;
        let mut participant = measure(Stage::SessionLockWait, participant_lock.lock()).await;

        let (transport, is_send) = participant
            .send_transport
            .as_ref()
            .filter(|transport| transport.id() == transport_id)
            .map(|transport| (transport, true))
            .or_else(|| {
                participant
                    .recv_transport
                    .as_ref()
                    .filter(|transport| transport.id() == transport_id)
                    .map(|transport| (transport, false))
            })
            .ok_or_else(|| {
                MediaError::TransportError(format!("Transport not found: {transport_id}"))
            })?;

        let state = transport.dtls_state();
        if transport.closed() || matches!(state, DtlsState::Failed | DtlsState::Closed) {
            return Err(MediaError::InvalidState(format!(
                "Transport is closed or cannot connect from DTLS state {state:?}"
            )));
        }
        let native_id = transport.id();
        let applied = if is_send {
            participant.send_connect_applied
        } else {
            participant.recv_connect_applied
        };
        if applied == Some(native_id) {
            return Ok(false);
        }

        // Native ICE can start an AUTO-role DTLS handshake before signaling
        // supplies the peer fingerprint. Skipping connect merely because its
        // state is Connecting leaves that handshake unauthenticated indefinitely.
        measure_result(
            Stage::MediaConnectTransport,
            transport.connect(WebRtcTransportRemoteParameters { dtls_parameters }),
        )
        .await
        .map_err(|e| MediaError::TransportError(format!("Failed to connect transport: {e}")))?;

        // Keep the participant lock through the acknowledgment and bookkeeping.
        // Failed requests must not turn later attempts into successful no-ops.
        if is_send {
            participant.send_connect_applied = Some(native_id);
        } else {
            participant.recv_connect_applied = Some(native_id);
        }

        info!(
            "Connected transport {} for participant {}",
            transport_id, participant_id
        );
        Ok(true)
    }

    /// Creates a producer on the participant's send transport
    pub async fn create_producer(
        &self,
        participant_id: &str,
        transport_id: TransportId,
        kind: MediaKind,
        rtp_parameters: RtpParameters,
        app_data: AppData,
    ) -> MediaResult<Producer> {
        let participant_lock = self.get_participant_lock(participant_id)?;
        let mut participant = measure(Stage::SessionLockWait, participant_lock.lock()).await;

        // A producer the worker closed (transport failure, native teardown)
        // never reaches close_producer, so it leaves the session here rather
        // than counting against the cap for the process lifetime.
        participant
            .producers
            .retain(|_, producer| !producer.closed());
        let producer_cap = self.producer_cap;
        if participant.producers.len() >= producer_cap {
            return Err(MediaError::ProducerError(format!(
                "Producer limit reached ({producer_cap})"
            )));
        }

        let transport = participant
            .send_transport
            .as_ref()
            .filter(|transport| transport.id() == transport_id)
            .ok_or_else(|| {
                MediaError::TransportError(format!("Transport not found: {transport_id}"))
            })?;

        let mut producer_options = ProducerOptions::new(kind, rtp_parameters);
        producer_options.app_data = app_data;
        if kind == MediaKind::Video {
            producer_options.key_frame_request_delay = self.key_frame_request_delay;
        }

        let producer = measure_result(Stage::MediaProduce, transport.produce(producer_options))
            .await
            .map_err(|e| MediaError::ProducerError(format!("Failed to create producer: {e}")))?;

        let producer_id = producer.id();
        self.setup_producer_handlers(&producer, participant_id);
        participant.producers.insert(producer_id, producer.clone());

        info!(
            "Created {:?} producer {} for participant {}",
            kind, producer_id, participant_id
        );
        Ok(producer)
    }

    /// Creates a paused consumer on the participant's receive transport.
    /// The client must configure its local consumer before acknowledging with resume_consumer.
    pub async fn create_consumer(
        &self,
        participant_id: &str,
        producer_id: ProducerId,
        rtp_capabilities: RtpCapabilities,
        app_data: AppData,
        sender: Option<mpsc::Sender<crate::OutboundJson>>,
        consumer_counter: Option<Arc<AtomicUsize>>,
    ) -> MediaResult<Consumer> {
        let participant_lock = self.get_participant_lock(participant_id)?;
        let mut participant = measure(Stage::SessionLockWait, participant_lock.lock()).await;
        let participant = &mut *participant;

        participant
            .consumers
            .retain(|_, consumer| !consumer.closed());
        let live = &participant.consumers;
        participant
            .consumer_layers
            .retain(|id, _| live.contains_key(id));
        let consumer_cap = self.consumer_cap;
        if participant.consumers.len() >= consumer_cap {
            return Err(MediaError::ConsumerError(format!(
                "Consumer limit reached ({consumer_cap})"
            )));
        }

        let transport = participant
            .recv_transport
            .as_ref()
            .ok_or_else(|| MediaError::TransportError("Receive transport not found".to_string()))?;

        let mut consumer_options = ConsumerOptions::new(producer_id, rtp_capabilities);
        consumer_options.app_data = app_data;
        // RTP must not reach the browser before its SDP is ready, even when
        // the producer is active. Producer pause state is tracked separately.
        consumer_options.paused = true;
        // A consumer created while the bandwidth tier is low starts under it;
        // the worker ignores the field for consumers without layers.
        if let Some(tier) = participant.bandwidth_spatial_ceiling {
            consumer_options.preferred_layers = Some(ConsumerLayers {
                spatial_layer: tier,
                temporal_layer: None,
            });
        }

        let consumer = measure_result(Stage::MediaConsume, transport.consume(consumer_options))
            .await
            .map_err(|e| MediaError::ConsumerError(format!("Failed to create consumer: {e}")))?;

        // Priority divides this viewer's estimated video bandwidth. Audio is
        // independent, and all existing explicit and bandwidth layer ceilings
        // still apply. Configure before publishing/counting the consumer so a
        // failed native request cannot leave an untracked active subscription.
        let priority = consumer_video_priority(consumer.kind(), consumer.app_data());
        if priority != 1 {
            consumer.set_priority(priority).await.map_err(|error| {
                MediaError::ConsumerError(format!("Failed to prioritize presentation: {error}"))
            })?;
        }

        let consumer_id = consumer.id();

        // Increment the worker's consumer count
        if let Some(ref counter) = consumer_counter {
            counter.fetch_add(1, Ordering::Relaxed);
        }

        self.setup_consumer_handlers(&consumer, participant_id, sender, consumer_counter);
        if let Some(top) = layered_top(&consumer) {
            participant
                .consumer_layers
                .insert(consumer_id, ConsumerLayerState { top, client: None });
        }
        participant.consumers.insert(consumer_id, consumer.clone());

        info!(
            "Created consumer {} for producer {} and participant {}",
            consumer_id, producer_id, participant_id
        );
        Ok(consumer)
    }

    /// Resumes a consumer for a participant, returning whether worker state changed.
    pub async fn resume_consumer(
        &self,
        participant_id: &str,
        consumer_id: ConsumerId,
    ) -> MediaResult<bool> {
        let participant_lock = self.get_participant_lock(participant_id)?;
        let participant = measure(Stage::SessionLockWait, participant_lock.lock()).await;

        let consumer = participant.consumers.get(&consumer_id).ok_or_else(|| {
            MediaError::ConsumerError(format!("Consumer not found: {consumer_id}"))
        })?;

        if !consumer_pause_transition_needed(consumer.paused(), false) {
            return Ok(false);
        }

        measure_result(Stage::MediaResume, consumer.resume())
            .await
            .map_err(|e| MediaError::ConsumerError(format!("Failed to resume consumer: {e}")))?;

        info!(
            "Resumed consumer {} for participant {}",
            consumer_id, participant_id
        );
        Ok(true)
    }

    /// Pauses a consumer for a participant, returning whether worker state changed.
    pub async fn pause_consumer(
        &self,
        participant_id: &str,
        consumer_id: ConsumerId,
    ) -> MediaResult<bool> {
        let participant_lock = self.get_participant_lock(participant_id)?;
        let participant = participant_lock.lock().await;

        let consumer = participant.consumers.get(&consumer_id).ok_or_else(|| {
            MediaError::ConsumerError(format!("Consumer not found: {consumer_id}"))
        })?;

        if !consumer_pause_transition_needed(consumer.paused(), true) {
            return Ok(false);
        }

        consumer
            .pause()
            .await
            .map_err(|e| MediaError::ConsumerError(format!("Failed to pause consumer: {e}")))?;

        info!(
            "Paused consumer {} for participant {}",
            consumer_id, participant_id
        );
        Ok(true)
    }

    /// Pauses a producer for a participant
    pub async fn pause_producer(
        &self,
        participant_id: &str,
        producer_id: ProducerId,
    ) -> MediaResult<bool> {
        let participant_lock = self.get_participant_lock(participant_id)?;
        let participant = participant_lock.lock().await;

        let producer = participant.producers.get(&producer_id).ok_or_else(|| {
            MediaError::ProducerError(format!("Producer not found: {producer_id}"))
        })?;

        if !producer_pause_transition_needed(producer.paused(), true) {
            return Ok(false);
        }

        producer
            .pause()
            .await
            .map_err(|e| MediaError::ProducerError(format!("Failed to pause producer: {e}")))?;

        info!(
            "Paused producer {} for participant {}",
            producer_id, participant_id
        );
        Ok(true)
    }

    /// Resumes a producer for a participant
    pub async fn resume_producer(
        &self,
        participant_id: &str,
        producer_id: ProducerId,
    ) -> MediaResult<bool> {
        let participant_lock = self.get_participant_lock(participant_id)?;
        let participant = measure(Stage::SessionLockWait, participant_lock.lock()).await;

        let producer = participant.producers.get(&producer_id).ok_or_else(|| {
            MediaError::ProducerError(format!("Producer not found: {producer_id}"))
        })?;

        if !producer_pause_transition_needed(producer.paused(), false) {
            return Ok(false);
        }

        measure_result(Stage::MediaResume, producer.resume())
            .await
            .map_err(|e| MediaError::ProducerError(format!("Failed to resume producer: {e}")))?;

        info!(
            "Resumed producer {} for participant {}",
            producer_id, participant_id
        );
        Ok(true)
    }

    /// Records the viewer's own layer ceiling for a consumer (tile size or
    /// manual quality) and writes the merged ceiling, returning whether the
    /// worker was asked to change. Consumers without layers are left alone
    /// without a worker request.
    pub async fn set_preferred_layers(
        &self,
        participant_id: &str,
        consumer_id: ConsumerId,
        layers: ConsumerLayers,
    ) -> MediaResult<bool> {
        let participant_lock = self.get_participant_lock(participant_id)?;
        let mut participant = participant_lock.lock().await;
        let participant = &mut *participant;

        let consumer = participant.consumers.get(&consumer_id).ok_or_else(|| {
            MediaError::ConsumerError(format!("Consumer not found: {consumer_id}"))
        })?;
        let Some(state) = participant.consumer_layers.get_mut(&consumer_id) else {
            return Ok(false);
        };
        state.client = Some(layers);
        Self::write_effective_layers(
            consumer,
            *state,
            participant.bandwidth_spatial_ceiling,
            participant_id,
        )
        .await
    }

    /// Records the receive transport's bandwidth tier as a spatial ceiling for
    /// every layered consumer and returns, sorted, the consumers whose applied
    /// layers now differ; `apply_layer_ceilings` writes each one. Nothing is
    /// sent to the worker here.
    pub async fn set_bandwidth_ceiling(
        &self,
        participant_id: &str,
        spatial_layer: u8,
    ) -> MediaResult<Vec<ConsumerId>> {
        self.set_bandwidth_ceiling_for_transport(participant_id, None, spatial_layer)
            .await
    }

    pub(crate) async fn set_bandwidth_ceiling_for_transport(
        &self,
        participant_id: &str,
        expected: Option<TransportId>,
        spatial_layer: u8,
    ) -> MediaResult<Vec<ConsumerId>> {
        let participant_lock = self.get_participant_lock(participant_id)?;
        let mut participant = participant_lock.lock().await;
        if let Some(expected) = expected
            && !participant
                .recv_transport
                .as_ref()
                .is_some_and(|transport| transport.id() == expected && !transport.closed())
        {
            return Err(MediaError::InvalidState(
                "Receive transport changed".to_string(),
            ));
        }
        participant.bandwidth_spatial_ceiling = Some(spatial_layer);
        let mut pending: Vec<ConsumerId> = participant
            .consumer_layers
            .iter()
            .filter(|(id, state)| {
                participant
                    .consumers
                    .get(*id)
                    .filter(|consumer| !consumer.closed())
                    .is_some_and(|consumer| {
                        consumer_layers_transition_needed(
                            consumer.preferred_layers(),
                            effective_layers(state.top, state.client, Some(spatial_layer)),
                        )
                    })
            })
            .map(|(id, _)| *id)
            .collect();
        pending.sort();
        Ok(pending)
    }

    /// Writes the merged ceiling of one consumer to the worker if it differs
    /// from the applied one, returning whether a request was made.
    pub async fn apply_layer_ceilings(
        &self,
        participant_id: &str,
        consumer_id: ConsumerId,
    ) -> MediaResult<bool> {
        let participant_lock = self.get_participant_lock(participant_id)?;
        let participant = participant_lock.lock().await;
        let consumer = participant.consumers.get(&consumer_id).ok_or_else(|| {
            MediaError::ConsumerError(format!("Consumer not found: {consumer_id}"))
        })?;
        let Some(state) = participant.consumer_layers.get(&consumer_id) else {
            return Ok(false);
        };
        Self::write_effective_layers(
            consumer,
            *state,
            participant.bandwidth_spatial_ceiling,
            participant_id,
        )
        .await
    }

    async fn write_effective_layers(
        consumer: &Consumer,
        state: ConsumerLayerState,
        bandwidth_spatial: Option<u8>,
        participant_id: &str,
    ) -> MediaResult<bool> {
        let target = effective_layers(state.top, state.client, bandwidth_spatial);
        if !consumer_layers_transition_needed(consumer.preferred_layers(), target) {
            return Ok(false);
        }
        consumer.set_preferred_layers(target).await.map_err(|e| {
            MediaError::ConsumerError(format!("Failed to set preferred layers: {e}"))
        })?;
        debug!(
            "Set preferred layers {:?} for consumer {} of participant {}",
            target,
            consumer.id(),
            participant_id
        );
        Ok(true)
    }

    /// Restarts ICE on a transport, returning new ICE parameters
    pub async fn restart_ice(
        &self,
        participant_id: &str,
        transport_id: TransportId,
    ) -> MediaResult<IceParameters> {
        let participant_lock = self.get_participant_lock(participant_id)?;
        let participant = participant_lock.lock().await;

        let transport = participant
            .send_transport
            .as_ref()
            .filter(|transport| transport.id() == transport_id)
            .or_else(|| {
                participant
                    .recv_transport
                    .as_ref()
                    .filter(|transport| transport.id() == transport_id)
            })
            .ok_or_else(|| {
                MediaError::TransportError(format!("Transport not found: {transport_id}"))
            })?;

        let ice_parameters = transport
            .restart_ice()
            .await
            .map_err(|e| MediaError::TransportError(format!("Failed to restart ICE: {e}")))?;

        info!(
            "Restarted ICE for transport {} of participant {}",
            transport_id, participant_id
        );
        Ok(ice_parameters)
    }

    /// Subscribes to BWE (bandwidth estimation) trace events on a participant's recv transport.
    /// Events are sent through the provided channel for the stats task to process.
    pub async fn subscribe_bwe_events(
        &self,
        participant_id: &str,
        bwe_sender: mpsc::Sender<ReceiveBandwidthEstimate>,
    ) -> MediaResult<()> {
        let participant_lock = self.get_participant_lock(participant_id)?;
        let participant = participant_lock.lock().await;

        let transport = participant
            .recv_transport
            .as_ref()
            .ok_or_else(|| MediaError::TransportError("Recv transport not found".to_string()))?;

        let handler = Self::bwe_trace_handler(transport, bwe_sender).await?;
        self.replace_bwe_trace_handler(participant.generation, handler);

        info!(
            "Subscribed to BWE events for participant {}",
            participant_id
        );
        Ok(())
    }

    async fn bwe_trace_handler(
        transport: &WebRtcTransport,
        bwe_sender: mpsc::Sender<ReceiveBandwidthEstimate>,
    ) -> MediaResult<Box<dyn Any + Send + Sync>> {
        transport
            .enable_trace_event(vec![TransportTraceEventType::Bwe])
            .await
            .map_err(|e| {
                MediaError::TransportError(format!("Failed to enable trace events: {e}"))
            })?;

        // Register callback to forward BWE events through the channel
        let transport_id = transport.id();
        let handler = transport.on_trace(Arc::new(move |event: &TransportTraceEventData| {
            if let TransportTraceEventData::Bwe { info, .. } = event {
                let bitrate = application_bitrate(info.available_bitrate);
                let _ = bwe_sender.try_send(ReceiveBandwidthEstimate {
                    transport_id,
                    bitrate,
                });
                debug!(
                    bwe_type = ?info.r#type,
                    available_bitrate = info.available_bitrate,
                    application_bitrate = bitrate,
                    desired_bitrate = info.desired_bitrate,
                    effective_desired_bitrate = info.effective_desired_bitrate,
                    min_bitrate = info.min_bitrate,
                    max_bitrate = info.max_bitrate,
                    start_bitrate = info.start_bitrate,
                    max_padding_bitrate = info.max_padding_bitrate,
                    "Receive bandwidth estimate"
                );
            }
        }));
        Ok(Box::new(handler))
    }

    /// One media quality sample over every participant. Consumer and producer
    /// scores and current layers are read from the crate's cached state (no
    /// IPC); at most `max_transport_stats` receive transports are asked for
    /// their statistics per call, continuing round-robin from `cursor`, so a
    /// large room set is covered over several samples without a request burst.
    pub async fn quality_sample(
        &self,
        cursor: &mut usize,
        max_transport_stats: usize,
    ) -> crate::media::quality::QualitySample {
        use crate::media::quality::{QualitySample, TransportReading, collect_transport_stats};
        use std::time::Duration;
        // One budget covers lock inspection and worker IPC. Busy participants
        // are skipped immediately: diagnostic work must never queue behind a
        // media operation. At most eight worker requests run concurrently.
        let started = std::time::Instant::now();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
        let participants: Vec<Arc<TokioMutex<ParticipantMedia>>> = {
            let map = self.participants.read();
            let mut ids: Vec<&String> = map.keys().collect();
            ids.sort();
            ids.into_iter()
                .filter_map(|id| map.get(id).cloned())
                .collect()
        };
        let mut sample = QualitySample {
            participants_available: participants.len() as u64,
            ..QualitySample::default()
        };
        let mut transports = Vec::new();
        for participant in &participants {
            if tokio::time::Instant::now() >= deadline {
                sample.budget_exhausted = true;
                break;
            }
            let Ok(participant) = participant.try_lock() else {
                continue;
            };
            sample.participants_sampled += 1;
            for consumer in participant.consumers.values() {
                if consumer.closed() {
                    continue;
                }
                sample.record_consumer(
                    consumer.score().score,
                    consumer.current_layers().map(|layers| layers.spatial_layer),
                    consumer.kind() == MediaKind::Video,
                    consumer.paused() || consumer.producer_paused(),
                );
            }
            for producer in participant.producers.values() {
                if !producer.closed() {
                    sample.record_producer(producer.score().iter().map(|score| score.score));
                }
            }
            if let Some(transport) = participant.recv_transport.as_ref().filter(|t| !t.closed()) {
                transports.push(transport.clone());
            }
        }
        let total = transports.len();
        let take = max_transport_stats.min(total);
        let start = if total > 0 { *cursor % total } else { 0 };
        sample.transports_available = total as u64;
        sample.transports_selected = take as u64;
        let requests = (0..take).map(|offset| {
            let transport = transports[(start + offset) % total].clone();
            async move {
                let stats = transport.get_stats().await.ok()?;
                let stat = stats.first()?;
                // Reuse this sampler's existing bounded IPC request. Absolute
                // counters permit probe deltas without per-packet trace events
                // or retaining transport/participant identifiers.
                debug!(
                    timestamp_ms = stat.timestamp,
                    send_bitrate = stat.send_bitrate,
                    rtp_bytes_sent = stat.rtp_bytes_sent,
                    rtp_send_bitrate = stat.rtp_send_bitrate,
                    rtx_bytes_sent = stat.rtx_bytes_sent,
                    rtx_send_bitrate = stat.rtx_send_bitrate,
                    probation_bytes_sent = stat.probation_bytes_sent,
                    probation_send_bitrate = stat.probation_send_bitrate,
                    available_outgoing_bitrate = stat.available_outgoing_bitrate,
                    rtp_packet_loss_sent = stat.rtp_packet_loss_sent,
                    "Receive transport bandwidth sample"
                );
                Some(TransportReading {
                    loss_sent: stat.rtp_packet_loss_sent,
                    available_outgoing_bitrate: stat
                        .available_outgoing_bitrate
                        .map(application_bitrate),
                })
            }
        });
        collect_transport_stats(&mut sample, requests, deadline).await;
        // Advance by work actually started, including failures, for fair retry.
        *cursor = start + sample.transports_requested as usize;
        sample.duration = started.elapsed();
        sample.completed_at = Some(std::time::Instant::now());
        sample.completed_wall_time = Some(std::time::SystemTime::now());
        sample
    }

    /// Releases a consumer and its layer state within this participant's session.
    /// Unknown IDs are harmless no-ops, including after the producer closed it.
    pub async fn close_consumer(
        &self,
        participant_id: &str,
        consumer_id: ConsumerId,
    ) -> MediaResult<bool> {
        let participant_lock = self.get_participant_lock(participant_id)?;
        let mut participant = participant_lock.lock().await;
        participant.consumer_layers.remove(&consumer_id);
        Ok(participant.consumers.remove(&consumer_id).is_some())
    }

    /// Closes a producer for a participant
    pub async fn close_producer(
        &self,
        participant_id: &str,
        producer_id: ProducerId,
    ) -> MediaResult<()> {
        let participant_lock = self.get_participant_lock(participant_id)?;
        let mut participant = participant_lock.lock().await;

        let _producer = participant.producers.remove(&producer_id).ok_or_else(|| {
            MediaError::ProducerError(format!("Producer not found: {producer_id}"))
        })?;

        info!(
            "Closed producer {} for participant {}",
            producer_id, participant_id
        );
        Ok(())
    }

    /// Returns a snapshot containing cloned media handles, not an exclusive
    /// owner. Fails if the session namespace is absent; callers must revalidate
    /// membership before mutating resources from the snapshot.
    pub async fn get_participant(&self, participant_id: &str) -> MediaResult<ParticipantMedia> {
        let participant_lock = self.get_participant_lock(participant_id)?;
        let participant = participant_lock.lock().await;
        Ok(participant.clone())
    }

    /// Evicts this session namespace, then closes its media under its own lock.
    /// A second removal returns `ParticipantNotFound`; an old generation's
    /// cleanup cannot erase a replacement generation's BWE callback.
    /// Records where the participant's receive transport was placed. The lease
    /// lives with the participant's media, so every removal path forgets it.
    pub async fn attach_viewer_lease(
        &self,
        participant_id: &str,
        lease: crate::media::router_manager::ViewerLease,
    ) -> MediaResult<()> {
        let participant_lock = self.get_participant_lock(participant_id)?;
        let mut participant = participant_lock.lock().await;
        participant.viewer_lease = Some(Arc::new(lease));
        Ok(())
    }

    pub async fn remove_participant(&self, participant_id: &str) -> MediaResult<()> {
        // Remove from outer map (brief write lock)
        let participant_lock = {
            let mut participants = self.participants.write();
            participants.remove(participant_id)
        };

        if let Some(lock) = participant_lock {
            let mut participant = lock.lock().await;
            let lifecycle_id = if tracing::enabled!(target: "simplestchat::lifecycle", tracing::Level::DEBUG)
            {
                lifecycle_participant_id(participant_id)
            } else {
                None
            };
            if let Some(lifecycle_id) = lifecycle_id {
                debug!(
                    target: "simplestchat::lifecycle",
                    event = "media_cleanup_started",
                    participant_id = %lifecycle_id,
                    generation = %participant.generation,
                    "lifecycle"
                );
            }

            participant.close_all();
            // Clear by this media state's unique lifetime, not the reusable
            // participant ID. Holding its mutex orders this after any in-flight
            // subscription on the old receive transport.
            self.clear_bwe_trace_handler(participant.generation);
            // This marks completion of application-owned handle drops, not a
            // worker close acknowledgement or the end of other cloned handles.
            if let Some(lifecycle_id) = lifecycle_id {
                debug!(
                    target: "simplestchat::lifecycle",
                    event = "media_cleanup_finished",
                    participant_id = %lifecycle_id,
                    generation = %participant.generation,
                    "lifecycle"
                );
            }
            info!(
                "Removed participant {} and closed all media resources",
                participant_id
            );
            Ok(())
        } else {
            Err(MediaError::ParticipantNotFound(participant_id.to_string()))
        }
    }

    /// Sets up event handlers for a transport.
    /// Handlers are detached so they persist for the transport's lifetime.
    fn setup_transport_handlers(
        &self,
        transport: &WebRtcTransport,
        participant_id: &str,
        transport_type: &'static str,
    ) {
        let lifecycle_id = if tracing::enabled!(target: "simplestchat::lifecycle", tracing::Level::DEBUG)
        {
            lifecycle_participant_id(participant_id)
        } else {
            None
        };
        let participant_id = participant_id.to_string();
        let transport_id = transport.id();

        if let Some(lifecycle_id) = lifecycle_id {
            debug!(
                target: "simplestchat::lifecycle",
                event = "transport_created",
                participant_id = %lifecycle_id,
                transport_id = %transport_id,
                transport_type,
                "lifecycle"
            );
        }

        transport
            .on_close(Box::new({
                // Capture identifiers only: a strong transport handle here
                // would retain the transport whose close we need to observe.
                move || {
                    if let Some(lifecycle_id) = lifecycle_id {
                        debug!(
                            target: "simplestchat::lifecycle",
                            event = "transport_closed",
                            participant_id = %lifecycle_id,
                            transport_id = %transport_id,
                            transport_type,
                            "lifecycle"
                        );
                    }
                    debug!("Media transport closed");
                }
            }))
            .detach();

        transport
            .on_dtls_state_change({
                let participant_id = participant_id.clone();
                move |dtls_state| {
                    if let Some(lifecycle_id) = lifecycle_id {
                        debug!(
                            target: "simplestchat::lifecycle",
                            event = "transport_dtls",
                            participant_id = %lifecycle_id,
                            transport_id = %transport_id,
                            transport_type,
                            state = ?dtls_state,
                            "lifecycle"
                        );
                    }
                    info!(
                        "DTLS state: {:?} for {} transport {} (participant {})",
                        dtls_state, transport_type, transport_id, participant_id
                    );
                }
            })
            .detach();

        transport
            .on_ice_state_change({
                move |ice_state| {
                    if let Some(lifecycle_id) = lifecycle_id {
                        debug!(
                            target: "simplestchat::lifecycle",
                            event = "transport_ice",
                            participant_id = %lifecycle_id,
                            transport_id = %transport_id,
                            transport_type,
                            state = ?ice_state,
                            "lifecycle"
                        );
                    }
                    info!(
                        "ICE state: {:?} for {} transport {} (participant {})",
                        ice_state, transport_type, transport_id, participant_id
                    );
                }
            })
            .detach();
    }

    /// Sets up event handlers for a producer.
    /// Handlers are detached so they persist for the producer's lifetime.
    fn setup_producer_handlers(&self, producer: &Producer, participant_id: &str) {
        let participant_id = participant_id.to_string();
        let producer_id = producer.id();

        producer
            .on_close(|| {
                debug!("Media producer closed");
            })
            .detach();

        producer
            .on_pause({
                let participant_id = participant_id.clone();
                move || {
                    debug!(
                        "Producer {} paused for participant {}",
                        producer_id, participant_id
                    );
                }
            })
            .detach();

        producer
            .on_resume({
                move || {
                    debug!(
                        "Producer {} resumed for participant {}",
                        producer_id, participant_id
                    );
                }
            })
            .detach();
    }

    /// Sets up event handlers for a consumer.
    /// Handlers are detached so they persist for the consumer's lifetime.
    fn setup_consumer_handlers(
        &self,
        consumer: &Consumer,
        participant_id: &str,
        sender: Option<mpsc::Sender<crate::OutboundJson>>,
        consumer_counter: Option<Arc<AtomicUsize>>,
    ) {
        let participant_id = participant_id.to_string();
        let consumer_id = consumer.id();

        consumer
            .on_close({
                let counter = consumer_counter;
                move || {
                    if let Some(ref c) = counter {
                        c.fetch_sub(1, Ordering::Relaxed);
                    }
                    debug!("Media consumer closed");
                }
            })
            .detach();

        consumer
            .on_pause({
                let participant_id = participant_id.clone();
                move || {
                    debug!(
                        "Consumer {} paused for participant {}",
                        consumer_id, participant_id
                    );
                }
            })
            .detach();

        consumer
            .on_resume({
                let participant_id = participant_id.clone();
                move || {
                    debug!(
                        "Consumer {} resumed for participant {}",
                        consumer_id, participant_id
                    );
                }
            })
            .detach();

        consumer
            .on_producer_pause({
                let participant_id = participant_id.clone();
                move || {
                    debug!(
                        "Producer paused for consumer {} of participant {}",
                        consumer_id, participant_id
                    );
                }
            })
            .detach();

        consumer
            .on_producer_resume({
                let participant_id = participant_id.clone();
                move || {
                    debug!(
                        "Producer resumed for consumer {} of participant {}",
                        consumer_id, participant_id
                    );
                }
            })
            .detach();

        // Notify client when active simulcast layers change
        if let Some(sender) = sender {
            consumer
                .on_layers_change({
                    let participant_id = participant_id.clone();
                    move |layers| {
                        debug!(
                            "Consumer {} layers changed to {:?} for participant {}",
                            consumer_id, layers, participant_id
                        );
                        let msg = ServerMessage::ConsumerLayersChanged {
                            consumer_id,
                            spatial_layer: layers.as_ref().map(|l| l.spatial_layer),
                            temporal_layer: layers.as_ref().and_then(|l| l.temporal_layer),
                        };
                        if let Ok(json) = serde_json::to_string(&msg) {
                            let _ = sender.try_send(crate::OutboundJson::from(json));
                        }
                    }
                })
                .detach();
        }
    }

    /// Evicts all managed sessions and closes their media. Repeating after a
    /// completed drain succeeds. The shutdown coordinator must first stop new
    /// admissions and bound this future: per-session IPC/locks can stall.
    pub async fn close_all(&self) -> Result<()> {
        info!("Closing all transports");

        let all_participants: Vec<(String, Arc<TokioMutex<ParticipantMedia>>)> = {
            let mut participants = self.participants.write();
            participants.drain().collect()
        };

        for (participant_id, lock) in all_participants {
            let mut participant = lock.lock().await;
            participant.close_all();
            self.clear_bwe_trace_handler(participant.generation);
            debug!("Closed all transports for participant: {}", participant_id);
        }

        Ok(())
    }
}

#[cfg(test)]
#[path = "transport_manager_tests.rs"]
mod tests;
