#![forbid(unsafe_code)]

// Transport management for WebRTC connections

use crate::media::config::WebRtcTransportConfig;
use crate::media::types::{MediaError, MediaResult, ParticipantMedia, TransportInfo};
use crate::signaling::protocol::ServerMessage;
use anyhow::Result;
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
use tracing::{debug, info, warn};
use uuid::Uuid;

fn max_producers_per_participant() -> usize {
    std::env::var("MAX_PRODUCERS_PER_PARTICIPANT")
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|value| *value > 0)
        .unwrap_or(8)
}

fn max_consumers_per_participant() -> usize {
    std::env::var("MAX_CONSUMERS_PER_PARTICIPANT")
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|value| *value > 0)
        .unwrap_or(16)
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

impl TransportManager {
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
        debug!(
            "Creating send transport for participant: {}",
            participant_id
        );

        let participant_lock = self.get_or_create_participant(&participant_id);
        let mut participant = participant_lock.lock().await;
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

        if !self.participant_is_current(&participant_id, &participant_lock) {
            drop(transport);
            return Err(MediaError::InvalidState(
                "Participant media session closed while creating send transport".to_string(),
            ));
        }

        let transport_info = TransportInfo::from(&transport);
        self.setup_transport_handlers(&transport, &participant_id, "send");
        participant.send_transport = Some(transport);

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
        debug!(
            "Creating receive transport for participant: {}",
            participant_id
        );

        let participant_lock = self.get_or_create_participant(&participant_id);
        let mut participant = participant_lock.lock().await;
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

