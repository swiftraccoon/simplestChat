#![forbid(unsafe_code)]

// Transport management for WebRTC connections

use crate::diagnostics::{Stage, measure, measure_result};
use crate::media::config::WebRtcTransportConfig;
use crate::media::types::{
    ConsumerLayerState, MediaError, MediaResult, ParticipantMedia, TransportInfo,
};
use crate::signaling::protocol::ServerMessage;
use anyhow::Result;
use mediasoup::consumer::ConsumerType;
use mediasoup::prelude::*;
use mediasoup::transport::{TransportTraceEventData, TransportTraceEventType};
use mediasoup_types::data_structures::DtlsState;
use std::any::Any;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::RwLock as StdRwLock;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::Mutex as TokioMutex;
use tokio::sync::mpsc;
use tracing::{debug, info};
use uuid::Uuid;

fn max_producers_per_participant() -> usize {
    std::env::var("MAX_PRODUCERS_PER_PARTICIPANT")
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|value| *value > 0)
        .unwrap_or(8)
}

pub(crate) fn max_consumers_per_participant() -> usize {
    consumer_cap_from(
        std::env::var("MAX_CONSUMERS_PER_PARTICIPANT")
            .ok()
            .as_deref(),
    )
}

/// Per-participant consumer cap. Browsers consume every remote producer, so
/// a participant needs `2 * (N - 1)` consumers in an N-person room with
/// camera and microphone (four per peer with screen video and audio). The default
/// covers 32 such publishers; the previous value of 16 silently blanked the
/// tenth participant's tiles.
fn consumer_cap_from(configured: Option<&str>) -> usize {
    configured
        .and_then(|value| value.trim().parse().ok())
        .filter(|value| *value > 0)
        .unwrap_or(64)
}

fn producer_pause_transition_needed(currently_paused: bool, requested_paused: bool) -> bool {
    currently_paused != requested_paused
}

