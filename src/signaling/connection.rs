#![forbid(unsafe_code)]

// WebSocket connection handler for individual clients

use super::protocol::{ClientMessage, ServerMessage};
use crate::metrics::ServerMetrics;
use crate::room::RoomManager;
use crate::turn::TurnConfig;
use axum::extract::ws::{Message, WebSocket};
use futures_util::{SinkExt, StreamExt};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::RwLock as StdRwLock;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio::sync::OwnedSemaphorePermit;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

/// Bounded channel capacity per client.
/// At 100 msg/s rate limit, 64 slots = 640ms of burst buffer.
/// Messages queued beyond this are stale — drop them early.
const CHANNEL_CAPACITY: usize = 64;

/// Idle timeout — close connection if no message received within this duration.
/// Prevents Slowloris-style attacks that hold semaphore permits indefinitely.
const IDLE_TIMEOUT: Duration = Duration::from_secs(300); // 5 minutes

/// Maximum consumers per participant. Prevents DoS via unlimited consume requests.
fn max_consumers_per_participant() -> usize {
    std::env::var("MAX_CONSUMERS_PER_PARTICIPANT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(16)
}

/// Token bucket rate limiter: max tokens (burst capacity).
const RATE_LIMIT_MAX_TOKENS: u64 = 100;
/// Token bucket: refill rate in tokens per second.
const RATE_LIMIT_REFILL_RATE: u64 = 100;
/// Internal: 1 token in microseconds (for integer math).
const TOKEN_US: u64 = 1_000_000;
/// Internal: max tokens in microseconds.
const MAX_TOKENS_US: u64 = RATE_LIMIT_MAX_TOKENS * TOKEN_US;

/// Grace period entry for a disconnected participant
struct GraceEntry {
    room_id: String,
    reconnect_token: String,
    timer: tokio::task::JoinHandle<()>,
}

/// Shared map of participants in grace period (disconnected but not yet removed)
#[derive(Clone)]
pub struct GracePeriodMap {
    inner: Arc<StdRwLock<HashMap<String, GraceEntry>>>,
}

impl GracePeriodMap {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(StdRwLock::new(HashMap::new())),
        }
    }

    fn insert(&self, participant_id: String, entry: GraceEntry) {
        let mut map = self.inner.write().unwrap_or_else(|e| e.into_inner());
        // Cancel any existing grace timer for this participant
        if let Some(old) = map.remove(&participant_id) {
            old.timer.abort();
        }
        map.insert(participant_id, entry);
    }

    fn remove(&self, participant_id: &str) -> Option<GraceEntry> {
        let mut map = self.inner.write().unwrap_or_else(|e| e.into_inner());
        map.remove(participant_id)
    }
}

/// Serialize a ServerMessage and send it through the channel as pre-serialized JSON.
fn send_json(
    sender: &mpsc::Sender<Arc<String>>,
    msg: &ServerMessage,
) -> anyhow::Result<()> {
    let json = Arc::new(serde_json::to_string(msg)?);
    sender.try_send(json).map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(())
}

