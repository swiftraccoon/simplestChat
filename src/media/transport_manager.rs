#![forbid(unsafe_code)]

// Transport management for WebRTC connections

use crate::media::types::{MediaError, MediaResult, TransportInfo, ParticipantMedia};
use crate::signaling::protocol::ServerMessage;
use mediasoup::prelude::*;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::RwLock as StdRwLock;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::Mutex as TokioMutex;
use tokio::sync::mpsc;
use tracing::{info, debug, warn};
use anyhow::Result;

/// Manages WebRTC transports for participants.
///
/// Uses per-participant locking: the outer HashMap is protected by a std::sync::RwLock
/// (held only for brief lookups, never across await points), while each participant's
/// media state is protected by its own tokio::sync::Mutex (held across async operations
/// but only blocking that specific participant).
pub struct TransportManager {
    participants: Arc<StdRwLock<HashMap<String, Arc<TokioMutex<ParticipantMedia>>>>>,
}

impl TransportManager {
    /// Creates a new TransportManager
    pub fn new() -> Self {
        Self {
            participants: Arc::new(StdRwLock::new(HashMap::new())),
        }
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
            .or_insert_with(|| Arc::new(TokioMutex::new(ParticipantMedia::new(participant_id.to_string()))))
            .clone()
    }

    /// Gets an existing participant's lock (brief outer read lock, no await)
    fn get_participant_lock(&self, participant_id: &str) -> MediaResult<Arc<TokioMutex<ParticipantMedia>>> {
        let participants = self.participants.read().unwrap_or_else(|e| e.into_inner());
        participants
            .get(participant_id)
            .cloned()
            .ok_or_else(|| MediaError::ParticipantNotFound(participant_id.to_string()))
    }

    /// Creates a send transport for a participant
    pub async fn create_send_transport(
        &self,
        participant_id: String,
        router: &Router,
        webrtc_server: WebRtcServer,
    ) -> MediaResult<TransportInfo> {
        debug!("Creating send transport for participant: {}", participant_id);

        // Create transport WITHOUT any lock held — uses shared WebRtcServer port
        let transport_options = WebRtcTransportOptions::new_with_server(webrtc_server);
        let transport = router
            .create_webrtc_transport(transport_options)
            .await
            .map_err(|e| MediaError::TransportError(format!("Failed to create send transport: {e}")))?;

        let transport_info = TransportInfo::from(&transport);
        self.setup_transport_handlers(&transport, &participant_id, "send");

        // Store (per-participant lock only)
        let participant_lock = self.get_or_create_participant(&participant_id);
        let mut participant = participant_lock.lock().await;
        participant.send_transport = Some(transport);

        info!("Created send transport {} for participant {}", transport_info.id, participant_id);
        Ok(transport_info)
    }

    /// Creates a receive transport for a participant
    pub async fn create_recv_transport(
        &self,
        participant_id: String,
        router: &Router,
        webrtc_server: WebRtcServer,
    ) -> MediaResult<TransportInfo> {
        debug!("Creating receive transport for participant: {}", participant_id);

        // Create transport WITHOUT any lock held — uses shared WebRtcServer port
        let transport_options = WebRtcTransportOptions::new_with_server(webrtc_server);
        let transport = router
            .create_webrtc_transport(transport_options)
            .await
            .map_err(|e| MediaError::TransportError(format!("Failed to create receive transport: {e}")))?;

        let transport_info = TransportInfo::from(&transport);
        self.setup_transport_handlers(&transport, &participant_id, "recv");

        // Store (per-participant lock only)
        let participant_lock = self.get_or_create_participant(&participant_id);
        let mut participant = participant_lock.lock().await;
        participant.recv_transport = Some(transport);

        info!("Created receive transport {} for participant {}", transport_info.id, participant_id);
        Ok(transport_info)
    }