fn consumer_pause_transition_needed(currently_paused: bool, requested_paused: bool) -> bool {
    currently_paused != requested_paused
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

/// Manages WebRTC transports for participants.
///
/// Uses per-participant locking: the outer HashMap is protected by a std::sync::RwLock
/// (held only for brief lookups, never across await points), while each participant's
/// media state is protected by its own tokio::sync::Mutex (held across async operations
/// but only blocking that specific participant).
pub struct TransportManager {
    participants: Arc<StdRwLock<HashMap<String, Arc<TokioMutex<ParticipantMedia>>>>>,
    /// Index of producer paused state: producer_id -> paused.
    /// Maintained by pause_producer, resume_producer, close_producer, remove_participant.
    paused_producers: Arc<StdRwLock<HashMap<String, bool>>>,
    /// The concrete callback handle type is intentionally erased here because
    /// mediasoup does not re-export it. Retaining one handle per media-state
    /// keeps the callback active; replacing/dropping it unregisters the old
    /// reconnect subscription instead of leaking detached callbacks. A unique
    /// generation prevents cleanup of an old participant ID from touching a
    /// newly-created media state that happens to reuse that public ID.
    bwe_trace_handlers: Arc<StdRwLock<HashMap<Uuid, Box<dyn Any + Send + Sync>>>>,
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
            participants: Arc::new(StdRwLock::new(HashMap::new())),
            paused_producers: Arc::new(StdRwLock::new(HashMap::new())),
            bwe_trace_handlers: Arc::new(StdRwLock::new(HashMap::new())),
        }
    }

    fn replace_bwe_trace_handler(&self, generation: Uuid, handler: Box<dyn Any + Send + Sync>) {
        self.bwe_trace_handlers
            .write()
            .unwrap_or_else(|error| error.into_inner())
            .insert(generation, handler);
    }

    fn clear_bwe_trace_handler(&self, generation: Uuid) {
        self.bwe_trace_handlers
            .write()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&generation);
    }

    /// Gets or creates a participant entry (brief outer lock, no await)
    fn get_or_create_participant(&self, participant_id: &str) -> Arc<TokioMutex<ParticipantMedia>> {
        // Fast path: read lock
        {
            let participants = self.participants.read().unwrap_or_else(|e| e.into_inner());
            if let Some(p) = participants.get(participant_id) {
                return Arc::clone(p);
            }
        }
        // Slow path: write lock to insert
        let mut participants = self.participants.write().unwrap_or_else(|e| e.into_inner());
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
        let participants = self.participants.read().unwrap_or_else(|e| e.into_inner());
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
        let participants = self.participants.read().unwrap_or_else(|e| e.into_inner());
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
        let mut transport_options = WebRtcTransportOptions::new_with_server(webrtc_server);
        transport_options.initial_available_outgoing_bitrate =
            config.initial_available_outgoing_bitrate;
        transport_options.enable_udp = config.enable_udp;
        transport_options.enable_tcp = config.enable_tcp;
        transport_options.prefer_udp = config.prefer_udp;
        transport_options.prefer_tcp = config.prefer_tcp;
        let transport = measure_result(Stage::MediaCreateTransport, async {
            let transport = router
                .create_webrtc_transport(transport_options)
                .await
                .map_err(|e| {
                    MediaError::TransportError(format!("Failed to create send transport: {e}"))
                })?;

            if let Some(maximum) = config.max_incoming_bitrate {
                transport
                    .set_max_incoming_bitrate(maximum)
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
        let mut transport_options = WebRtcTransportOptions::new_with_server(webrtc_server);
        transport_options.initial_available_outgoing_bitrate =
            config.initial_available_outgoing_bitrate;
        transport_options.enable_udp = config.enable_udp;
        transport_options.enable_tcp = config.enable_tcp;
        transport_options.prefer_udp = config.prefer_udp;
        transport_options.prefer_tcp = config.prefer_tcp;
        let transport = measure_result(Stage::MediaCreateTransport, async {
            let transport = router
                .create_webrtc_transport(transport_options)
                .await
                .map_err(|e| {
                    MediaError::TransportError(format!("Failed to create receive transport: {e}"))
                })?;

            transport
                .set_max_outgoing_bitrate(config.max_outgoing_bitrate)
                .await
                .map_err(|error| {
                    MediaError::TransportError(format!(
                        "Failed to apply outgoing bitrate limit: {error}"
                    ))
                })?;
            transport
                .set_min_outgoing_bitrate(config.min_outgoing_bitrate)
                .await
                .map_err(|error| {
                    MediaError::TransportError(format!(
                        "Failed to apply minimum outgoing bitrate: {error}"
                    ))
                })?;

            Ok::<_, MediaError>(transport)
        })
        .await?;

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

        info!(
            "Created receive transport {} for participant {}",
            transport_info.id, participant_id
        );
        Ok(transport_info)
    }

    /// Connects a transport by ID (determines send vs recv automatically).
    /// Returns whether this call performed worker IPC; retries after a lost
    /// signaling acknowledgement are successful no-ops once native connect has
    /// acknowledged this transport's remote parameters. This is independent of
    /// ICE/DTLS readiness, including ICE that arrives before the first request.
    pub async fn connect_transport(
        &self,
        participant_id: &str,
        transport_id: &str,
        dtls_parameters: DtlsParameters,
    ) -> MediaResult<bool> {
        let participant_lock = self.get_participant_lock(participant_id)?;
        let mut participant = measure(Stage::SessionLockWait, participant_lock.lock()).await;

        let (transport, is_send) = participant
            .send_transport
            .as_ref()
            .filter(|transport| transport.id().to_string() == transport_id)
            .map(|transport| (transport, true))
            .or_else(|| {
                participant
                    .recv_transport
                    .as_ref()
                    .filter(|transport| transport.id().to_string() == transport_id)
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
        kind: MediaKind,
        rtp_parameters: RtpParameters,
        app_data: AppData,
    ) -> MediaResult<Producer> {
        let participant_lock = self.get_participant_lock(participant_id)?;
        let mut participant = measure(Stage::SessionLockWait, participant_lock.lock()).await;

        // A producer the worker closed (transport failure, native teardown)
        // never reaches close_producer, so its paused-index entry must leave
        // with it here rather than persisting for the process lifetime.
        let mut pruned = Vec::new();
        participant.producers.retain(|id, producer| {
            if producer.closed() {
                pruned.push(id.clone());
                false
            } else {
                true
            }
        });
        if !pruned.is_empty() {
            let mut index = self
                .paused_producers
                .write()
                .unwrap_or_else(|e| e.into_inner());
            for id in &pruned {
                index.remove(id);
            }
        }
        let producer_cap = max_producers_per_participant();
        if participant.producers.len() >= producer_cap {
            return Err(MediaError::ProducerError(format!(
                "Producer limit reached ({producer_cap})"
            )));
        }

        let transport = participant
            .send_transport
            .as_ref()
            .ok_or_else(|| MediaError::TransportError("Send transport not found".to_string()))?;

        let mut producer_options = ProducerOptions::new(kind, rtp_parameters);
        producer_options.app_data = app_data;

        let producer = measure_result(Stage::MediaProduce, transport.produce(producer_options))
            .await
            .map_err(|e| MediaError::ProducerError(format!("Failed to create producer: {e}")))?;

        let producer_id = producer.id().to_string();
        self.setup_producer_handlers(&producer, participant_id);
        participant
            .producers
            .insert(producer_id.clone(), producer.clone());

        // Initialize paused index (producers start unpaused)
        {
            let mut index = self
                .paused_producers
                .write()
                .unwrap_or_else(|e| e.into_inner());
            index.insert(producer_id.clone(), false);
        }

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
        let consumer_cap = max_consumers_per_participant();
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

        let consumer_id = consumer.id().to_string();

        // Increment the worker's consumer count
        if let Some(ref counter) = consumer_counter {
            counter.fetch_add(1, Ordering::Relaxed);
        }

        self.setup_consumer_handlers(&consumer, participant_id, sender, consumer_counter);
        if let Some(top) = layered_top(&consumer) {
            participant.consumer_layers.insert(
                consumer_id.clone(),
                ConsumerLayerState { top, client: None },
            );
        }
        participant
            .consumers
            .insert(consumer_id.clone(), consumer.clone());

        info!(
            "Created consumer {} for producer {} and participant {}",
            consumer_id, producer_id, participant_id
        );
        Ok(consumer)
    }

    /// Gets the number of consumers for a participant
    pub async fn consumer_count(&self, participant_id: &str) -> MediaResult<usize> {
        let participant_lock = self.get_participant_lock(participant_id)?;
        let mut participant = participant_lock.lock().await;
        participant
            .consumers
            .retain(|_, consumer| !consumer.closed());
        Ok(participant.consumers.len())
    }

    /// Resumes a consumer for a participant, returning whether worker state changed.
    pub async fn resume_consumer(
        &self,
        participant_id: &str,
        consumer_id: &str,
    ) -> MediaResult<bool> {
        let participant_lock = self.get_participant_lock(participant_id)?;
        let participant = measure(Stage::SessionLockWait, participant_lock.lock()).await;

        let consumer = participant.consumers.get(consumer_id).ok_or_else(|| {
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
        consumer_id: &str,
    ) -> MediaResult<bool> {
        let participant_lock = self.get_participant_lock(participant_id)?;
        let participant = participant_lock.lock().await;

        let consumer = participant.consumers.get(consumer_id).ok_or_else(|| {
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
        producer_id: &str,
    ) -> MediaResult<bool> {
        let participant_lock = self.get_participant_lock(participant_id)?;
        let participant = participant_lock.lock().await;

        let producer = participant.producers.get(producer_id).ok_or_else(|| {
            MediaError::ProducerError(format!("Producer not found: {producer_id}"))
        })?;

        if !producer_pause_transition_needed(producer.paused(), true) {
            return Ok(false);
        }

        producer
            .pause()
            .await
            .map_err(|e| MediaError::ProducerError(format!("Failed to pause producer: {e}")))?;

        // Update paused index
        {
            let mut index = self
                .paused_producers
                .write()
                .unwrap_or_else(|e| e.into_inner());
            index.insert(producer_id.to_string(), true);
        }

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
        producer_id: &str,
    ) -> MediaResult<bool> {
        let participant_lock = self.get_participant_lock(participant_id)?;
        let participant = measure(Stage::SessionLockWait, participant_lock.lock()).await;

        let producer = participant.producers.get(producer_id).ok_or_else(|| {
            MediaError::ProducerError(format!("Producer not found: {producer_id}"))
        })?;

        if !producer_pause_transition_needed(producer.paused(), false) {
            return Ok(false);
        }

        measure_result(Stage::MediaResume, producer.resume())
            .await
            .map_err(|e| MediaError::ProducerError(format!("Failed to resume producer: {e}")))?;

        // Update paused index
        {
            let mut index = self
                .paused_producers
                .write()
                .unwrap_or_else(|e| e.into_inner());
            index.insert(producer_id.to_string(), false);
        }

        info!(
            "Resumed producer {} for participant {}",
            producer_id, participant_id
        );
        Ok(true)
    }

    /// Checks if a producer is paused using the in-memory index.
    /// Returns `Some(paused)` if found, `None` if the producer doesn't exist.
    /// O(1) lookup — no per-participant locking needed.
    pub fn find_producer_paused(&self, producer_id: &str) -> Option<bool> {
        let index = self
            .paused_producers
            .read()
            .unwrap_or_else(|e| e.into_inner());
        index.get(producer_id).copied()
    }

    /// Records the viewer's own layer ceiling for a consumer (tile size or
    /// manual quality) and writes the merged ceiling, returning whether the
    /// worker was asked to change. Consumers without layers are left alone
    /// without a worker request.
    pub async fn set_preferred_layers(
        &self,
        participant_id: &str,
        consumer_id: &str,
        layers: ConsumerLayers,
    ) -> MediaResult<bool> {
        let participant_lock = self.get_participant_lock(participant_id)?;
        let mut participant = participant_lock.lock().await;
        let participant = &mut *participant;

        let consumer = participant.consumers.get(consumer_id).ok_or_else(|| {
            MediaError::ConsumerError(format!("Consumer not found: {consumer_id}"))
        })?;
        let Some(state) = participant.consumer_layers.get_mut(consumer_id) else {
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
    ) -> MediaResult<Vec<String>> {
        let participant_lock = self.get_participant_lock(participant_id)?;
        let mut participant = participant_lock.lock().await;
        participant.bandwidth_spatial_ceiling = Some(spatial_layer);
        let mut pending: Vec<String> = participant
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
            .map(|(id, _)| id.clone())
            .collect();
        pending.sort();
        Ok(pending)
    }

    /// Writes the merged ceiling of one consumer to the worker if it differs
    /// from the applied one, returning whether a request was made.
    pub async fn apply_layer_ceilings(
        &self,
        participant_id: &str,
        consumer_id: &str,
    ) -> MediaResult<bool> {
        let participant_lock = self.get_participant_lock(participant_id)?;
        let participant = participant_lock.lock().await;
        let consumer = participant.consumers.get(consumer_id).ok_or_else(|| {
            MediaError::ConsumerError(format!("Consumer not found: {consumer_id}"))
        })?;
        let Some(state) = participant.consumer_layers.get(consumer_id) else {
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
        transport_id: &str,
    ) -> MediaResult<IceParameters> {
        let participant_lock = self.get_participant_lock(participant_id)?;
        let participant = participant_lock.lock().await;

        let transport = participant
            .send_transport
            .as_ref()
            .filter(|transport| transport.id().to_string() == transport_id)
            .or_else(|| {
                participant
                    .recv_transport
                    .as_ref()
                    .filter(|transport| transport.id().to_string() == transport_id)
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
        bwe_sender: mpsc::Sender<u32>,
    ) -> MediaResult<()> {
        let participant_lock = self.get_participant_lock(participant_id)?;
        let participant = participant_lock.lock().await;

        let transport = participant
            .recv_transport
            .as_ref()
            .ok_or_else(|| MediaError::TransportError("Recv transport not found".to_string()))?;

        // Enable BWE trace events on the recv transport
        transport
            .enable_trace_event(vec![TransportTraceEventType::Bwe])
            .await
            .map_err(|e| {
                MediaError::TransportError(format!("Failed to enable trace events: {e}"))
            })?;

        // Register callback to forward BWE events through the channel
        let pid = participant_id.to_string();
        let handler = transport.on_trace(Arc::new(move |event: &TransportTraceEventData| {
            if let TransportTraceEventData::Bwe { info, .. } = event {
                let bitrate = info.available_bitrate;
                let _ = bwe_sender.try_send(bitrate);
                debug!("BWE event for {}: available_bitrate={}", pid, bitrate);
            }
        }));
        self.replace_bwe_trace_handler(participant.generation, Box::new(handler));

        info!(
            "Subscribed to BWE events for participant {}",
            participant_id
        );
        Ok(())
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
            let map = self.participants.read().unwrap_or_else(|e| e.into_inner());
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
                Some(TransportReading {
                    loss_sent: stat.rtp_packet_loss_sent,
                    available_outgoing_bitrate: stat.available_outgoing_bitrate,
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
        consumer_id: &str,
    ) -> MediaResult<bool> {
        let participant_lock = self.get_participant_lock(participant_id)?;
        let mut participant = participant_lock.lock().await;
        participant.consumer_layers.remove(consumer_id);
        Ok(participant.consumers.remove(consumer_id).is_some())
    }

    /// Closes a producer for a participant
    pub async fn close_producer(&self, participant_id: &str, producer_id: &str) -> MediaResult<()> {
        let participant_lock = self.get_participant_lock(participant_id)?;
        let mut participant = participant_lock.lock().await;

        let _producer = participant.producers.remove(producer_id).ok_or_else(|| {
            MediaError::ProducerError(format!("Producer not found: {producer_id}"))
        })?;

        // Clean up paused index
        {
            let mut index = self
                .paused_producers
                .write()
                .unwrap_or_else(|e| e.into_inner());
            index.remove(producer_id);
        }

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
            let mut participants = self.participants.write().unwrap_or_else(|e| e.into_inner());
            participants.remove(participant_id)
        };

        if let Some(lock) = participant_lock {
            let mut participant = lock.lock().await;
            let lifecycle_id = if tracing::enabled!(target: "simplestChat::lifecycle", tracing::Level::DEBUG)
            {
                lifecycle_participant_id(participant_id)
            } else {
                None
            };
            if let Some(lifecycle_id) = lifecycle_id {
                debug!(
                    target: "simplestChat::lifecycle",
                    event = "media_cleanup_started",
                    participant_id = %lifecycle_id,
                    generation = %participant.generation,
                    "lifecycle"
                );
            }

            // Clean up paused index for all this participant's producers
            {
                let mut index = self
                    .paused_producers
                    .write()
                    .unwrap_or_else(|e| e.into_inner());
                for producer_id in participant.producers.keys() {
                    index.remove(producer_id);
                }
            }

            participant.close_all().await;
            // Clear by this media state's unique lifetime, not the reusable
            // participant ID. Holding its mutex orders this after any in-flight
            // subscription on the old receive transport.
            self.clear_bwe_trace_handler(participant.generation);
            // This marks completion of application-owned handle drops, not a
            // worker close acknowledgement or the end of other cloned handles.
            if let Some(lifecycle_id) = lifecycle_id {
                debug!(
                    target: "simplestChat::lifecycle",
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
        let lifecycle_id = if tracing::enabled!(target: "simplestChat::lifecycle", tracing::Level::DEBUG)
        {
            lifecycle_participant_id(participant_id)
        } else {
            None
        };
        let participant_id = participant_id.to_string();
        let transport_id = transport.id();

        if let Some(lifecycle_id) = lifecycle_id {
            debug!(
                target: "simplestChat::lifecycle",
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
                            target: "simplestChat::lifecycle",
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
                            target: "simplestChat::lifecycle",
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
                            target: "simplestChat::lifecycle",
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
        let producer_id = producer.id().to_string();

        producer
            .on_close(|| {
                debug!("Media producer closed");
            })
            .detach();

        producer
            .on_pause({
                let participant_id = participant_id.clone();
                let producer_id = producer_id.clone();
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
        let consumer_id = consumer.id().to_string();

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
                let consumer_id = consumer_id.clone();
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
                let consumer_id = consumer_id.clone();
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
                let consumer_id = consumer_id.clone();
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
                let consumer_id = consumer_id.clone();
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
                    let consumer_id = consumer_id.clone();
                    let participant_id = participant_id.clone();
                    move |layers| {
                        debug!(
                            "Consumer {} layers changed to {:?} for participant {}",
                            consumer_id, layers, participant_id
                        );
                        let msg = ServerMessage::ConsumerLayersChanged {
                            consumer_id: consumer_id.clone(),
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
            let mut participants = self.participants.write().unwrap_or_else(|e| e.into_inner());
            participants.drain().collect()
        };

        for (participant_id, lock) in all_participants {
            let mut participant = lock.lock().await;
            participant.close_all().await;
            self.clear_bwe_trace_handler(participant.generation);
            debug!("Closed all transports for participant: {}", participant_id);
        }

        self.paused_producers
            .write()
            .unwrap_or_else(|error| error.into_inner())
            .clear();

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::config::{MediaConfig, RouterConfig};
    use crate::media::router_manager::RouterManager;
    use crate::media::worker_manager::WorkerManager;
    use std::num::{NonZeroU8, NonZeroU32};

    #[tokio::test]
    async fn quality_sampler_skips_busy_locks_and_reports_partial_coverage() {
        let manager = TransportManager::new();
        let busy = Arc::new(TokioMutex::new(ParticipantMedia::new("busy".to_owned())));
        let available = Arc::new(TokioMutex::new(ParticipantMedia::new(
            "available".to_owned(),
        )));
        manager.participants.write().unwrap().extend([
            ("busy".to_owned(), busy.clone()),
            ("available".to_owned(), available),
        ]);
        let held = busy.lock().await;
        let mut cursor = 0;
        let sample = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            manager.quality_sample(&mut cursor, 100),
        )
        .await
        .expect("sampling must not wait for a busy participant");
        assert_eq!(sample.participants_available, 2);
        assert_eq!(sample.participants_sampled, 1);
        assert!(!sample.budget_exhausted);
        assert!(sample.completed_at.is_some());
        drop(held);
        let next = manager.quality_sample(&mut cursor, 0).await;
        assert_eq!(next.participants_sampled, 2);
        assert_eq!(next.transports_requested, 0);
    }

    struct DropCounter(Arc<AtomicUsize>);

    impl Drop for DropCounter {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[test]
    fn default_consumer_cap_covers_a_32_person_camera_and_microphone_room() {
        // Every participant consumes audio and video from each peer, so a room
        // of N publishers needs 2 * (N - 1) consumers per participant.
        assert!(consumer_cap_from(None) >= 2 * 31);
        assert_eq!(consumer_cap_from(Some(" 8 ")), 8);
        assert_eq!(consumer_cap_from(Some("0")), consumer_cap_from(None));
    }

    #[test]
    fn lifecycle_identity_contains_only_the_canonical_public_uuid() {
        let participant_id = Uuid::new_v4();
        let namespace = format!(
            "private room\u{1f}{}\u{1f}{}",
            Uuid::new_v4(),
            participant_id.to_string().to_uppercase()
        );
        assert_eq!(lifecycle_participant_id(&namespace), Some(participant_id));
    }

    #[test]
    fn lifecycle_identity_rejects_unfamiliar_media_namespaces() {
        let participant_id = Uuid::new_v4();
        let session_id = Uuid::new_v4();
        for namespace in [
            "arbitrary private data".to_string(),
            participant_id.to_string(),
            format!("private room\u{1f}{participant_id}"),
            format!("private room\u{1f}not-a-session-uuid\u{1f}{participant_id}"),
            format!("private room\u{1f}{session_id}\u{1f}not-a-participant-uuid"),
            format!("\u{1f}{session_id}\u{1f}{participant_id}"),
        ] {
            assert_eq!(lifecycle_participant_id(&namespace), None);
        }
    }

    #[test]
    fn replacing_bwe_subscription_drops_the_previous_handler() {
        let manager = TransportManager::new();
        let drops = Arc::new(AtomicUsize::new(0));
        let old_generation = Uuid::new_v4();
        let new_generation = Uuid::new_v4();
        manager.replace_bwe_trace_handler(old_generation, Box::new(DropCounter(drops.clone())));
        assert_eq!(drops.load(Ordering::Relaxed), 0);

        manager.replace_bwe_trace_handler(old_generation, Box::new(DropCounter(drops.clone())));
        assert_eq!(drops.load(Ordering::Relaxed), 1);

        manager.replace_bwe_trace_handler(new_generation, Box::new(DropCounter(drops.clone())));
        manager.clear_bwe_trace_handler(old_generation);
        assert_eq!(drops.load(Ordering::Relaxed), 2);
        assert!(
            manager
                .bwe_trace_handlers
                .read()
                .unwrap()
                .contains_key(&new_generation),
            "cleanup of an old generation must retain a reused ID's handler"
        );
        manager.clear_bwe_trace_handler(new_generation);
        assert_eq!(drops.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn stale_participant_handle_is_rejected_after_replacement() {
        let manager = TransportManager::new();
        let participant_id = "reused-participant";
        let old = manager.get_or_create_participant(participant_id);
        assert!(manager.participant_is_current(participant_id, &old));

        let replacement = Arc::new(TokioMutex::new(ParticipantMedia::new(
            participant_id.to_string(),
        )));
        manager
            .participants
            .write()
            .unwrap()
            .insert(participant_id.to_string(), replacement.clone());

        assert!(!manager.participant_is_current(participant_id, &old));
        assert!(manager.participant_is_current(participant_id, &replacement));
    }

    #[test]
    fn unchanged_producer_pause_state_is_suppressed() {
        assert!(!producer_pause_transition_needed(false, false));
        assert!(!producer_pause_transition_needed(true, true));
        assert!(producer_pause_transition_needed(false, true));
        assert!(producer_pause_transition_needed(true, false));
    }

    #[test]
    fn unchanged_consumer_control_state_is_suppressed() {
        assert!(!consumer_pause_transition_needed(false, false));
        assert!(!consumer_pause_transition_needed(true, true));
        assert!(consumer_pause_transition_needed(false, true));
        assert!(consumer_pause_transition_needed(true, false));

        let layers = ConsumerLayers {
            spatial_layer: 1,
            temporal_layer: Some(2),
        };
        assert!(!consumer_layers_transition_needed(Some(layers), layers));
        assert!(consumer_layers_transition_needed(None, layers));
        assert!(consumer_layers_transition_needed(
            Some(layers),
            ConsumerLayers {
                spatial_layer: 2,
                temporal_layer: Some(2),
            }
        ));
    }

    #[tokio::test]
    async fn test_transport_creation() {
        // Keep this test isolated from a running local application and other
        // media tests by asking the OS for an available UDP port first.
        let reservation = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let mut config = MediaConfig::default();
        config.worker_config.num_workers = 1;
        config.webrtc_server_port_base = reservation.local_addr().unwrap().port();
        drop(reservation);
        let config = Arc::new(config);
        let worker_manager = Arc::new(WorkerManager::new(config.clone()).await.unwrap());
        let router_manager = RouterManager::new(worker_manager.clone());
        let transport_manager = TransportManager::new();

        // Create router first
        let room_id = "test-room".to_string();
        router_manager
            .create_router(room_id.clone(), RouterConfig::default())
            .await
            .unwrap();
        let router = router_manager.get_router(&room_id).await.unwrap();

        // Look up the WebRtcServer for this room's worker
        let worker_id = router_manager.get_worker_id(&room_id).await.unwrap();
        let webrtc_server = worker_manager.get_webrtc_server(worker_id).await.unwrap();
        let consumer_counter = worker_manager.get_consumer_counter(worker_id).unwrap();

        // Create transports via WebRtcServer (shared port)
        let participant_id = "test-participant".to_string();
        let send_transport = transport_manager
            .create_send_transport(
                participant_id.clone(),
                &router,
                webrtc_server.clone(),
                &config.webrtc_transport_config,
            )
            .await;

        assert!(send_transport.is_ok());

        let recv_transport = transport_manager
            .create_recv_transport(
                participant_id.clone(),
                &router,
                webrtc_server.clone(),
                &config.webrtc_transport_config,
            )
            .await;

        assert!(recv_transport.is_ok());

        // Read the real worker's applied policy, not only the Rust configuration.
        // The outgoing floor and both directional ceilings must remain applied.
        {
            let participant = transport_manager
                .get_participant_lock(&participant_id)
                .unwrap();
            let participant = participant.lock().await;
            let sent = participant
                .send_transport
                .as_ref()
                .unwrap()
                .get_stats()
                .await
                .unwrap();
            let received = participant
                .recv_transport
                .as_ref()
                .unwrap()
                .get_stats()
                .await
                .unwrap();
            assert_eq!(sent.len(), 1);
            assert_eq!(received.len(), 1);
            assert_eq!(sent[0].max_incoming_bitrate, Some(3_000_000));
            assert_eq!(received[0].max_outgoing_bitrate, Some(3_000_000));
            assert_eq!(received[0].min_outgoing_bitrate, Some(100_000));
        }

        // Programmatic overrides must reach the worker without losing its cap.
        let override_id = "explicit-minimum".to_string();
        let mut overridden = config.webrtc_transport_config.clone();
        overridden.min_outgoing_bitrate = 60_000;
        transport_manager
            .create_recv_transport(override_id.clone(), &router, webrtc_server, &overridden)
            .await
            .unwrap();
        {
            let participant = transport_manager
                .get_participant_lock(&override_id)
                .unwrap();
            let participant = participant.lock().await;
            let stats = participant
                .recv_transport
                .as_ref()
                .unwrap()
                .get_stats()
                .await
                .unwrap();
            assert_eq!(stats.len(), 1);
            assert_eq!(stats[0].min_outgoing_bitrate, Some(60_000));
            assert_eq!(stats[0].max_outgoing_bitrate, Some(3_000_000));
        }
        transport_manager
            .remove_participant(&override_id)
            .await
            .unwrap();

        let producer = transport_manager
            .create_producer(
                &participant_id,
                MediaKind::Audio,
                RtpParameters {
                    mid: Some("audio".to_string()),
                    codecs: vec![RtpCodecParameters::Audio {
                        mime_type: MimeTypeAudio::Opus,
                        payload_type: 111,
                        clock_rate: NonZeroU32::new(48_000).unwrap(),
                        channels: NonZeroU8::new(2).unwrap(),
                        parameters: RtpCodecParametersParameters::default(),
                        rtcp_feedback: vec![],
                    }],
                    ..RtpParameters::default()
                },
                AppData::default(),
            )
            .await
            .unwrap();

        // Exercise more retries than the per-session cap. Closing each failed or
        // retired receiver must release both native ownership and allocation load.
        // Active and paused producers require the same browser-ready acknowledgment.
        transport_manager.get_or_create_participant("other-session");
        for attempt in 0..=consumer_cap_from(None) {
            let producer_paused = attempt > 0;
            if producer_paused {
                transport_manager
                    .pause_producer(&participant_id, &producer.id().to_string())
                    .await
                    .unwrap();
            }
            let consumer = transport_manager
                .create_consumer(
                    &participant_id,
                    producer.id(),
                    RtpCapabilities {
                        codecs: config.router_config.media_codecs.clone(),
                        ..RtpCapabilities::default()
                    },
                    AppData::default(),
                    None,
                    Some(consumer_counter.clone()),
                )
                .await
                .unwrap();
            let info = crate::media::types::ConsumerInfo::from_consumer(&consumer);
            assert_eq!(consumer_counter.load(Ordering::Relaxed), 1);
            assert!(
                !transport_manager
                    .close_consumer("other-session", &info.id)
                    .await
                    .unwrap(),
                "another participant cannot close the receiver"
            );
            assert!(info.paused, "new consumers must wait for browser readiness");
            assert_eq!(info.producer_paused, producer_paused);
            assert!(consumer.dump().await.unwrap().paused);

            assert!(
                transport_manager
                    .resume_consumer(&participant_id, &info.id)
                    .await
                    .unwrap(),
                "the first browser acknowledgment must resume the worker consumer"
            );
            let resumed = consumer.dump().await.unwrap();
            assert!(!resumed.paused);
            assert_eq!(resumed.producer_paused, producer_paused);
            assert!(
                !transport_manager
                    .resume_consumer(&participant_id, &info.id)
                    .await
                    .unwrap(),
                "repeated acknowledgments remain idempotent"
            );
            let weak = consumer.downgrade();
            drop(consumer);
            assert!(
                transport_manager
                    .close_consumer(&participant_id, &info.id)
                    .await
                    .unwrap()
            );
            assert!(
                weak.upgrade().is_none(),
                "no application consumer handle remains"
            );
            assert_eq!(consumer_counter.load(Ordering::Relaxed), 0);
            assert!(
                !transport_manager
                    .close_consumer(&participant_id, &info.id)
                    .await
                    .unwrap(),
                "repeated closure must be a no-op"
            );
        }
    }
    #[test]
    fn effective_layers_take_the_stricter_of_client_and_bandwidth_ceilings() {
        let top = ConsumerLayers {
            spatial_layer: 2,
            temporal_layer: Some(2),
        };
        assert_eq!(effective_layers(top, None, None), top);
        assert_eq!(
            effective_layers(
                top,
                Some(ConsumerLayers {
                    spatial_layer: 0,
                    temporal_layer: None,
                }),
                None
            ),
            ConsumerLayers {
                spatial_layer: 0,
                temporal_layer: Some(2),
            },
            "an absent client temporal layer means the top one"
        );
        assert_eq!(
            effective_layers(top, None, Some(1)),
            ConsumerLayers {
                spatial_layer: 1,
                temporal_layer: Some(2),
            }
        );
        assert_eq!(
            effective_layers(
                top,
                Some(ConsumerLayers {
                    spatial_layer: 2,
                    temporal_layer: Some(1),
                }),
                Some(0)
            ),
            ConsumerLayers {
                spatial_layer: 0,
                temporal_layer: Some(1),
            },
            "the stricter spatial ceiling wins and the client's temporal choice is kept"
        );
        assert_eq!(
            effective_layers(
                top,
                Some(ConsumerLayers {
                    spatial_layer: 7,
                    temporal_layer: Some(9),
                }),
                Some(5)
            ),
            top,
            "requests above the stream's layers clamp to the top"
        );
    }

    fn simulcast_video_parameters() -> RtpParameters {
        let encoding = |ssrc: u32| RtpEncodingParameters {
            ssrc: Some(ssrc),
            scalability_mode: "L1T3".parse().unwrap(),
            ..RtpEncodingParameters::default()
        };
        RtpParameters {
            mid: Some("video".to_string()),
            codecs: vec![RtpCodecParameters::Video {
                mime_type: MimeTypeVideo::Vp8,
                payload_type: 96,
                clock_rate: NonZeroU32::new(90_000).unwrap(),
                parameters: RtpCodecParametersParameters::default(),
                rtcp_feedback: vec![],
            }],
            encodings: vec![encoding(1001), encoding(1002), encoding(1003)],
            ..RtpParameters::default()
        }
    }

    #[tokio::test]
    async fn bandwidth_ceiling_merges_with_the_client_ceiling_and_skips_audio() {
        let reservation = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let mut config = MediaConfig::default();
        config.worker_config.num_workers = 1;
        config.webrtc_server_port_base = reservation.local_addr().unwrap().port();
        drop(reservation);
        let config = Arc::new(config);
        let worker_manager = Arc::new(WorkerManager::new(config.clone()).await.unwrap());
        let router_manager = RouterManager::new(worker_manager.clone());
        let transport_manager = TransportManager::new();
        let room_id = "layers-room".to_string();
        router_manager
            .create_router(room_id.clone(), RouterConfig::default())
            .await
            .unwrap();
        let router = router_manager.get_router(&room_id).await.unwrap();
        let worker_id = router_manager.get_worker_id(&room_id).await.unwrap();
        let webrtc_server = worker_manager.get_webrtc_server(worker_id).await.unwrap();
        let pid = "viewer".to_string();
        transport_manager
            .create_send_transport(
                pid.clone(),
                &router,
                webrtc_server.clone(),
                &config.webrtc_transport_config,
            )
            .await
            .unwrap();
        transport_manager
            .create_recv_transport(
                pid.clone(),
                &router,
                webrtc_server,
                &config.webrtc_transport_config,
            )
            .await
            .unwrap();
        let capabilities = RtpCapabilities {
            codecs: config.router_config.media_codecs.clone(),
            ..RtpCapabilities::default()
        };
        let audio_producer = transport_manager
            .create_producer(
                &pid,
                MediaKind::Audio,
                RtpParameters {
                    mid: Some("audio".to_string()),
                    codecs: vec![RtpCodecParameters::Audio {
                        mime_type: MimeTypeAudio::Opus,
                        payload_type: 111,
                        clock_rate: NonZeroU32::new(48_000).unwrap(),
                        channels: NonZeroU8::new(2).unwrap(),
                        parameters: RtpCodecParametersParameters::default(),
                        rtcp_feedback: vec![],
                    }],
                    ..RtpParameters::default()
                },
                AppData::default(),
            )
            .await
            .unwrap();
        let video_producer = transport_manager
            .create_producer(
                &pid,
                MediaKind::Video,
                simulcast_video_parameters(),
                AppData::default(),
            )
            .await
            .unwrap();
        let audio = transport_manager
            .create_consumer(
                &pid,
                audio_producer.id(),
                capabilities.clone(),
                AppData::default(),
                None,
                None,
            )
            .await
            .unwrap();
        let video = transport_manager
            .create_consumer(
                &pid,
                video_producer.id(),
                capabilities.clone(),
                AppData::default(),
                None,
                None,
            )
            .await
            .unwrap();
        let audio_id = audio.id().to_string();
        let video_id = video.id().to_string();
        assert_eq!(video.r#type(), ConsumerType::Simulcast);
        let top = ConsumerLayers {
            spatial_layer: 2,
            temporal_layer: Some(2),
        };
        assert_eq!(video.preferred_layers(), Some(top));

        // Audio has no layers: neither writer may spend a worker request on it.
        assert!(
            !transport_manager
                .set_preferred_layers(
                    &pid,
                    &audio_id,
                    ConsumerLayers {
                        spatial_layer: 1,
                        temporal_layer: None,
                    },
                )
                .await
                .unwrap()
        );
        assert_eq!(audio.preferred_layers(), None);
        assert_eq!(
            transport_manager
                .set_bandwidth_ceiling(&pid, 2)
                .await
                .unwrap(),
            Vec::<String>::new(),
            "the top tier matches the worker's default ceiling: nothing to write"
        );

        // The bandwidth tier lowers the video consumer only.
        assert_eq!(
            transport_manager
                .set_bandwidth_ceiling(&pid, 1)
                .await
                .unwrap(),
            vec![video_id.clone()]
        );
        assert!(
            transport_manager
                .apply_layer_ceilings(&pid, &video_id)
                .await
                .unwrap()
        );
        assert!(
            !transport_manager
                .apply_layer_ceilings(&pid, &video_id)
                .await
                .unwrap(),
            "a second application is a no-op"
        );
        assert_eq!(
            video.preferred_layers(),
            Some(ConsumerLayers {
                spatial_layer: 1,
                temporal_layer: Some(2),
            })
        );

        // A stricter client ceiling stays in force when bandwidth recovers.
        assert!(
            transport_manager
                .set_preferred_layers(
                    &pid,
                    &video_id,
                    ConsumerLayers {
                        spatial_layer: 0,
                        temporal_layer: Some(1),
                    },
                )
                .await
                .unwrap()
        );
        assert_eq!(
            transport_manager
                .set_bandwidth_ceiling(&pid, 2)
                .await
                .unwrap(),
            Vec::<String>::new()
        );
        assert_eq!(
            video.preferred_layers(),
            Some(ConsumerLayers {
                spatial_layer: 0,
                temporal_layer: Some(1),
            }),
            "the bandwidth tier must not push a capped tile back to the top layer"
        );

        // Lifting the client ceiling restores the bandwidth tier, and a lower
        // tier applies to consumers created while it is in force.
        assert!(
            transport_manager
                .set_preferred_layers(
                    &pid,
                    &video_id,
                    ConsumerLayers {
                        spatial_layer: 2,
                        temporal_layer: None,
                    },
                )
                .await
                .unwrap()
        );
        assert_eq!(video.preferred_layers(), Some(top));
        assert_eq!(
            transport_manager
                .set_bandwidth_ceiling(&pid, 0)
                .await
                .unwrap(),
            vec![video_id.clone()]
        );
        assert!(
            transport_manager
                .apply_layer_ceilings(&pid, &video_id)
                .await
                .unwrap()
        );
        let late = transport_manager
            .create_consumer(
                &pid,
                video_producer.id(),
                capabilities,
                AppData::default(),
                None,
                None,
            )
            .await
            .unwrap();
        assert_eq!(
            late.preferred_layers(),
            Some(ConsumerLayers {
                spatial_layer: 0,
                temporal_layer: Some(2),
            }),
            "a consumer created under a low tier starts capped without an extra request"
        );
        drop(video);
        assert!(
            transport_manager
                .close_consumer(&pid, &video_id)
                .await
                .unwrap()
        );
        let participant = transport_manager.get_participant(&pid).await.unwrap();
        assert!(!participant.consumer_layers.contains_key(&video_id));
        assert!(participant.consumers.contains_key(&late.id().to_string()));
    }
    /// A loopback port free for both UDP and TCP, so a TCP-enabled WebRtcServer
    /// can bind both in tests.
    fn reserve_udp_and_tcp_port() -> u16 {
        for _ in 0..50 {
            let udp = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
            let port = udp.local_addr().unwrap().port();
            if std::net::TcpListener::bind(("0.0.0.0", port)).is_ok() {
                drop(udp);
                return port;
            }
        }
        panic!("no port free for both UDP and TCP");
    }

    #[tokio::test]
    async fn tcp_listener_adds_tcp_ice_candidates_and_is_off_by_default() {
        for tcp in [false, true] {
            let mut config = MediaConfig::default();
            config.worker_config.num_workers = 1;
            config.webrtc_server_port_base = reserve_udp_and_tcp_port();
            assert!(!config.webrtc_transport_config.enable_tcp, "TCP is opt-in");
            config.set_tcp(tcp);
            assert_eq!(config.webrtc_server_tcp, tcp);
            assert_eq!(
                config.webrtc_transport_config.enable_tcp, tcp,
                "the transport flag must follow the listener, never claim TCP without one"
            );
            let config = Arc::new(config);
            let worker_manager = Arc::new(WorkerManager::new(config.clone()).await.unwrap());
            let router_manager = RouterManager::new(worker_manager.clone());
            let transport_manager = TransportManager::new();
            let room_id = format!("tcp-room-{tcp}");
            router_manager
                .create_router(room_id.clone(), RouterConfig::default())
                .await
                .unwrap();
            let router = router_manager.get_router(&room_id).await.unwrap();
            let worker_id = router_manager.get_worker_id(&room_id).await.unwrap();
            let webrtc_server = worker_manager.get_webrtc_server(worker_id).await.unwrap();
            let info = transport_manager
                .create_send_transport(
                    "candidate-check".to_string(),
                    &router,
                    webrtc_server,
                    &config.webrtc_transport_config,
                )
                .await
                .unwrap();
            let protocols: Vec<Protocol> = info
                .ice_candidates
                .iter()
                .map(|candidate| candidate.protocol)
                .collect();
            assert!(protocols.contains(&Protocol::Udp), "{protocols:?}");
            assert_eq!(protocols.contains(&Protocol::Tcp), tcp, "{protocols:?}");
        }
    }
}