/// Handles a single WebSocket connection
pub async fn handle_connection(
    socket: WebSocket,
    room_manager: Arc<RoomManager>,
    turn_config: Option<Arc<TurnConfig>>,
    grace_periods: GracePeriodMap,
    metrics: ServerMetrics,
    _permit: OwnedSemaphorePermit,
) {
    let mut participant_id = Uuid::new_v4().to_string();
    info!("New WebSocket connection: {}", participant_id);

    metrics.inc_connections_total();
    let _conn_guard = metrics.connection_active_guard();

    // Generate reconnect token for this session
    let mut reconnect_token = Uuid::new_v4().to_string();

    let (mut ws_sender, mut ws_receiver) = socket.split();

    // Bounded channel for sending messages to this client
    let (tx, mut rx) = mpsc::channel::<Arc<String>>(CHANNEL_CAPACITY);

    // Clone for the send task
    let participant_id_clone = participant_id.clone();
    let send_metrics = metrics.clone();

    // Spawn task to send messages to client
    let send_task = tokio::spawn(async move {
        while let Some(json) = rx.recv().await {
            send_metrics.inc_messages_sent();
            if ws_sender.send(Message::Text((*json).clone().into())).await.is_err() {
                break;
            }
        }
        debug!("Send task finished for participant: {}", participant_id_clone);
    });

    // Handle incoming messages
    let mut current_room_id: Option<String> = None;
    let mut stats_task: Option<tokio::task::JoinHandle<()>> = None;
    let mut bwe_sender: Option<mpsc::Sender<u32>> = None;

    // Token bucket rate limiter state
    let mut tokens_us: u64 = MAX_TOKENS_US;
    let mut last_refill = Instant::now();
    let mut rate_limit_warned = false;

    loop {
        // Idle timeout: close connection if no message within IDLE_TIMEOUT
        let msg = match tokio::time::timeout(IDLE_TIMEOUT, ws_receiver.next()).await {
            Ok(Some(Ok(message))) => message,
            Ok(Some(Err(_))) | Ok(None) => break, // Stream error or closed
            Err(_) => {
                warn!("Idle timeout for participant {}", participant_id);
                break;
            }
        };

        match msg {
            Message::Text(text) => {
                metrics.inc_messages_received();

                // Token bucket rate limiting
                let now = Instant::now();
                let elapsed_us = now.duration_since(last_refill).as_micros() as u64;
                last_refill = now;
                // Refill: RATE_LIMIT_REFILL_RATE tokens per second = that many token-microseconds per microsecond
                tokens_us = (tokens_us + elapsed_us * RATE_LIMIT_REFILL_RATE).min(MAX_TOKENS_US);

                if tokens_us >= TOKEN_US {
                    tokens_us -= TOKEN_US;
                    rate_limit_warned = false;
                } else {
                    // Rate limited
                    if !rate_limit_warned {
                        rate_limit_warned = true;
                        warn!("Rate limit exceeded for participant {}", participant_id);
                        let _ = send_json(&tx, &ServerMessage::Error {
                            message: format!("Rate limit exceeded: max {} messages/second", RATE_LIMIT_REFILL_RATE),
                        });
                    }
                    continue;
                }

                match serde_json::from_str::<ClientMessage>(&text) {
                    Ok(client_msg) => {
                        // Handle reconnect specially — it must be processed before
                        // any room-dependent messages
                        if let ClientMessage::Reconnect {
                            participant_id: reconnect_id,
                            room_id: reconnect_room,
                            reconnect_token: client_token,
                        } = &client_msg {
                            let success = handle_reconnect(
                                reconnect_id,
                                reconnect_room,
                                client_token,
                                &grace_periods,
                                &room_manager,
                                &tx,
                            ).await;

                            if success {
                                participant_id = reconnect_id.clone();
                                current_room_id = Some(reconnect_room.clone());
                                // Preserve the reconnect token (was validated in handle_reconnect)
                                reconnect_token = client_token.clone();

                                // Restart stats task for reconnected session
                                if let Some(task) = stats_task.take() {
                                    task.abort();
                                }

                                let (bwe_tx, bwe_rx) = mpsc::channel::<u32>(32);
                                // Try to subscribe immediately (recv transport may exist from previous session)
                                if let Err(e) = room_manager.subscribe_bwe_events(&participant_id, bwe_tx.clone()).await {
                                    debug!("BWE subscription deferred for reconnecting {}: {}", participant_id, e);
                                }
                                bwe_sender = Some(bwe_tx);

                                stats_task = Some(spawn_stats_task(
                                    room_manager.clone(),
                                    participant_id.clone(),
                                    tx.clone(),
                                    bwe_rx,
                                ));
                            }

                            let _ = send_json(&tx, &ServerMessage::ReconnectResult {
                                success,
                                participant_id: if success { participant_id.clone() } else { reconnect_id.clone() },
                            });
                            continue;
                        }

                        // Track room changes to manage stats task lifecycle
                        let was_in_room = current_room_id.is_some();
                        let is_join = matches!(&client_msg, ClientMessage::JoinRoom { .. });
                        let is_leave = matches!(&client_msg, ClientMessage::LeaveRoom);

                        let start = Instant::now();
                        let result = handle_client_message(
                            &client_msg,
                            &participant_id,
                            &mut current_room_id,
                            &tx,
                            &room_manager,
                            &turn_config,
                            &metrics,
                            &reconnect_token,
                            &bwe_sender,
                        ).await;
                        metrics.observe_message_handling(start.elapsed());

                        if let Err(e) = result {
                            error!("Error handling message: {}", e);
                            metrics.inc_errors();
                            // If channel is closed, send task has exited — break
                            if tx.is_closed() {
                                break;
                            }
                            let _ = send_json(&tx, &ServerMessage::Error {
                                message: format!("Error: {e}"),
                            });
                        }

                        // Start stats task when joining a room
                        if is_join && current_room_id.is_some() {
                            // Generate a fresh reconnect token for the new session
                            reconnect_token = Uuid::new_v4().to_string();

                            if let Some(task) = stats_task.take() {
                                task.abort();
                            }

                            // Create BWE event channel (subscription happens later in CreateRecvTransport)
                            let (bwe_tx, bwe_rx) = mpsc::channel::<u32>(32);
                            bwe_sender = Some(bwe_tx);

                            stats_task = Some(spawn_stats_task(
                                room_manager.clone(),
                                participant_id.clone(),
                                tx.clone(),
                                bwe_rx,
                            ));
                        }

                        // Stop stats task when leaving a room
                        if is_leave || (was_in_room && current_room_id.is_none()) {
                            if let Some(task) = stats_task.take() {
                                task.abort();
                            }
                            bwe_sender = None;
                        }
                    }
                    Err(e) => {
                        warn!("Invalid message format: {}", e);
                        metrics.inc_errors();
                        let _ = send_json(&tx, &ServerMessage::Error {
                            message: format!("Invalid message format: {e}"),
                        });
                    }
                }
            }
            Message::Close(_) => {
                info!("Client {} closed connection", participant_id);
                break;
            }
            Message::Ping(_) | Message::Pong(_) => {
                // WebSocket ping/pong handled automatically
            }
            _ => {
                warn!("Unexpected message type from client {}", participant_id);
            }
        }
    }

    // Stop stats task
    if let Some(task) = stats_task.take() {
        task.abort();
    }

    // On disconnect: start grace period instead of immediate cleanup
    if let Some(room_id) = current_room_id.take() {
        info!("Participant {} disconnected from room {}, starting 30s grace period", participant_id, room_id);

        let grace_map = grace_periods.clone();
        let rm = room_manager.clone();
        let pid = participant_id.clone();
        let rid = room_id.clone();

        let timer = tokio::spawn(async move {
            tokio::time::sleep(tokio::time::Duration::from_secs(30)).await;
            // Grace period expired — clean up
            info!("Grace period expired for participant {} in room {}", pid, rid);
            grace_map.remove(&pid);
            if let Err(e) = rm.remove_participant(&rid, &pid).await {
                error!("Error removing participant after grace period: {}", e);
            }
        });

        grace_periods.insert(participant_id.clone(), GraceEntry {
            room_id,
            reconnect_token,
            timer,
        });
    }

    // _conn_guard dropped here → dec_connections_active
    // _permit dropped here → release semaphore

    drop(tx);
    let _ = send_task.await;

    info!("Connection handler finished for participant: {}", participant_id);
}