        info!(
            "Created receive transport {} for participant {}",
            transport_info.id, participant_id
        );
        Ok(transport_info)
    }

    /// Connects a transport by ID (determines send vs recv automatically).
    /// Returns whether this call performed worker IPC; retries after a lost
    /// signaling acknowledgement are successful no-ops.
    pub async fn connect_transport(
        &self,
        participant_id: &str,
        transport_id: &str,
        dtls_parameters: DtlsParameters,
    ) -> MediaResult<bool> {
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

        match transport.dtls_state() {
            DtlsState::New => {}
            // A lost signaling acknowledgement can legitimately cause the
            // client to retry while the handshake is still in progress.
            DtlsState::Connecting | DtlsState::Connected => return Ok(false),
            state => {
                return Err(MediaError::InvalidState(format!(
                    "Transport cannot connect from DTLS state {state:?}"
                )));
            }
        }

        transport
            .connect(WebRtcTransportRemoteParameters { dtls_parameters })
            .await
            .map_err(|e| MediaError::TransportError(format!("Failed to connect transport: {e}")))?;

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
        let mut participant = participant_lock.lock().await;

        participant
            .producers
            .retain(|_, producer| !producer.closed());
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

        let producer = transport
            .produce(producer_options)
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
        sender: Option<mpsc::Sender<Arc<String>>>,
        consumer_counter: Option<Arc<AtomicUsize>>,
    ) -> MediaResult<Consumer> {
        let participant_lock = self.get_participant_lock(participant_id)?;
        let mut participant = participant_lock.lock().await;

        participant
            .consumers
            .retain(|_, consumer| !consumer.closed());
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

        let consumer = transport
            .consume(consumer_options)
            .await
            .map_err(|e| MediaError::ConsumerError(format!("Failed to create consumer: {e}")))?;

        let consumer_id = consumer.id().to_string();

        // Increment the worker's consumer count
        if let Some(ref counter) = consumer_counter {
            counter.fetch_add(1, Ordering::Relaxed);
        }

        self.setup_consumer_handlers(&consumer, participant_id, sender, consumer_counter);
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
        let participant = participant_lock.lock().await;

        let consumer = participant.consumers.get(consumer_id).ok_or_else(|| {
            MediaError::ConsumerError(format!("Consumer not found: {consumer_id}"))
        })?;

        if !consumer_pause_transition_needed(consumer.paused(), false) {
            return Ok(false);
        }

        consumer
            .resume()
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
        let participant = participant_lock.lock().await;

        let producer = participant.producers.get(producer_id).ok_or_else(|| {
            MediaError::ProducerError(format!("Producer not found: {producer_id}"))
        })?;

        if !producer_pause_transition_needed(producer.paused(), false) {
            return Ok(false);
        }

        producer
            .resume()
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

    /// Pauses all consumers whose producer matches the given producer_id.
    /// Returns the count of consumers that were actually paused.
    pub async fn pause_consumers_of_producer(&self, producer_id: &str) -> MediaResult<usize> {
        let target_id: ProducerId = producer_id.parse().map_err(|_| {
            MediaError::ProducerError(format!("Invalid producer ID: {producer_id}"))
        })?;

        let all_locks: Vec<Arc<TokioMutex<ParticipantMedia>>> = {
            let participants = self.participants.read().unwrap_or_else(|e| e.into_inner());
            participants.values().cloned().collect()
        };

        let mut count = 0usize;
        for lock in all_locks {
            let participant = lock.lock().await;
            for (cid, consumer) in &participant.consumers {
                if !consumer.closed() && consumer.producer_id() == target_id && !consumer.paused() {
                    if let Err(e) = consumer.pause().await {
                        warn!(
                            "Failed to pause consumer {} of producer {}: {}",
                            cid, producer_id, e
                        );
                    } else {
                        count += 1;
                    }
                }
            }
        }

        Ok(count)
    }

    /// Resumes all consumers whose producer matches the given producer_id.
    /// Returns the count of consumers that were actually resumed.
    pub async fn resume_consumers_of_producer(&self, producer_id: &str) -> MediaResult<usize> {
        let target_id: ProducerId = producer_id.parse().map_err(|_| {
            MediaError::ProducerError(format!("Invalid producer ID: {producer_id}"))
        })?;

        let all_locks: Vec<Arc<TokioMutex<ParticipantMedia>>> = {
            let participants = self.participants.read().unwrap_or_else(|e| e.into_inner());
            participants.values().cloned().collect()
        };

        let mut count = 0usize;
        for lock in all_locks {
            let participant = lock.lock().await;
            for (cid, consumer) in &participant.consumers {
                if !consumer.closed() && consumer.producer_id() == target_id && consumer.paused() {
                    if let Err(e) = consumer.resume().await {
                        warn!(
                            "Failed to resume consumer {} of producer {}: {}",
                            cid, producer_id, e
                        );
                    } else {
                        count += 1;
                    }
                }
            }
        }

        Ok(count)
    }

    /// Sets preferred simulcast layers for a consumer, returning whether worker state changed.
    pub async fn set_preferred_layers(
        &self,
        participant_id: &str,
        consumer_id: &str,
        layers: ConsumerLayers,
    ) -> MediaResult<bool> {
        let participant_lock = self.get_participant_lock(participant_id)?;
        let participant = participant_lock.lock().await;

        let consumer = participant.consumers.get(consumer_id).ok_or_else(|| {
            MediaError::ConsumerError(format!("Consumer not found: {consumer_id}"))
        })?;

        if !consumer_layers_transition_needed(consumer.preferred_layers(), layers) {
            return Ok(false);
        }

        consumer.set_preferred_layers(layers).await.map_err(|e| {
            MediaError::ConsumerError(format!("Failed to set preferred layers: {e}"))
        })?;

        debug!(
            "Set preferred layers {:?} for consumer {} of participant {}",
            layers, consumer_id, participant_id
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

    /// Gets the consumer IDs for a participant (no IPC — reads in-memory HashMap only)
    pub async fn get_consumer_ids(&self, participant_id: &str) -> MediaResult<Vec<String>> {
        let participant_lock = self.get_participant_lock(participant_id)?;
        let mut participant = participant_lock.lock().await;
        participant
            .consumers
            .retain(|_, consumer| !consumer.closed());
        Ok(participant.consumers.keys().cloned().collect())
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

    /// Gets a participant's media state
    pub async fn get_participant(&self, participant_id: &str) -> MediaResult<ParticipantMedia> {
        let participant_lock = self.get_participant_lock(participant_id)?;
        let participant = participant_lock.lock().await;
        Ok(participant.clone())
    }

    /// Removes a participant and closes all their transports
    pub async fn remove_participant(&self, participant_id: &str) -> MediaResult<()> {
        // Remove from outer map (brief write lock)
        let participant_lock = {
            let mut participants = self.participants.write().unwrap_or_else(|e| e.into_inner());
            participants.remove(participant_id)
        };

        if let Some(lock) = participant_lock {
            let mut participant = lock.lock().await;

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
        transport_type: &str,
    ) {
        let participant_id = participant_id.to_string();
        let transport_type = transport_type.to_string();
        let transport_id = transport.id().to_string();

        transport
            .on_close({
                let participant_id = participant_id.clone();
                let transport_type = transport_type.clone();
                let transport_id = transport_id.clone();
                Box::new(move || {
                    warn!(
                        "Transport {} ({}) closed for participant {}",
                        transport_id, transport_type, participant_id
                    );
                })
            })
            .detach();

        transport
            .on_dtls_state_change({
                let participant_id = participant_id.clone();
                let transport_id = transport_id.clone();
                let transport_type = transport_type.clone();
                move |dtls_state| {
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
            .on_close({
                let participant_id = participant_id.clone();
                let producer_id = producer_id.clone();
                move || {
                    warn!(
                        "Producer {} closed for participant {}",
                        producer_id, participant_id
                    );
                }
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
        sender: Option<mpsc::Sender<Arc<String>>>,
        consumer_counter: Option<Arc<AtomicUsize>>,
    ) {
        let participant_id = participant_id.to_string();
        let consumer_id = consumer.id().to_string();

        consumer
            .on_close({
                let participant_id = participant_id.clone();
                let consumer_id = consumer_id.clone();
                let counter = consumer_counter;
                move || {
                    if let Some(ref c) = counter {
                        c.fetch_sub(1, Ordering::Relaxed);
                    }
                    warn!(
                        "Consumer {} closed for participant {}",
                        consumer_id, participant_id
                    );
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
                            let _ = sender.try_send(Arc::new(json));
                        }
                    }
                })
                .detach();
        }
    }

    /// Closes all transports for all participants
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

    struct DropCounter(Arc<AtomicUsize>);

    impl Drop for DropCounter {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::Relaxed);
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
                webrtc_server,
                &config.webrtc_transport_config,
            )
            .await;

        assert!(recv_transport.is_ok());

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

        // Both active and paused producers require the same browser-ready
        // acknowledgment. Producer pause must remain independent of it.
        for producer_paused in [false, true] {
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
                    None,
                )
                .await
                .unwrap();
            let info = crate::media::types::ConsumerInfo::from_consumer(&consumer);
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
        }
    }
}