    /// Connects a transport by ID (determines send vs recv automatically)
    pub async fn connect_transport(
        &self,
        participant_id: &str,
        transport_id: &str,
        dtls_parameters: DtlsParameters,
    ) -> MediaResult<()> {
        let participant_lock = self.get_participant_lock(participant_id)?;
        let participant = participant_lock.lock().await;

        let is_send = participant.send_transport.as_ref()
            .map_or(false, |t| t.id().to_string() == transport_id);

        let transport = if is_send {
            participant.send_transport.as_ref()
        } else {
            participant.recv_transport.as_ref()
        }
        .ok_or_else(|| MediaError::TransportError(format!("Transport not found: {transport_id}")))?;

        transport
            .connect(WebRtcTransportRemoteParameters { dtls_parameters })
            .await
            .map_err(|e| MediaError::TransportError(format!("Failed to connect transport: {e}")))?;

        info!("Connected transport {} for participant {}", transport_id, participant_id);
        Ok(())
    }

    /// Connects a send transport with DTLS parameters
    pub async fn connect_send_transport(
        &self,
        participant_id: &str,
        dtls_parameters: DtlsParameters,
    ) -> MediaResult<()> {
        let participant_lock = self.get_participant_lock(participant_id)?;
        let participant = participant_lock.lock().await;

        let transport = participant
            .send_transport
            .as_ref()
            .ok_or_else(|| MediaError::TransportError("Send transport not found".to_string()))?;

        transport
            .connect(WebRtcTransportRemoteParameters { dtls_parameters })
            .await
            .map_err(|e| MediaError::TransportError(format!("Failed to connect send transport: {e}")))?;

        info!("Connected send transport for participant {}", participant_id);
        Ok(())
    }

    /// Connects a receive transport with DTLS parameters
    pub async fn connect_recv_transport(
        &self,
        participant_id: &str,
        dtls_parameters: DtlsParameters,
    ) -> MediaResult<()> {
        let participant_lock = self.get_participant_lock(participant_id)?;
        let participant = participant_lock.lock().await;

        let transport = participant
            .recv_transport
            .as_ref()
            .ok_or_else(|| MediaError::TransportError("Receive transport not found".to_string()))?;

        transport
            .connect(WebRtcTransportRemoteParameters { dtls_parameters })
            .await
            .map_err(|e| MediaError::TransportError(format!("Failed to connect receive transport: {e}")))?;

        info!("Connected receive transport for participant {}", participant_id);
        Ok(())
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
        participant.producers.insert(producer_id.clone(), producer.clone());

        info!("Created {:?} producer {} for participant {}", kind, producer_id, participant_id);
        Ok(producer)
    }

    /// Creates a consumer on the participant's receive transport
    pub async fn create_consumer(
        &self,
        participant_id: &str,
        producer_id: ProducerId,
        rtp_capabilities: RtpCapabilities,
        app_data: AppData,
        sender: Option<mpsc::Sender<Arc<String>>>,
        paused: bool,
        consumer_counter: Option<Arc<AtomicUsize>>,
    ) -> MediaResult<Consumer> {
        let participant_lock = self.get_participant_lock(participant_id)?;
        let mut participant = participant_lock.lock().await;

        let transport = participant
            .recv_transport
            .as_ref()
            .ok_or_else(|| MediaError::TransportError("Receive transport not found".to_string()))?;

        let mut consumer_options = ConsumerOptions::new(producer_id, rtp_capabilities);
        consumer_options.app_data = app_data;
        consumer_options.paused = paused;

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
        participant.consumers.insert(consumer_id.clone(), consumer.clone());

        info!("Created consumer {} for producer {} and participant {}",
              consumer_id, producer_id, participant_id);
        Ok(consumer)
    }

    /// Gets the number of consumers for a participant
    pub async fn consumer_count(&self, participant_id: &str) -> MediaResult<usize> {
        let participant_lock = self.get_participant_lock(participant_id)?;
        let participant = participant_lock.lock().await;
        Ok(participant.consumers.len())
    }