/// Attempt to reconnect a participant to their existing session
async fn handle_reconnect(
    participant_id: &str,
    room_id: &str,
    client_token: &str,
    grace_periods: &GracePeriodMap,
    room_manager: &Arc<RoomManager>,
    new_sender: &mpsc::Sender<Arc<String>>,
) -> bool {
    // Check if participant is in grace period
    if let Some(entry) = grace_periods.remove(participant_id) {
        // Cancel the cleanup timer
        entry.timer.abort();

        // Verify reconnect token matches (prevents session hijacking)
        if entry.reconnect_token != client_token {
            warn!("Reconnect token mismatch for participant {}", participant_id);
            // Re-schedule cleanup — the participant is still in the room
            let grace_map = grace_periods.clone();
            let rm = room_manager.clone();
            let pid = participant_id.to_string();
            let rid = entry.room_id.clone();
            let timer = tokio::spawn(async move {
                tokio::time::sleep(tokio::time::Duration::from_secs(30)).await;
                info!("Grace period expired for participant {} in room {}", pid, rid);
                grace_map.remove(&pid);
                if let Err(e) = rm.remove_participant(&rid, &pid).await {
                    error!("Error removing participant after grace period: {}", e);
                }
            });
            grace_periods.insert(participant_id.to_string(), GraceEntry {
                room_id: entry.room_id,
                reconnect_token: entry.reconnect_token,
                timer,
            });
            return false;
        }

        // Verify room_id matches
        if entry.room_id != room_id {
            warn!("Reconnect room mismatch: expected {}, got {}", entry.room_id, room_id);
            // Re-schedule cleanup — the participant is still in the old room
            let grace_map = grace_periods.clone();
            let rm = room_manager.clone();
            let pid = participant_id.to_string();
            let rid = entry.room_id.clone();
            let timer = tokio::spawn(async move {
                tokio::time::sleep(tokio::time::Duration::from_secs(30)).await;
                info!("Grace period expired for participant {} in room {}", pid, rid);
                grace_map.remove(&pid);
                if let Err(e) = rm.remove_participant(&rid, &pid).await {
                    error!("Error removing participant after grace period: {}", e);
                }
            });
            grace_periods.insert(participant_id.to_string(), GraceEntry {
                room_id: entry.room_id,
                reconnect_token: entry.reconnect_token,
                timer,
            });
            return false;
        }

        // Rebind the sender in the room's participant list
        match room_manager.rebind_participant_sender(room_id, participant_id, new_sender.clone()).await {
            Ok(true) => {
                info!("Participant {} reconnected to room {}", participant_id, room_id);
                true
            }
            Ok(false) => {
                warn!("Participant {} not found in room {} during reconnect", participant_id, room_id);
                false
            }
            Err(e) => {
                error!("Error during reconnect for {}: {}", participant_id, e);
                false
            }
        }
    } else {
        debug!("No grace period found for participant {}", participant_id);
        false
    }
}

/// Spawns a background task that performs bandwidth adaptation and sends connection stats.
/// Returns a JoinHandle that can be aborted on disconnect.
///
/// Uses adaptive polling intervals to reduce CPU at scale:
/// - 2s when consumers are active and bandwidth is changing
/// - 5s when consumers are active but bandwidth tier is stable (3+ consecutive same-tier polls)
/// - 10s when no consumers exist (publish-only, no layer adaptation needed)
/// Spawns a background task that performs bandwidth adaptation and sends connection stats.
///
/// Uses event-driven BWE (bandwidth estimation) events for layer adaptation:
/// - BWE events arrive via channel when mediasoup detects bandwidth changes
/// - Layer preferences update immediately on tier change (debounced at 500ms)
/// - ConnectionStats sent to client every ~10s using last-known bitrate (no IPC)
fn spawn_stats_task(
    room_manager: Arc<RoomManager>,
    participant_id: String,
    sender: mpsc::Sender<Arc<String>>,
    mut bwe_rx: mpsc::Receiver<u32>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut current_layers: HashMap<String, u8> = HashMap::new();
        let mut last_bitrate: u32 = 0;
        let mut last_tier: Option<u8> = None;

        // Minimum interval between layer updates (debounce rapid BWE fluctuations)
        let mut last_layer_update = Instant::now();
        let debounce = Duration::from_millis(500);

        // ConnectionStats interval — no IPC call, just uses last-known bitrate
        let mut stats_interval = tokio::time::interval(Duration::from_secs(10));
        stats_interval.tick().await; // Skip first immediate tick

        loop {
            tokio::select! {
                // BWE event: bandwidth estimation changed
                bwe = bwe_rx.recv() => {
                    match bwe {
                        Some(bitrate) => {
                            last_bitrate = bitrate;

                            let target_layer = if bitrate < 200_000 {
                                0u8
                            } else if bitrate < 600_000 {
                                1u8
                            } else {
                                2u8
                            };

                            // Only update layers on tier change, with debounce
                            if last_tier != Some(target_layer) && last_layer_update.elapsed() >= debounce {
                                last_tier = Some(target_layer);
                                last_layer_update = Instant::now();

                                // Get current consumer IDs (still needs participant lock, but no IPC)
                                match room_manager.get_recv_transport_stats(&participant_id).await {
                                    Ok(Some((_bitrate, consumer_ids))) => {
                                        for consumer_id in &consumer_ids {
                                            let prev = current_layers.get(consumer_id).copied();
                                            if prev != Some(target_layer) {
                                                if let Err(e) = room_manager.set_preferred_layers(
                                                    &participant_id,
                                                    consumer_id,
                                                    target_layer,
                                                    None,
                                                ).await {
                                                    debug!("Failed to set layers for consumer {}: {}", consumer_id, e);
                                                } else {
                                                    current_layers.insert(consumer_id.clone(), target_layer);
                                                }
                                            }
                                        }
                                        current_layers.retain(|id, _| consumer_ids.contains(id));
                                    }
                                    Ok(None) => {
                                        current_layers.clear();
                                    }
                                    Err(e) => {
                                        debug!("Failed to get consumer IDs for {}: {}", participant_id, e);
                                    }
                                }
                            }
                        }
                        None => break, // Channel closed — participant disconnected
                    }
                }

                // Timer: send ConnectionStats every ~10s using last-known bitrate (zero IPC)
                _ = stats_interval.tick() => {
                    if last_bitrate > 0 {
                        if let Ok(json) = serde_json::to_string(&ServerMessage::ConnectionStats {
                            available_bitrate: Some(last_bitrate),
                            rtt: None,
                        }) {
                            let _ = sender.try_send(Arc::new(json));
                        }
                    }
                }
            }
        }
    })
}