    /// Resumes a consumer for a participant
    pub async fn resume_consumer(
        &self,
        participant_id: &str,
        consumer_id: &str,
    ) -> MediaResult<()> {
        let participant_lock = self.get_participant_lock(participant_id)?;
        let participant = participant_lock.lock().await;

        let consumer = participant
            .consumers
            .get(consumer_id)
            .ok_or_else(|| MediaError::ConsumerError(format!("Consumer not found: {consumer_id}")))?;

        consumer
            .resume()
            .await
            .map_err(|e| MediaError::ConsumerError(format!("Failed to resume consumer: {e}")))?;

        info!("Resumed consumer {} for participant {}", consumer_id, participant_id);
        Ok(())
    }

    /// Pauses a consumer for a participant
    pub async fn pause_consumer(
        &self,
        participant_id: &str,
        consumer_id: &str,
    ) -> MediaResult<()> {
        let participant_lock = self.get_participant_lock(participant_id)?;
        let participant = participant_lock.lock().await;

        let consumer = participant
            .consumers
            .get(consumer_id)
            .ok_or_else(|| MediaError::ConsumerError(format!("Consumer not found: {consumer_id}")))?;

        consumer
            .pause()
            .await
            .map_err(|e| MediaError::ConsumerError(format!("Failed to pause consumer: {e}")))?;

        info!("Paused consumer {} for participant {}", consumer_id, participant_id);
        Ok(())
    }

    /// Pauses a producer for a participant
    pub async fn pause_producer(
        &self,
        participant_id: &str,
        producer_id: &str,
    ) -> MediaResult<()> {
        let participant_lock = self.get_participant_lock(participant_id)?;
        let participant = participant_lock.lock().await;

        let producer = participant
            .producers
            .get(producer_id)
            .ok_or_else(|| MediaError::ProducerError(format!("Producer not found: {producer_id}")))?;

        producer
            .pause()
            .await
            .map_err(|e| MediaError::ProducerError(format!("Failed to pause producer: {e}")))?;

        info!("Paused producer {} for participant {}", producer_id, participant_id);
        Ok(())
    }

    /// Resumes a producer for a participant
    pub async fn resume_producer(
        &self,
        participant_id: &str,
        producer_id: &str,
    ) -> MediaResult<()> {
        let participant_lock = self.get_participant_lock(participant_id)?;
        let participant = participant_lock.lock().await;

        let producer = participant
            .producers
            .get(producer_id)
            .ok_or_else(|| MediaError::ProducerError(format!("Producer not found: {producer_id}")))?;

        producer
            .resume()
            .await
            .map_err(|e| MediaError::ProducerError(format!("Failed to resume producer: {e}")))?;

        info!("Resumed producer {} for participant {}", producer_id, participant_id);
        Ok(())
    }

    /// Searches across all participants to find if a producer is paused.
    /// Returns `Some(paused)` if found, `None` if the producer doesn't exist.
    pub async fn find_producer_paused(&self, producer_id: &str) -> Option<bool> {
        let target_id: ProducerId = producer_id.parse().ok()?;

        let all_locks: Vec<Arc<TokioMutex<ParticipantMedia>>> = {
            let participants = self.participants.read().unwrap_or_else(|e| e.into_inner());
            participants.values().cloned().collect()
        };

        for lock in all_locks {
            let participant = lock.lock().await;
            for (pid, producer) in &participant.producers {
                if producer.id() == target_id {
                    debug!("Found producer {} (paused={}) in participant {}", pid, producer.paused(), participant.id);
                    return Some(producer.paused());
                }
            }
        }

        None
    }