/// Generate ice_servers list for a participant (TURN credentials if configured)
fn make_ice_servers(turn_config: &Option<Arc<TurnConfig>>, participant_id: &str) -> Vec<crate::turn::IceServer> {
    match turn_config {
        Some(tc) => vec![tc.generate_credentials(participant_id)],
        None => vec![],
    }
}

const MAX_ROOM_ID_LEN: usize = 128;
const MAX_PARTICIPANT_NAME_LEN: usize = 64;
const MAX_CHAT_LEN: usize = 4096;

/// Handle a single client message
async fn handle_client_message(
    message: &ClientMessage,
    participant_id: &str,
    current_room_id: &mut Option<String>,
    sender: &mpsc::Sender<Arc<String>>,
    room_manager: &Arc<RoomManager>,
    turn_config: &Option<Arc<TurnConfig>>,
    metrics: &ServerMetrics,
    reconnect_token: &str,
    bwe_sender: &Option<mpsc::Sender<u32>>,
) -> anyhow::Result<()> {
    match message {
        ClientMessage::JoinRoom { room_id, participant_name } => {
            if room_id.is_empty() || room_id.len() > MAX_ROOM_ID_LEN {
                anyhow::bail!("Invalid room_id: must be 1-{MAX_ROOM_ID_LEN} characters");
            }
            if participant_name.is_empty() || participant_name.len() > MAX_PARTICIPANT_NAME_LEN {
                anyhow::bail!("Invalid participant_name: must be 1-{MAX_PARTICIPANT_NAME_LEN} characters");
            }
            // Leave current room if in one
            if let Some(old_room_id) = current_room_id.take() {
                room_manager.remove_participant(&old_room_id, participant_id).await?;
            }

            // Join new room
            let participants = room_manager
                .add_participant(room_id, participant_id.to_string(), participant_name.clone(), sender.clone())
                .await?;

            *current_room_id = Some(room_id.clone());
            metrics.inc_joins();

            send_json(sender, &ServerMessage::RoomJoined {
                participant_id: participant_id.to_string(),
                participants,
                reconnect_token: reconnect_token.to_string(),
            })?;
        }

        ClientMessage::LeaveRoom => {
            if let Some(room_id) = current_room_id.take() {
                room_manager.remove_participant(&room_id, participant_id).await?;
                metrics.inc_leaves();
            }
        }

        ClientMessage::GetRouterRtpCapabilities => {
            if let Some(room_id) = current_room_id.as_ref() {
                let capabilities = room_manager.get_router_rtp_capabilities(room_id).await?;
                send_json(sender, &ServerMessage::RouterRtpCapabilities {
                    rtp_capabilities: capabilities,
                })?;
            } else {
                anyhow::bail!("Not in a room");
            }
        }

        ClientMessage::CreateSendTransport => {
            if let Some(room_id) = current_room_id.as_ref() {
                let transport_info = room_manager
                    .create_send_transport(room_id, participant_id)
                    .await?;

                send_json(sender, &ServerMessage::TransportCreated {
                    transport_id: transport_info.id,
                    ice_parameters: transport_info.ice_parameters,
                    ice_candidates: transport_info.ice_candidates,
                    dtls_parameters: transport_info.dtls_parameters,
                    ice_servers: make_ice_servers(turn_config, participant_id),
                })?;
            } else {
                anyhow::bail!("Not in a room");
            }
        }

        ClientMessage::CreateRecvTransport => {
            if let Some(room_id) = current_room_id.as_ref() {
                let transport_info = room_manager
                    .create_recv_transport(room_id, participant_id)
                    .await?;

                // Subscribe to BWE events now that recv transport exists
                if let Some(ref bwe_tx) = bwe_sender {
                    if let Err(e) = room_manager.subscribe_bwe_events(participant_id, bwe_tx.clone()).await {
                        debug!("Failed to subscribe BWE events for {}: {}", participant_id, e);
                    }
                }

                send_json(sender, &ServerMessage::TransportCreated {
                    transport_id: transport_info.id,
                    ice_parameters: transport_info.ice_parameters,
                    ice_candidates: transport_info.ice_candidates,
                    dtls_parameters: transport_info.dtls_parameters,
                    ice_servers: make_ice_servers(turn_config, participant_id),
                })?;
            } else {
                anyhow::bail!("Not in a room");
            }
        }

        ClientMessage::ConnectTransport { transport_id, dtls_parameters } => {
            if let Some(room_id) = current_room_id.as_ref() {
                room_manager
                    .connect_transport(room_id, participant_id, transport_id, dtls_parameters.clone())
                    .await?;

                send_json(sender, &ServerMessage::TransportConnected { transport_id: transport_id.clone() })?;
            } else {
                anyhow::bail!("Not in a room");
            }
        }

        ClientMessage::Produce { transport_id: _, kind, rtp_parameters } => {
            if let Some(room_id) = current_room_id.as_ref() {
                let producer_id = room_manager
                    .create_producer(room_id, participant_id, *kind, rtp_parameters.clone())
                    .await?;

                metrics.inc_producers_created();
                send_json(sender, &ServerMessage::ProducerCreated { producer_id })?;
            } else {
                anyhow::bail!("Not in a room");
            }
        }

        ClientMessage::Consume { producer_id, rtp_capabilities } => {
            if let Some(room_id) = current_room_id.as_ref() {
                // Server-side consumer cap
                let cap = max_consumers_per_participant();
                match room_manager.media_server().transport_manager().consumer_count(participant_id).await {
                    Ok(count) if count >= cap => {
                        anyhow::bail!("Consumer limit reached ({cap}). Cannot create more consumers.");
                    }
                    _ => {} // Under cap or not tracked yet, proceed
                }

                let producer_paused = room_manager
                    .media_server()
                    .transport_manager()
                    .find_producer_paused(producer_id)
                    .await
                    .unwrap_or(false);

                let consumer_info = room_manager
                    .create_consumer(room_id, participant_id, producer_id.parse()?, rtp_capabilities.clone(), Some(sender.clone()), producer_paused)
                    .await?;

                metrics.inc_consumers_created();
                send_json(sender, &ServerMessage::ConsumerCreated {
                    consumer_id: consumer_info.id,
                    producer_id: consumer_info.producer_id,
                    kind: consumer_info.kind,
                    rtp_parameters: consumer_info.rtp_parameters,
                })?;
            } else {
                anyhow::bail!("Not in a room");
            }
        }

        ClientMessage::ResumeConsumer { consumer_id } => {
            if let Some(room_id) = current_room_id.as_ref() {
                room_manager
                    .resume_consumer(room_id, participant_id, consumer_id)
                    .await?;

                send_json(sender, &ServerMessage::ConsumerResumed { consumer_id: consumer_id.clone() })?;
            } else {
                anyhow::bail!("Not in a room");
            }
        }

        ClientMessage::PauseConsumer { consumer_id } => {
            if let Some(room_id) = current_room_id.as_ref() {
                room_manager
                    .pause_consumer(room_id, participant_id, consumer_id)
                    .await?;

                send_json(sender, &ServerMessage::ConsumerPaused { consumer_id: consumer_id.clone() })?;
            } else {
                anyhow::bail!("Not in a room");
            }
        }

        ClientMessage::CloseProducer { producer_id } => {
            if let Some(room_id) = current_room_id.as_ref() {
                room_manager
                    .close_producer(room_id, participant_id, producer_id)
                    .await?;
            } else {
                anyhow::bail!("Not in a room");
            }
        }

        ClientMessage::PauseProducer { producer_id } => {
            if let Some(room_id) = current_room_id.as_ref() {
                room_manager.pause_producer(room_id, participant_id, producer_id).await?;
                send_json(sender, &ServerMessage::ProducerPaused { producer_id: producer_id.clone() })?;
            } else {
                anyhow::bail!("Not in a room");
            }
        }

        ClientMessage::ResumeProducer { producer_id } => {
            if let Some(room_id) = current_room_id.as_ref() {
                room_manager.resume_producer(room_id, participant_id, producer_id).await?;
                send_json(sender, &ServerMessage::ProducerResumed { producer_id: producer_id.clone() })?;
            } else {
                anyhow::bail!("Not in a room");
            }
        }

        ClientMessage::Reconnect { .. } => {
            // Handled in the main message loop before dispatching here
            // This branch should never be reached
        }

        ClientMessage::RestartIce { transport_id } => {
            if current_room_id.is_some() {
                let ice_parameters = room_manager
                    .restart_ice(participant_id, transport_id)
                    .await?;

                send_json(sender, &ServerMessage::IceRestarted {
                    transport_id: transport_id.clone(),
                    ice_parameters,
                })?;
            } else {
                anyhow::bail!("Not in a room");
            }
        }

        ClientMessage::SetConsumerPreferredLayers { consumer_id, spatial_layer, temporal_layer } => {
            if current_room_id.is_some() {
                room_manager
                    .set_preferred_layers(participant_id, consumer_id, *spatial_layer, *temporal_layer)
                    .await?;
            } else {
                anyhow::bail!("Not in a room");
            }
        }

        ClientMessage::ChatMessage { content } => {
            if content.is_empty() || content.len() > MAX_CHAT_LEN {
                anyhow::bail!("Invalid chat message: must be 1-{MAX_CHAT_LEN} characters");
            }
            if let Some(room_id) = current_room_id.as_ref() {
                room_manager
                    .broadcast_chat(room_id, participant_id, content.clone())
                    .await?;
            } else {
                anyhow::bail!("Not in a room");
            }
        }
    }

    Ok(())
}