    /// Pauses all consumers whose producer matches the given producer_id.
    /// Returns the count of consumers that were actually paused.
    pub async fn pause_consumers_of_producer(&self, producer_id: &str) -> MediaResult<usize> {
        let target_id: ProducerId = producer_id.parse()
            .map_err(|_| MediaError::ProducerError(format!("Invalid producer ID: {producer_id}")))?;

        let all_locks: Vec<Arc<TokioMutex<ParticipantMedia>>> = {
            let participants = self.participants.read().unwrap_or_else(|e| e.into_inner());
            participants.values().cloned().collect()
        };

        let mut count = 0usize;
        for lock in all_locks {
            let participant = lock.lock().await;
            for (cid, consumer) in &participant.consumers {
                if consumer.producer_id() == target_id && !consumer.paused() {
                    if let Err(e) = consumer.pause().await {
                        warn!("Failed to pause consumer {} of producer {}: {}", cid, producer_id, e);
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
        let target_id: ProducerId = producer_id.parse()
            .map_err(|_| MediaError::ProducerError(format!("Invalid producer ID: {producer_id}")))?;

        let all_locks: Vec<Arc<TokioMutex<ParticipantMedia>>> = {
            let participants = self.participants.read().unwrap_or_else(|e| e.into_inner());
            participants.values().cloned().collect()
        };

        let mut count = 0usize;
        for lock in all_locks {
            let participant = lock.lock().await;
            for (cid, consumer) in &participant.consumers {
                if consumer.producer_id() == target_id && consumer.paused() {
                    if let Err(e) = consumer.resume().await {
                        warn!("Failed to resume consumer {} of producer {}: {}", cid, producer_id, e);
                    } else {
                        count += 1;
                    }
                }
            }
        }

        Ok(count)
    }

    /// Sets preferred simulcast layers for a consumer
    pub async fn set_preferred_layers(
        &self,
        participant_id: &str,
        consumer_id: &str,
        layers: ConsumerLayers,
    ) -> MediaResult<()> {
        let participant_lock = self.get_participant_lock(participant_id)?;
        let participant = participant_lock.lock().await;

        let consumer = participant
            .consumers
            .get(consumer_id)
            .ok_or_else(|| MediaError::ConsumerError(format!("Consumer not found: {consumer_id}")))?;

        consumer
            .set_preferred_layers(layers)
            .await
            .map_err(|e| MediaError::ConsumerError(format!("Failed to set preferred layers: {e}")))?;

        debug!("Set preferred layers {:?} for consumer {} of participant {}",
               layers, consumer_id, participant_id);
        Ok(())
    }

    /// Restarts ICE on a transport, returning new ICE parameters
    pub async fn restart_ice(
        &self,
        participant_id: &str,
        transport_id: &str,
    ) -> MediaResult<IceParameters> {
        let participant_lock = self.get_participant_lock(participant_id)?;
        let participant = participant_lock.lock().await;

        let is_send = participant.send_transport.as_ref()
            .map_or(false, |t| t.id().to_string() == transport_id);

        let transport = if is_send {
            participant.send_transport.as_ref()
        } else {
            participant.recv_transport.as_ref()
        }
        .ok_or_else(|| MediaError::TransportError(format!("Transport not found: {transport_id}")))?;

        let ice_parameters = transport
            .restart_ice()
            .await
            .map_err(|e| MediaError::TransportError(format!("Failed to restart ICE: {e}")))?;

        info!("Restarted ICE for transport {} of participant {}", transport_id, participant_id);
        Ok(ice_parameters)
    }

    /// Gets recv transport stats for bandwidth estimation
    pub async fn get_recv_transport_stats(
        &self,
        participant_id: &str,
    ) -> MediaResult<Option<(u32, Vec<String>)>> {
        let participant_lock = self.get_participant_lock(participant_id)?;
        let participant = participant_lock.lock().await;

        let transport = match &participant.recv_transport {
            Some(t) => t,
            None => return Ok(None),
        };

        let stats = transport
            .get_stats()
            .await
            .map_err(|e| MediaError::TransportError(format!("Failed to get transport stats: {e}")))?;

        // Access available_outgoing_bitrate directly from WebRtcTransportStat struct
        let bitrate = stats.first()
            .and_then(|s| s.available_outgoing_bitrate)
            .unwrap_or(0);

        let consumer_ids: Vec<String> = participant.consumers.keys().cloned().collect();

        Ok(Some((bitrate, consumer_ids)))
    }

    /// Gets send transport stats for connection quality reporting
    pub async fn get_send_transport_stats(
        &self,
        participant_id: &str,
    ) -> MediaResult<Option<Option<u32>>> {
        let participant_lock = self.get_participant_lock(participant_id)?;
        let participant = participant_lock.lock().await;

        let transport = match &participant.send_transport {
            Some(t) => t,
            None => return Ok(None),
        };

        let stats = transport
            .get_stats()
            .await
            .map_err(|e| MediaError::TransportError(format!("Failed to get send transport stats: {e}")))?;

        // Access available_outgoing_bitrate directly from WebRtcTransportStat struct
        let bitrate = stats.first()
            .and_then(|s| s.available_outgoing_bitrate);

        Ok(Some(bitrate))
    }

    /// Closes a producer for a participant
    pub async fn close_producer(
        &self,
        participant_id: &str,
        producer_id: &str,
    ) -> MediaResult<()> {
        let participant_lock = self.get_participant_lock(participant_id)?;
        let mut participant = participant_lock.lock().await;

        participant
            .producers
            .remove(producer_id)
            .ok_or_else(|| MediaError::ProducerError(format!("Producer not found: {producer_id}")))?;

        info!("Closed producer {} for participant {}", producer_id, participant_id);
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
            participant.close_all().await;
            info!("Removed participant {} and closed all media resources", participant_id);
            Ok(())
        } else {
            Err(MediaError::ParticipantNotFound(participant_id.to_string()))
        }
    }

    /// Sets up event handlers for a transport.
    /// Handlers are detached so they persist for the transport's lifetime.
    fn setup_transport_handlers(&self, transport: &WebRtcTransport, participant_id: &str, transport_type: &str) {
        let participant_id = participant_id.to_string();
        let transport_type = transport_type.to_string();
        let transport_id = transport.id().to_string();

        transport.on_close({
            let participant_id = participant_id.clone();
            let transport_type = transport_type.clone();
            let transport_id = transport_id.clone();
            Box::new(move || {
                warn!("Transport {} ({}) closed for participant {}",
                      transport_id, transport_type, participant_id);
            })
        }).detach();

        transport.on_dtls_state_change({
            let participant_id = participant_id.clone();
            let transport_id = transport_id.clone();
            let transport_type = transport_type.clone();
            move |dtls_state| {
                info!("DTLS state: {:?} for {} transport {} (participant {})",
                       dtls_state, transport_type, transport_id, participant_id);
            }
        }).detach();

        transport.on_ice_state_change({
            let participant_id = participant_id;
            let transport_id = transport_id;
            let transport_type = transport_type;
            move |ice_state| {
                info!("ICE state: {:?} for {} transport {} (participant {})",
                       ice_state, transport_type, transport_id, participant_id);
            }
        }).detach();
    }

    /// Sets up event handlers for a producer.
    /// Handlers are detached so they persist for the producer's lifetime.
    fn setup_producer_handlers(&self, producer: &Producer, participant_id: &str) {
        let participant_id = participant_id.to_string();
        let producer_id = producer.id().to_string();

        producer.on_close({
            let participant_id = participant_id.clone();
            let producer_id = producer_id.clone();
            move || {
                warn!("Producer {} closed for participant {}", producer_id, participant_id);
            }
        }).detach();

        producer.on_pause({
            let participant_id = participant_id.clone();
            let producer_id = producer_id.clone();
            move || {
                debug!("Producer {} paused for participant {}", producer_id, participant_id);
            }
        }).detach();

        producer.on_resume({
            let participant_id = participant_id;
            let producer_id = producer_id;
            move || {
                debug!("Producer {} resumed for participant {}", producer_id, participant_id);
            }
        }).detach();
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

        consumer.on_close({
            let participant_id = participant_id.clone();
            let consumer_id = consumer_id.clone();
            let counter = consumer_counter;
            move || {
                if let Some(ref c) = counter {
                    c.fetch_sub(1, Ordering::Relaxed);
                }
                warn!("Consumer {} closed for participant {}", consumer_id, participant_id);
            }
        }).detach();

        consumer.on_pause({
            let participant_id = participant_id.clone();
            let consumer_id = consumer_id.clone();
            move || {
                debug!("Consumer {} paused for participant {}", consumer_id, participant_id);
            }
        }).detach();

        consumer.on_resume({
            let participant_id = participant_id.clone();
            let consumer_id = consumer_id.clone();
            move || {
                debug!("Consumer {} resumed for participant {}", consumer_id, participant_id);
            }
        }).detach();

        consumer.on_producer_pause({
            let participant_id = participant_id.clone();
            let consumer_id = consumer_id.clone();
            move || {
                debug!("Producer paused for consumer {} of participant {}",
                       consumer_id, participant_id);
            }
        }).detach();

        consumer.on_producer_resume({
            let participant_id = participant_id.clone();
            let consumer_id = consumer_id.clone();
            move || {
                debug!("Producer resumed for consumer {} of participant {}",
                       consumer_id, participant_id);
            }
        }).detach();

        // Notify client when active simulcast layers change
        if let Some(sender) = sender {
            consumer.on_layers_change({
                let consumer_id = consumer_id.clone();
                let participant_id = participant_id.clone();
                move |layers| {
                    debug!("Consumer {} layers changed to {:?} for participant {}",
                           consumer_id, layers, participant_id);
                    let msg = ServerMessage::ConsumerLayersChanged {
                        consumer_id: consumer_id.clone(),
                        spatial_layer: layers.as_ref().map(|l| l.spatial_layer),
                        temporal_layer: layers.as_ref().and_then(|l| l.temporal_layer),
                    };
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = sender.try_send(Arc::new(json));
                    }
                }
            }).detach();
        }
    }

    /// Gets transport statistics
    pub async fn get_transport_stats(&self, participant_id: &str, transport_type: &str) -> MediaResult<String> {
        let participant_lock = self.get_participant_lock(participant_id)?;
        let participant = participant_lock.lock().await;

        let transport = match transport_type {
            "send" => &participant.send_transport,
            "recv" => &participant.recv_transport,
            _ => return Err(MediaError::InvalidState("Invalid transport type".to_string())),
        };

        let transport = transport
            .as_ref()
            .ok_or_else(|| MediaError::TransportError(format!("{transport_type} transport not found")))?;

        Ok(format!("{:?}", transport.id()))
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
            debug!("Closed all transports for participant: {}", participant_id);
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::config::{MediaConfig, RouterConfig};
    use crate::media::worker_manager::WorkerManager;
    use crate::media::router_manager::RouterManager;

    #[tokio::test]
    async fn test_transport_creation() {
        let config = Arc::new(MediaConfig::default());
        let worker_manager = Arc::new(WorkerManager::new(config.clone()).await.unwrap());
        let router_manager = RouterManager::new(worker_manager.clone());
        let transport_manager = TransportManager::new();

        // Create router first
        let room_id = "test-room".to_string();
        router_manager.create_router(room_id.clone(), RouterConfig::default()).await.unwrap();
        let router = router_manager.get_router(&room_id).await.unwrap();

        // Look up the WebRtcServer for this room's worker
        let worker_id = router_manager.get_worker_id(&room_id).await.unwrap();
        let webrtc_server = worker_manager.get_webrtc_server(worker_id).await.unwrap();

        // Create transports via WebRtcServer (shared port)
        let participant_id = "test-participant".to_string();
        let send_transport = transport_manager
            .create_send_transport(participant_id.clone(), &router, webrtc_server.clone())
            .await;

        assert!(send_transport.is_ok());

        let recv_transport = transport_manager
            .create_recv_transport(participant_id.clone(), &router, webrtc_server)
            .await;

        assert!(recv_transport.is_ok());
    }
}
