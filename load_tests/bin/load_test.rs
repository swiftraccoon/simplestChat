#![forbid(unsafe_code)]

//! Load test binary - Spawn multiple synthetic clients to test server scalability
//!
//! Usage:
//!   cargo run --bin load_test -- --clients 10 --duration 30
//!   cargo run --bin load_test -- --clients 1000 --rooms 16 --duration 30
//!   cargo run --bin load_test -- --clients 1000 --mode webinar --duration 60
//!   cargo run --bin load_test -- --clients 100 --churn-rate 5 --duration 60

use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::time::sleep;
use anyhow::Result;
use rand;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use futures_util::{SinkExt, StreamExt};
use mediasoup::prelude::*;
use serde_json;

use simplestChat::signaling::protocol::{ClientMessage, ServerMessage};

// Include modules directly
mod media_generator {
    include!("../clients/media_generator.rs");
}

mod metrics {
    include!("../clients/metrics.rs");
}

mod webrtc_client {
    include!("../clients/webrtc_client.rs");
}

use media_generator::{MediaGenerator, MediaConfig};
use metrics::{MetricsCollector, TestSummary};
use webrtc_client::WebRtcSession;
use webrtc::track::track_local::TrackLocalWriter;
use tokio::sync::Mutex;
use std::num::{NonZeroU32, NonZeroU8};

#[derive(Debug, Clone)]
struct ClientConfig {
    server_url: String,
    room_id: String,
    participant_name: String,
    media_config: MediaConfig,
    session_duration: Duration,
    is_publisher: bool,
    is_churner: bool,
    churn_session_min_secs: u64,
    churn_session_max_secs: u64,
    max_audio_consumers: usize,
    max_video_consumers: usize,
    /// Whether to consume producers that already exist when joining.
    /// Only needed in webinar/panel mode where most clients don't publish.
    /// In conference mode, NewProducer events handle discovery naturally.
    consume_existing_producers: bool,
}

#[derive(Debug)]
struct TestConfig {
    num_clients: usize,
    duration_secs: u64,
    ramp_up_secs: u64,
    server_url: String,
    room_id: String,
    media_config: MediaConfig,
    num_rooms: usize,
    publish_ratio: f64,
    churn_rate: f64,
    max_audio_consumers: usize,
    max_video_consumers: usize,
}

impl Default for TestConfig {
    fn default() -> Self {
        Self {
            num_clients: 5,
            duration_secs: 30,
            ramp_up_secs: 5,
            server_url: "ws://localhost:3000/ws".to_string(),
            room_id: "load-test-room".to_string(),
            media_config: MediaConfig::default(),
            num_rooms: 1,
            publish_ratio: 1.0,
            churn_rate: 0.0,
            max_audio_consumers: DEFAULT_MAX_AUDIO_CONSUMERS,
            max_video_consumers: DEFAULT_MAX_VIDEO_CONSUMERS,
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"))
        )
        .init();

    // Parse command line arguments
    let args: Vec<String> = std::env::args().collect();
    let mut config = TestConfig::default();
    let mut quality_override: Option<String> = None;
    let mut fps_override: Option<u8> = None;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--clients" | "-c" => {
                if i + 1 < args.len() {
                    config.num_clients = args[i + 1].parse().unwrap_or(config.num_clients);
                    i += 2;
                } else {
                    i += 1;
                }
            }
            "--duration" | "-d" => {
                if i + 1 < args.len() {
                    config.duration_secs = args[i + 1].parse().unwrap_or(config.duration_secs);
                    i += 2;
                } else {
                    i += 1;
                }
            }
            "--ramp-up" | "-r" => {
                if i + 1 < args.len() {
                    config.ramp_up_secs = args[i + 1].parse().unwrap_or(config.ramp_up_secs);
                    i += 2;
                } else {
                    i += 1;
                }
            }
            "--server" | "-s" => {
                if i + 1 < args.len() {
                    config.server_url = args[i + 1].clone();
                    i += 2;
                } else {
                    i += 1;
                }
            }
            "--room" => {
                if i + 1 < args.len() {
                    config.room_id = args[i + 1].clone();
                    i += 2;
                } else {
                    i += 1;
                }
            }
            "--rooms" => {
                if i + 1 < args.len() {
                    config.num_rooms = args[i + 1].parse().unwrap_or(1).max(1);
                    i += 2;
                } else {
                    i += 1;
                }
            }
            "--publish-ratio" => {
                if i + 1 < args.len() {
                    config.publish_ratio = args[i + 1].parse::<f64>().unwrap_or(1.0).clamp(0.0, 1.0);
                    i += 2;
                } else {
                    i += 1;
                }
            }
            "--mode" => {
                if i + 1 < args.len() {
                    config.publish_ratio = match args[i + 1].as_str() {
                        "webinar" => 0.01,
                        "panel" => 0.1,
                        "classroom" => 0.2,
                        "conference" => 1.0,
                        other => {
                            eprintln!("Unknown mode '{}', using conference (1.0)", other);
                            1.0
                        }
                    };
                    i += 2;
                } else {
                    i += 1;
                }
            }
            "--churn-rate" => {
                if i + 1 < args.len() {
                    config.churn_rate = args[i + 1].parse::<f64>().unwrap_or(0.0).max(0.0);
                    i += 2;
                } else {
                    i += 1;
                }
            }
            "--max-audio" => {
                if i + 1 < args.len() {
                    config.max_audio_consumers = args[i + 1].parse().unwrap_or(DEFAULT_MAX_AUDIO_CONSUMERS);
                    i += 2;
                } else {
                    i += 1;
                }
            }
            "--max-video" => {
                if i + 1 < args.len() {
                    config.max_video_consumers = args[i + 1].parse().unwrap_or(DEFAULT_MAX_VIDEO_CONSUMERS);
                    i += 2;
                } else {
                    i += 1;
                }
            }
            "--quality" | "-q" => {
                if i + 1 < args.len() {
                    quality_override = Some(args[i + 1].clone());
                    i += 2;
                } else {
                    i += 1;
                }
            }
            "--fps" => {
                if i + 1 < args.len() {
                    fps_override = Some(args[i + 1].parse().unwrap_or(30));
                    i += 2;
                } else {
                    i += 1;
                }
            }
            "--audio-only" => {
                config.media_config = MediaConfig::audio_only();
                i += 1;
            }
            "--video-only" => {
                config.media_config = MediaConfig::video_only();
                i += 1;
            }
            "--help" | "-h" => {
                print_usage();
                return Ok(());
            }
            _ => {
                i += 1;
            }
        }
    }

    // Apply quality preset (after all args parsed so order doesn't matter)
    if let Some(ref quality) = quality_override {
        let fps = fps_override.unwrap_or(30);
        let fps = match fps {
            15 | 30 | 60 => fps,
            _ => { eprintln!("Invalid FPS '{}', using 30", fps); 30 }
        };
        config.media_config = MediaConfig::from_preset(quality, fps);
    } else if let Some(fps) = fps_override {
        let fps = match fps {
            15 | 30 | 60 => fps,
            _ => { eprintln!("Invalid FPS '{}', using 30", fps); 30 }
        };
        config.media_config.video_fps = fps;
        let multiplier = match fps { 15 => 0.6, 60 => 1.5, _ => 1.0 };
        config.media_config.video_bitrate_kbps =
            (config.media_config.video_bitrate_kbps as f64 * multiplier) as u32;
    }

    run_load_test(config).await
}

async fn run_load_test(config: TestConfig) -> Result<()> {
    let num_publishers = ((config.num_clients as f64) * config.publish_ratio).ceil() as usize;
    let num_publishers = num_publishers.max(1).min(config.num_clients);
    let num_churners = if config.churn_rate > 0.0 {
        ((config.churn_rate * config.duration_secs as f64) as usize).min(config.num_clients)
    } else {
        0
    };
    let clients_per_room = if config.num_rooms > 0 {
        (config.num_clients + config.num_rooms - 1) / config.num_rooms
    } else {
        config.num_clients
    };

    println!("\n=== Starting Load Test ===");
    println!("Clients: {}", config.num_clients);
    println!("Duration: {}s", config.duration_secs);
    println!("Ramp-up: {}s", config.ramp_up_secs);
    println!("Server: {}", config.server_url);
    println!("Rooms: {} ({} clients/room avg)", config.num_rooms, clients_per_room);
    println!("Publishers: {}/{} ({:.0}% publish ratio)",
        num_publishers, config.num_clients, config.publish_ratio * 100.0);
    if num_churners > 0 {
        println!("Churners: {} (rate: {}/s)", num_churners, config.churn_rate);
    }
    println!("Media: Audio={}, Video={}",
        config.media_config.audio_enabled,
        config.media_config.video_enabled
    );
    println!("Quality: {} ({} kbps video, {} kbps audio)",
        config.media_config.quality_label(),
        config.media_config.video_bitrate_kbps,
        config.media_config.audio_bitrate_kbps,
    );
    println!("Max consumers: audio={}, video={}",
        config.max_audio_consumers, config.max_video_consumers);
    println!("========================\n");

    let session_duration = Duration::from_secs(config.duration_secs);
    let ramp_up_delay = if config.num_clients > 1 {
        Duration::from_secs(config.ramp_up_secs) / config.num_clients as u32
    } else {
        Duration::from_secs(0)
    };

    let mut handles = Vec::new();
    let mut metrics_collectors = Vec::new();

    // Churner clients are the LAST N clients
    let churner_start_idx = config.num_clients.saturating_sub(num_churners);

    // Spawn clients with gradual ramp-up
    for i in 0..config.num_clients {
        let client_id = format!("client-{}", i);
        let participant_name = format!("TestUser{}", i);
        let room_id = if config.num_rooms <= 1 {
            config.room_id.clone()
        } else {
            format!("{}-{}", config.room_id, i % config.num_rooms)
        };
        let is_publisher = i < num_publishers;
        let is_churner = i >= churner_start_idx;

        let client_config = ClientConfig {
            server_url: config.server_url.clone(),
            room_id,
            participant_name,
            media_config: config.media_config.clone(),
            session_duration,
            is_publisher,
            is_churner,
            churn_session_min_secs: 5,
            churn_session_max_secs: 30,
            max_audio_consumers: config.max_audio_consumers,
            max_video_consumers: config.max_video_consumers,
            consume_existing_producers: true,
        };

        let metrics = Arc::new(MetricsCollector::new(client_id.clone()));
        metrics_collectors.push(metrics.clone());

        let handle = tokio::spawn(run_client(client_config, client_id, metrics));
        handles.push(handle);

        if i > 0 && i < config.num_clients - 1 {
            tracing::info!("Waiting {}ms before spawning next client...", ramp_up_delay.as_millis());
            sleep(ramp_up_delay).await;
        }
    }

    // Hard deadline: at 1000 clients, all tokio worker threads get saturated with
    // CPU-bound webrtc-rs SDP processing. No async operations (timers, channels,
    // select, even .await on spawn_blocking) can complete because no worker is free
    // to poll them. Solution: OS thread that sleeps independently of tokio, then
    // generates reports synchronously (MetricsCollector uses std::sync::Mutex)
    // and calls process::exit(0) to bypass the stuck runtime.
    let deadline_secs = config.ramp_up_secs + config.duration_secs + 120;
    println!("All clients spawned. Running test for {}s (deadline: {}s)...\n",
        config.duration_secs, deadline_secs);

    // Clone collectors for the OS deadline thread
    let collectors_for_deadline: Vec<Arc<MetricsCollector>> =
        metrics_collectors.iter().cloned().collect();

    // OS deadline thread — completely independent of tokio runtime
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(deadline_secs));
        eprintln!("\n=== DEADLINE REACHED ({deadline_secs}s) — generating results from OS thread ===");
        write_results_sync(&collectors_for_deadline);
        std::process::exit(0);
    });

    // Normal path: wait for all client tasks to complete.
    // For small tests (2-100 clients), this finishes well before the deadline.
    for handle in handles {
        let _ = handle.await;
    }

    // If we get here, the runtime isn't stuck — generate results normally
    println!("All clients completed.");
    write_results_sync(&metrics_collectors);

    Ok(())
}

/// Write results to JSON files. Fully synchronous — safe to call from OS thread.
fn write_results_sync(collectors: &[Arc<MetricsCollector>]) {
    let mut all_metrics = Vec::new();
    for collector in collectors {
        all_metrics.push(collector.generate_report());
    }

    let summary = TestSummary::from_metrics(&all_metrics);
    summary.print_summary();

    match serde_json::to_string_pretty(&all_metrics) {
        Ok(json) => {
            if let Err(e) = std::fs::write("load_test_results.json", json) {
                eprintln!("Failed to write results: {}", e);
            } else {
                println!("Detailed results saved to: load_test_results.json");
            }
        }
        Err(e) => eprintln!("Failed to serialize results: {}", e),
    }

    match serde_json::to_string_pretty(&summary) {
        Ok(json) => {
            if let Err(e) = std::fs::write("load_test_summary.json", json) {
                eprintln!("Failed to write summary: {}", e);
            } else {
                println!("Summary saved to: load_test_summary.json");
            }
        }
        Err(e) => eprintln!("Failed to serialize summary: {}", e),
    }
}

async fn run_client(
    config: ClientConfig,
    client_id: String,
    metrics: Arc<MetricsCollector>,
) {
    if config.is_churner {
        run_churner_client(config, client_id, metrics).await;
    } else {
        match run_client_inner(config, client_id.clone(), metrics).await {
            Ok(_) => tracing::info!("{}: Client completed successfully", client_id),
            Err(e) => tracing::error!("{}: Client failed: {}", client_id, e),
        }
    }
}

/// Churner client: repeatedly connects, runs for a random duration, disconnects, and reconnects
async fn run_churner_client(
    config: ClientConfig,
    client_id: String,
    metrics: Arc<MetricsCollector>,
) {
    let total_start = Instant::now();
    let total_duration = config.session_duration;
    let mut iteration = 0u32;

    while total_start.elapsed() < total_duration {
        iteration += 1;
        tracing::info!("{}: Churn iteration {} starting", client_id, iteration);

        // Random session duration between min and max
        let session_secs = config.churn_session_min_secs +
            (rand::random::<u64>() % (config.churn_session_max_secs - config.churn_session_min_secs + 1));
        let remaining = total_duration.saturating_sub(total_start.elapsed());
        let this_session = Duration::from_secs(session_secs).min(remaining);

        if this_session.as_secs() < 3 {
            break; // Not enough time for a meaningful session
        }

        let mut churn_config = config.clone();
        churn_config.session_duration = this_session;

        match run_client_inner(churn_config, client_id.clone(), metrics.clone()).await {
            Ok(_) => {
                metrics.record_reconnection();
                tracing::info!("{}: Churn iteration {} completed ({}s)", client_id, iteration, this_session.as_secs());
            }
            Err(e) => {
                metrics.record_reconnection_failure();
                tracing::warn!("{}: Churn iteration {} failed: {}", client_id, iteration, e);
            }
        }

        // Cooldown before reconnecting
        let remaining = total_duration.saturating_sub(total_start.elapsed());
        if remaining.as_secs() < 5 {
            break;
        }
        sleep(Duration::from_secs(2)).await;
    }
}

async fn run_client_inner(
    config: ClientConfig,
    client_id: String,
    metrics: Arc<MetricsCollector>,
) -> Result<()> {
    tracing::info!("{}: Starting synthetic client (publisher={}, room={})",
        client_id, config.is_publisher, config.room_id);

    metrics.set_room_id(&config.room_id);

    // Connect to WebSocket signaling server
    let (ws_stream, _) = connect_async(&config.server_url)
        .await
        .map_err(|e| {
            tracing::error!("{}: Failed to connect: {}", client_id, e);
            e
        })?;

    tracing::info!("{}: WebSocket connected", client_id);

    let (mut write, mut read) = ws_stream.split();

    // Buffer for async events received during setup phase
    let mut buffered_events: Vec<ServerMessage> = Vec::new();
    // Producers that already existed when we joined — consumed after setup, not during
    let mut existing_producer_events: Vec<ServerMessage> = Vec::new();

    // Join room (timed)
    let t = Instant::now();
    let join_msg = ClientMessage::JoinRoom {
        room_id: config.room_id.clone(),
        participant_name: config.participant_name.clone(),
    };
    send_message(&mut write, join_msg).await?;

    // Wait for room joined response
    let _participant_id = match receive_response(&mut read, &mut buffered_events).await? {
        ServerMessage::RoomJoined { participant_id, participants, .. } => {
            metrics.record_signaling_latency("join_room", t.elapsed().as_millis() as u64);
            tracing::info!("{}: Joined room as {}, {} other participants",
                client_id, participant_id, participants.len());
            metrics.mark_connection_successful();

            // In webinar/panel mode (publish_ratio < 1.0), viewers need to discover
            // producers that were created before they joined. In conference mode,
            // NewProducer events handle this naturally (everyone publishes, so late
            // joiners discover producers via broadcast events).
            if config.consume_existing_producers {
                let existing_producer_count: usize = participants.iter()
                    .map(|p| p.producers.len())
                    .sum();
                if existing_producer_count > 0 {
                    tracing::info!("{}: Room has {} existing producers from {} participants (consuming deferred)",
                        client_id, existing_producer_count, participants.len());
                }

                for p in &participants {
                    for producer in &p.producers {
                        existing_producer_events.push(ServerMessage::NewProducer {
                            participant_id: p.id.clone(),
                            producer_id: producer.id.clone(),
                            kind: producer.kind,
                            source: producer.source.clone(),
                        });
                    }
                }
            } else {
                let existing_producer_count: usize = participants.iter()
                    .map(|p| p.producers.len())
                    .sum();
                if existing_producer_count > 0 {
                    tracing::debug!("{}: Skipping {} existing producers (conference mode — NewProducer events will handle)",
                        client_id, existing_producer_count);
                }
            }

            participant_id
        }
        ServerMessage::Error { message } => {
            metrics.record_error(format!("Failed to join room: {}", message));
            return Err(anyhow::anyhow!("Failed to join room: {}", message));
        }
        msg => {
            return Err(anyhow::anyhow!("Unexpected message: {:?}", msg));
        }
    };

    // Get router RTP capabilities (timed)
    let t = Instant::now();
    send_message(&mut write, ClientMessage::GetRouterRtpCapabilities).await?;
    let router_caps = match receive_response(&mut read, &mut buffered_events).await? {
        ServerMessage::RouterRtpCapabilities { rtp_capabilities } => {
            metrics.record_signaling_latency("get_router_caps", t.elapsed().as_millis() as u64);
            tracing::debug!("{}: Got router RTP capabilities", client_id);
            rtp_capabilities
        }
        ServerMessage::Error { message } => {
            metrics.record_error(format!("Failed to get router caps: {}", message));
            return Err(anyhow::anyhow!("Failed to get router caps: {}", message));
        }
        msg => {
            return Err(anyhow::anyhow!("Unexpected message: {:?}", msg));
        }
    };

    // Pre-compute RtpCapabilities from RtpCapabilitiesFinalized once (avoids serde round-trip per consumer)
    let rtp_capabilities: RtpCapabilities = serde_json::from_value(
        serde_json::to_value(&router_caps).expect("Failed to serialize router caps")
    ).expect("Failed to deserialize as RtpCapabilities");

    // Create WebRTC session (metrics are passed to recv transport's on_track handler)
    let webrtc_session = WebRtcSession::new(client_id.clone(), metrics.clone());
    let webrtc_session = Arc::new(Mutex::new(webrtc_session));

    // Publishers create send transport; viewers skip it
    let mut send_transport_id: Option<String> = None;
    if config.is_publisher {
        // Create send transport (timed)
        let t = Instant::now();
        send_message(&mut write, ClientMessage::CreateSendTransport).await?;
        let (st_id, send_ice_params, send_ice_cands, send_dtls_params) = match receive_response(&mut read, &mut buffered_events).await? {
            ServerMessage::TransportCreated { transport_id, ice_parameters, ice_candidates, dtls_parameters, .. } => {
                metrics.record_signaling_latency("create_send_transport", t.elapsed().as_millis() as u64);
                tracing::debug!("{}: Send transport created: {}", client_id, transport_id);
                (transport_id, ice_parameters, ice_candidates, dtls_parameters)
            }
            ServerMessage::Error { message } => {
                metrics.record_error(format!("Failed to create send transport: {}", message));
                return Err(anyhow::anyhow!("Failed to create send transport: {}", message));
            }
            msg => {
                return Err(anyhow::anyhow!("Unexpected message: {:?}", msg));
            }
        };

        // Create REAL WebRTC send transport
        let local_send_dtls = webrtc_session
            .lock()
            .await
            .create_send_transport(st_id.clone(), send_ice_params, send_ice_cands, send_dtls_params)
            .await?;

        // Connect send transport with REAL DTLS parameters (timed)
        let t = Instant::now();
        let connect_send_msg = ClientMessage::ConnectTransport {
            transport_id: st_id.clone(),
            dtls_parameters: local_send_dtls,
        };
        send_message(&mut write, connect_send_msg).await?;
        match receive_response(&mut read, &mut buffered_events).await? {
            ServerMessage::TransportConnected { .. } => {
                metrics.record_signaling_latency("connect_send_transport", t.elapsed().as_millis() as u64);
                tracing::info!("{}: Send transport connected (REAL ICE/DTLS!)", client_id);
            }
            ServerMessage::Error { message } => {
                metrics.record_error(format!("Send transport connect failed: {}", message));
                return Err(anyhow::anyhow!("Send transport connect failed: {}", message));
            }
            _ => {}
        }

        send_transport_id = Some(st_id);
    }

    // Create receive transport (timed) — all clients need this
    let t = Instant::now();
    send_message(&mut write, ClientMessage::CreateRecvTransport).await?;
    let (recv_transport_id, recv_ice_params, recv_ice_cands, recv_dtls_params) = match receive_response(&mut read, &mut buffered_events).await? {
        ServerMessage::TransportCreated { transport_id, ice_parameters, ice_candidates, dtls_parameters, .. } => {
            metrics.record_signaling_latency("create_recv_transport", t.elapsed().as_millis() as u64);
            tracing::debug!("{}: Receive transport created: {}", client_id, transport_id);
            (transport_id, ice_parameters, ice_candidates, dtls_parameters)
        }
        ServerMessage::Error { message } => {
            metrics.record_error(format!("Failed to create recv transport: {}", message));
            return Err(anyhow::anyhow!("Failed to create recv transport: {}", message));
        }
        msg => {
            return Err(anyhow::anyhow!("Unexpected message: {:?}", msg));
        }
    };

    // Create REAL WebRTC receive transport
    let local_recv_dtls = webrtc_session
        .lock()
        .await
        .create_recv_transport(recv_transport_id.clone(), recv_ice_params, recv_ice_cands, recv_dtls_params)
        .await?;

    // Connect receive transport with REAL DTLS parameters (timed)
    let t = Instant::now();
    let connect_recv_msg = ClientMessage::ConnectTransport {
        transport_id: recv_transport_id.clone(),
        dtls_parameters: local_recv_dtls,
    };
    send_message(&mut write, connect_recv_msg).await?;
    match receive_response(&mut read, &mut buffered_events).await? {
        ServerMessage::TransportConnected { .. } => {
            metrics.record_signaling_latency("connect_recv_transport", t.elapsed().as_millis() as u64);
            tracing::info!("{}: Receive transport connected (REAL ICE/DTLS!)", client_id);
        }
        ServerMessage::Error { message } => {
            metrics.record_error(format!("Receive transport connect failed: {}", message));
            return Err(anyhow::anyhow!("Receive transport connect failed: {}", message));
        }
        _ => {}
    }

    // Publishers: produce media
    if config.is_publisher {
        let st_id = send_transport_id.as_ref().unwrap();

        // Get actual SSRCs from webrtc-rs (tracks were bound during send transport SDP negotiation)
        let (audio_ssrc, video_ssrc) = webrtc_session.lock().await.send_ssrcs().await?;
        tracing::info!("{}: Send transport SSRCs - audio: {}, video: {}", client_id, audio_ssrc, video_ssrc);

        // Produce audio if enabled (timed)
        if config.media_config.audio_enabled {
            let t = Instant::now();
            let audio_params = extract_rtp_parameters(MediaKind::Audio, audio_ssrc, config.media_config.video_bitrate_kbps);
            let produce_msg = ClientMessage::Produce {
                transport_id: st_id.clone(),
                kind: MediaKind::Audio,
                rtp_parameters: audio_params,
            };
            send_message(&mut write, produce_msg).await?;
            match receive_response(&mut read, &mut buffered_events).await? {
                ServerMessage::ProducerCreated { producer_id } => {
                    metrics.record_signaling_latency("produce_audio", t.elapsed().as_millis() as u64);
                    tracing::info!("{}: Audio producer created: {}", client_id, producer_id);
                    metrics.record_producer_created();
                }
                ServerMessage::Error { message } => {
                    metrics.record_error(format!("Audio producer failed: {}", message));
                }
                _ => {}
            }
        }

        // Produce video if enabled (timed)
        if config.media_config.video_enabled {
            let t = Instant::now();
            let video_params = extract_rtp_parameters(MediaKind::Video, video_ssrc, config.media_config.video_bitrate_kbps);
            let produce_msg = ClientMessage::Produce {
                transport_id: st_id.clone(),
                kind: MediaKind::Video,
                rtp_parameters: video_params,
            };
            send_message(&mut write, produce_msg).await?;
            match receive_response(&mut read, &mut buffered_events).await? {
                ServerMessage::ProducerCreated { producer_id } => {
                    metrics.record_signaling_latency("produce_video", t.elapsed().as_millis() as u64);
                    tracing::info!("{}: Video producer created: {}", client_id, producer_id);
                    metrics.record_producer_created();
                }
                ServerMessage::Error { message } => {
                    metrics.record_error(format!("Video producer failed: {}", message));
                }
                _ => {}
            }
        }
    }

    // Replay buffered events from setup phase (e.g., NewProducer from other clients).
    // This sends Consume requests for producers from other clients.
    let mut needs_renegotiation = false;
    let mut pending_resumes: Vec<String> = Vec::new();
    let mut audio_consumes_sent: usize = 0;
    let mut video_consumes_sent: usize = 0;

    // Count expected consumers — capped at max consumers since we limit Consume requests
    let expected_consumers = {
        let mut audio_count = 0usize;
        let mut video_count = 0usize;
        for e in &buffered_events {
            if let ServerMessage::NewProducer { kind, .. } = e {
                match kind {
                    MediaKind::Audio => {
                        if audio_count < config.max_audio_consumers { audio_count += 1; }
                    }
                    MediaKind::Video => {
                        if video_count < config.max_video_consumers { video_count += 1; }
                    }
                }
            }
        }
        audio_count + video_count
    };

    if !buffered_events.is_empty() {
        tracing::info!("{}: Replaying {} buffered events, expecting {} consumers",
            client_id, buffered_events.len(), expected_consumers);
        for event in buffered_events {
            handle_server_message(
                event,
                &metrics,
                &client_id,
                &mut write,
                &rtp_capabilities,
                &webrtc_session,
                &mut needs_renegotiation,
                &mut pending_resumes,
                &mut audio_consumes_sent,
                &mut video_consumes_sent,
                config.max_audio_consumers,
                config.max_video_consumers,
            ).await;
        }
    }

    // Wait for ConsumerCreated responses and batch into a single SDP renegotiation.
    // Cap at 5s to avoid blocking setup. Any remaining consumers are handled by
    // receive_messages_loop which has its own batching.
    if expected_consumers > 0 {
        let overall_timeout = Duration::from_millis(
            std::cmp::min(5000, std::cmp::max(1000, (expected_consumers as u64) * 10))
        );
        let consumer_deadline = tokio::time::Instant::now() + overall_timeout;
        let mut received_consumers = pending_resumes.len();

        tracing::debug!("{}: Waiting for {} consumers (have {}), timeout {}ms",
            client_id, expected_consumers, received_consumers, overall_timeout.as_millis());

        while tokio::time::Instant::now() < consumer_deadline && received_consumers < expected_consumers {
            tokio::select! {
                msg = read.next() => {
                    match msg {
                        Some(Ok(Message::Text(text))) => {
                            if let Ok(server_msg) = serde_json::from_str::<ServerMessage>(&text) {
                                let was_consumer = matches!(server_msg, ServerMessage::ConsumerCreated { .. });
                                handle_server_message(
                                    server_msg,
                                    &metrics,
                                    &client_id,
                                    &mut write,
                                    &rtp_capabilities,
                                    &webrtc_session,
                                    &mut needs_renegotiation,
                                    &mut pending_resumes,
                                    &mut audio_consumes_sent,
                                    &mut video_consumes_sent,
                                    config.max_audio_consumers,
                                    config.max_video_consumers,
                                ).await;
                                if was_consumer {
                                    received_consumers += 1;
                                }
                            }
                        }
                        _ => {}
                    }
                }
                _ = tokio::time::sleep(Duration::from_millis(200)) => {
                    // 200ms of silence — consumers have stopped arriving
                    tracing::debug!("{}: Consumer collection idle timeout ({}/{})",
                        client_id, received_consumers, expected_consumers);
                    break;
                }
            }
        }
        tracing::info!("{}: Collected {}/{} consumers", client_id, received_consumers, expected_consumers);
    }

    // Renegotiate SDP once for ALL consumers recorded during setup.
    if needs_renegotiation {
        if let Err(e) = webrtc_session.lock().await.renegotiate_consumers().await {
            tracing::error!("{}: Failed to renegotiate consumers: {}", client_id, e);
        }

        // CRITICAL: webrtc-rs's set_remote_description ENQUEUES the start_rtp operation
        // asynchronously — it does NOT register SSRCs before returning.
        tokio::time::sleep(Duration::from_millis(100)).await;
        tracing::debug!("{}: Yielded for SSRC registration to complete", client_id);
    }

    // NOW resume consumers — renegotiation's queued operation has had time to register SSRCs.
    let resume_count = pending_resumes.len();
    for consumer_id in pending_resumes.drain(..) {
        let resume_msg = ClientMessage::ResumeConsumer {
            consumer_id: consumer_id.clone(),
        };
        let json = serde_json::to_string(&resume_msg)?;
        write.feed(Message::Text(json.into())).await?;
    }
    if resume_count > 0 {
        write.flush().await?;
        tracing::info!("{}: Resumed {} consumers after initial SDP renegotiation", client_id, resume_count);
    }

    tracing::info!("{}: Setup complete, starting media session for {}s", client_id, config.session_duration.as_secs());

    // Start media sending task (publishers only)
    let media_task = if config.is_publisher {
        let media_gen = MediaGenerator::new(config.media_config.clone());
        let metrics_send = metrics.clone();
        let media_config = config.media_config.clone();
        let client_id_send = client_id.clone();
        let webrtc_session_send = Arc::clone(&webrtc_session);
        Some(tokio::spawn(async move {
            send_real_media_loop(webrtc_session_send, media_gen, media_config, metrics_send, client_id_send).await;
        }))
    } else {
        None
    };

    let metrics_recv = metrics.clone();
    let client_id_recv = client_id.clone();
    // Receive loop gets extra time so the abort (not timeout) controls shutdown
    let recv_timeout = config.session_duration + Duration::from_secs(5);

    // Clone caps and webrtc_session for consumer creation
    let rtp_caps_for_consume = rtp_capabilities.clone();
    let webrtc_session_recv = Arc::clone(&webrtc_session);

    let max_audio = config.max_audio_consumers;
    let max_video = config.max_video_consumers;
    let receive_task = tokio::spawn(async move {
        receive_messages_loop(
            read,
            write,
            metrics_recv,
            client_id_recv,
            recv_timeout,
            rtp_caps_for_consume,
            webrtc_session_recv,
            audio_consumes_sent,
            video_consumes_sent,
            max_audio,
            max_video,
            existing_producer_events,
        ).await;
    });

    // Session timer starts AFTER setup — late-joining clients get full media time
    sleep(config.session_duration).await;

    // Clean shutdown — explicitly close PeerConnections to avoid slow async drop.
    tracing::info!("{}: Session duration completed, shutting down", client_id);
    if let Some(task) = media_task {
        task.abort();
    }
    receive_task.abort();

    // Close PeerConnections synchronously before returning
    if let Err(e) = webrtc_session.lock().await.close().await {
        tracing::debug!("{}: Close error (non-fatal): {}", client_id, e);
    }

    Ok(())
}

async fn send_real_media_loop(
    webrtc_session: Arc<Mutex<WebRtcSession>>,
    mut media_gen: MediaGenerator,
    config: MediaConfig,
    metrics: Arc<MetricsCollector>,
    client_id: String,
) {
    tracing::debug!("{}: Starting REAL media send loop", client_id);

    // Get tracks from WebRTC session
    let (audio_track, video_track) = {
        let session = webrtc_session.lock().await;
        (session.audio_track(), session.video_track())
    };

    let mut audio_interval = tokio::time::interval(media_gen.audio_packet_interval());
    let mut video_interval = tokio::time::interval(media_gen.video_packet_interval());

    audio_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    video_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    let mut first_packet = true;

    loop {
        tokio::select! {
            _ = audio_interval.tick(), if config.audio_enabled => {
                if let Some(track) = &audio_track {
                    let packet_bytes = media_gen.generate_audio_packet();

                    match track.write(&packet_bytes).await {
                        Ok(_) => {
                            metrics.record_packet_sent(packet_bytes.len());
                            if first_packet {
                                metrics.mark_first_media_sent();
                                first_packet = false;
                                tracing::info!("{}: First REAL RTP packet sent!", client_id);
                            }
                        }
                        Err(e) => {
                            tracing::error!("{}: Failed to send RTP: {}", client_id, e);
                            break;
                        }
                    }
                }
            }
            _ = video_interval.tick(), if config.video_enabled => {
                if let Some(track) = &video_track {
                    let frame_packets = media_gen.generate_video_frame();
                    let mut send_error = false;
                    for packet_bytes in frame_packets {
                        match track.write(&packet_bytes).await {
                            Ok(_) => {
                                metrics.record_packet_sent(packet_bytes.len());
                                if first_packet {
                                    metrics.mark_first_media_sent();
                                    first_packet = false;
                                    tracing::info!("{}: First REAL RTP packet sent!", client_id);
                                }
                            }
                            Err(e) => {
                                tracing::error!("{}: Failed to send RTP: {}", client_id, e);
                                send_error = true;
                                break;
                            }
                        }
                    }
                    if send_error { break; }
                }
            }
        }
    }
}

async fn receive_messages_loop(
    mut read: futures_util::stream::SplitStream<tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>>,
    mut write: futures_util::stream::SplitSink<tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>, Message>,
    metrics: Arc<MetricsCollector>,
    client_id: String,
    timeout: Duration,
    rtp_capabilities: RtpCapabilities,
    webrtc_session: Arc<Mutex<WebRtcSession>>,
    mut audio_consumes_sent: usize,
    mut video_consumes_sent: usize,
    max_audio: usize,
    max_video: usize,
    existing_producer_events: Vec<ServerMessage>,
) {
    let deadline = tokio::time::Instant::now() + timeout;
    let mut needs_renegotiation = false;
    let mut pending_resumes: Vec<String> = Vec::new();
    let mut renegotiation_since: Option<tokio::time::Instant> = None;
    let mut total_resumed: usize = 0;

    // Process existing producers with throttling — send one Consume request per
    // iteration of the main loop, interleaved with real-time events. This avoids
    // the burst that kills ratio when many producers already exist.
    let mut deferred_events: std::collections::VecDeque<ServerMessage> = existing_producer_events.into();
    let mut deferred_batch_size: usize = 0;

    while tokio::time::Instant::now() < deadline {
        let got_message = tokio::select! {
            msg = read.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        match serde_json::from_str::<ServerMessage>(&text) {
                            Ok(server_msg) => {
                                handle_server_message(
                                    server_msg,
                                    &metrics,
                                    &client_id,
                                    &mut write,
                                    &rtp_capabilities,
                                    &webrtc_session,
                                    &mut needs_renegotiation,
                                    &mut pending_resumes,
                                    &mut audio_consumes_sent,
                                    &mut video_consumes_sent,
                                    max_audio,
                                    max_video,
                                ).await;
                            }
                            Err(e) => {
                                tracing::warn!("{}: Failed to parse message: {}", client_id, e);
                            }
                        }
                        true
                    }
                    Some(Ok(Message::Binary(data))) => {
                        metrics.record_packet_received(data.len());
                        true
                    }
                    Some(Err(e)) => {
                        tracing::error!("{}: WebSocket error: {}", client_id, e);
                        metrics.record_error(format!("WebSocket error: {}", e));
                        break;
                    }
                    None => {
                        tracing::info!("{}: WebSocket closed", client_id);
                        break;
                    }
                    Some(Ok(Message::Close(_))) => {
                        tracing::info!("{}: Server sent close frame", client_id);
                        break;
                    }
                    _ => { true } // Ping/Pong handled by library
                }
            }
            _ = tokio::time::sleep(Duration::from_millis(2000)) => {
                false
            }
        };

        // Process deferred existing-producer events gradually (2 per loop iteration).
        // This interleaves with real-time events and avoids the burst that kills
        // server throughput when hundreds of producers already exist.
        if !deferred_events.is_empty() {
            let batch = std::cmp::min(2, deferred_events.len());
            for _ in 0..batch {
                if let Some(event) = deferred_events.pop_front() {
                    handle_server_message(
                        event,
                        &metrics,
                        &client_id,
                        &mut write,
                        &rtp_capabilities,
                        &webrtc_session,
                        &mut needs_renegotiation,
                        &mut pending_resumes,
                        &mut audio_consumes_sent,
                        &mut video_consumes_sent,
                        max_audio,
                        max_video,
                    ).await;
                    deferred_batch_size += 1;
                }
            }
            if deferred_events.is_empty() && deferred_batch_size > 0 {
                tracing::debug!("{}: Finished processing {} deferred existing-producer events",
                    client_id, deferred_batch_size);
            }
        }

        // Track when renegotiation was first needed (for max timer)
        if needs_renegotiation && renegotiation_since.is_none() {
            renegotiation_since = Some(tokio::time::Instant::now());
        }

        // Max timer: how long to wait before forcing renegotiation even while messages
        // are still arriving. Uses step function based on total consumers resumed.
        let max_timer_ms: u64 = if total_resumed < 100 {
            2000
        } else if total_resumed < 500 {
            10000
        } else {
            30000
        };

        let max_timer_expired = renegotiation_since
            .map(|t| t.elapsed() > Duration::from_millis(max_timer_ms))
            .unwrap_or(false);

        if needs_renegotiation && (!got_message || max_timer_expired) {
            // Single renegotiation for ALL consumers collected in this window
            if let Err(e) = webrtc_session.lock().await.renegotiate_consumers().await {
                tracing::error!("{}: Failed to renegotiate consumers: {}", client_id, e);
            }

            let batch_size = pending_resumes.len();

            // Adaptive SSRC wait
            let ssrc_wait_ms = std::cmp::min(200, std::cmp::max(50, batch_size as u64 * 5));
            let ssrc_deadline = tokio::time::Instant::now() + Duration::from_millis(ssrc_wait_ms);
            let mut ws_dead = false;
            loop {
                tokio::select! {
                    msg = read.next() => {
                        match msg {
                            Some(Ok(Message::Text(text))) => {
                                if let Ok(server_msg) = serde_json::from_str::<ServerMessage>(&text) {
                                    handle_server_message(
                                        server_msg,
                                        &metrics,
                                        &client_id,
                                        &mut write,
                                        &rtp_capabilities,
                                        &webrtc_session,
                                        &mut needs_renegotiation,
                                        &mut pending_resumes,
                                        &mut audio_consumes_sent,
                                        &mut video_consumes_sent,
                                        max_audio,
                                        max_video,
                                        ).await;
                                }
                            }
                            Some(Ok(Message::Binary(data))) => {
                                metrics.record_packet_received(data.len());
                            }
                            Some(Err(e)) => {
                                tracing::error!("{}: WebSocket error during SSRC wait: {}", client_id, e);
                                metrics.record_error(format!("WebSocket error: {}", e));
                                ws_dead = true;
                                break;
                            }
                            None => {
                                tracing::info!("{}: WebSocket closed during SSRC wait", client_id);
                                ws_dead = true;
                                break;
                            }
                            _ => {} // Ping/Pong — ignore, don't break
                        }
                    }
                    _ = tokio::time::sleep_until(ssrc_deadline) => break,
                }
            }
            if ws_dead { break; }

            // Only resume consumers from the CURRENT batch
            let to_resume: Vec<String> = pending_resumes.drain(..batch_size).collect();
            let resume_count = to_resume.len();
            for consumer_id in to_resume {
                let resume_msg = ClientMessage::ResumeConsumer {
                    consumer_id: consumer_id.clone(),
                };
                let json = serde_json::to_string(&resume_msg).unwrap();
                if let Err(e) = write.feed(Message::Text(json.into())).await {
                    tracing::error!("{}: Failed to feed resume for {}: {}", client_id, consumer_id, e);
                }
            }
            if resume_count > 0 {
                if let Err(e) = write.flush().await {
                    tracing::error!("{}: Failed to flush resumes: {}", client_id, e);
                }
                total_resumed += resume_count;
                tracing::debug!("{}: Renegotiated + resumed {} consumers (total: {}, timer: {}ms)",
                    client_id, resume_count, total_resumed, max_timer_ms);
            }

            if pending_resumes.is_empty() {
                needs_renegotiation = false;
                renegotiation_since = None;
            } else {
                renegotiation_since = Some(tokio::time::Instant::now());
            }
        }
    }
}

// Smart subscription caps: audio is cheap (~10KB/consumer), video is expensive (SDP + read loop).
// Configurable via TestConfig, but use these as defaults.
const DEFAULT_MAX_AUDIO_CONSUMERS: usize = 4;
const DEFAULT_MAX_VIDEO_CONSUMERS: usize = 4;

async fn handle_server_message(
    msg: ServerMessage,
    metrics: &Arc<MetricsCollector>,
    client_id: &str,
    write: &mut futures_util::stream::SplitSink<
        tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
        Message,
    >,
    rtp_capabilities: &RtpCapabilities,
    webrtc_session: &Arc<Mutex<WebRtcSession>>,
    needs_renegotiation: &mut bool,
    pending_resumes: &mut Vec<String>,
    audio_consumes_sent: &mut usize,
    video_consumes_sent: &mut usize,
    max_audio: usize,
    max_video: usize,
) {
    match msg {
        ServerMessage::NewProducer { participant_id: _, producer_id, kind, .. } => {
            // Smart subscription: separate caps for audio and video
            let at_cap = match kind {
                MediaKind::Audio => *audio_consumes_sent >= max_audio,
                MediaKind::Video => *video_consumes_sent >= max_video,
            };

            if at_cap {
                return;
            }

            tracing::debug!("{}: New producer available: {} ({:?}), creating consumer...",
                client_id, producer_id, kind);

            let consume_msg = ClientMessage::Consume {
                producer_id: producer_id.clone(),
                rtp_capabilities: rtp_capabilities.clone(),
            };

            if let Err(e) = send_message(write, consume_msg).await {
                tracing::error!("{}: Failed to send Consume message: {}", client_id, e);
                return;
            }

            match kind {
                MediaKind::Audio => *audio_consumes_sent += 1,
                MediaKind::Video => *video_consumes_sent += 1,
            }

            let total = *audio_consumes_sent + *video_consumes_sent;
            tracing::debug!("{}: Sent Consume request (audio:{}/{}, video:{}/{}, total:{}) for producer {}",
                client_id, audio_consumes_sent, max_audio,
                video_consumes_sent, max_video, total, producer_id);
        }
        ServerMessage::ConsumerCreated { consumer_id, producer_id, kind, rtp_parameters } => {
            metrics.record_consumer_created();

            // Record consumer info WITHOUT renegotiating SDP yet.
            if let Err(e) = webrtc_session.lock().await.record_consumer(producer_id.clone(), kind, &rtp_parameters) {
                tracing::error!("{}: Failed to record consumer: {}", client_id, e);
                return;
            }

            pending_resumes.push(consumer_id.clone());
            *needs_renegotiation = true;
        }
        ServerMessage::ParticipantJoined { participant_id, participant_name } => {
            tracing::debug!("{}: Participant joined: {} ({})",
                client_id, participant_name, participant_id);
        }
        ServerMessage::ParticipantLeft { participant_id } => {
            tracing::debug!("{}: Participant left: {}", client_id, participant_id);
        }
        ServerMessage::ProducerClosed { producer_id } => {
            tracing::debug!("{}: Producer closed: {}", client_id, producer_id);
        }
        ServerMessage::ProducerPaused { producer_id } => {
            tracing::debug!("{}: Producer paused: {}", client_id, producer_id);
        }
        ServerMessage::ProducerResumed { producer_id } => {
            tracing::debug!("{}: Producer resumed: {}", client_id, producer_id);
        }
        ServerMessage::ConsumerResumed { consumer_id } => {
            tracing::debug!("{}: Consumer resumed: {}", client_id, consumer_id);
        }
        ServerMessage::ConsumerPaused { consumer_id } => {
            tracing::debug!("{}: Consumer paused: {}", client_id, consumer_id);
        }
        ServerMessage::Error { message } => {
            tracing::warn!("{}: Server error: {}", client_id, message);
            metrics.record_error(format!("Server error: {}", message));
        }
        _ => {}
    }
}

async fn send_message(
    write: &mut futures_util::stream::SplitSink<
        tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
        Message,
    >,
    msg: ClientMessage,
) -> Result<()> {
    let json = serde_json::to_string(&msg)?;
    write.send(Message::Text(json.into())).await?;
    Ok(())
}

async fn receive_message(
    read: &mut futures_util::stream::SplitStream<
        tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    >,
) -> Result<ServerMessage> {
    match read.next().await {
        Some(Ok(Message::Text(text))) => {
            let msg = serde_json::from_str(&text)?;
            Ok(msg)
        }
        Some(Ok(msg)) => Err(anyhow::anyhow!("Unexpected message type: {:?}", msg)),
        Some(Err(e)) => Err(e.into()),
        None => Err(anyhow::anyhow!("WebSocket closed")),
    }
}

/// Receive next response message, buffering async event notifications.
async fn receive_response(
    read: &mut futures_util::stream::SplitStream<
        tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    >,
    buffered_events: &mut Vec<ServerMessage>,
) -> Result<ServerMessage> {
    loop {
        match receive_message(read).await? {
            msg @ ServerMessage::ParticipantJoined { .. } => {
                tracing::debug!("Setup: buffering ParticipantJoined event");
                buffered_events.push(msg);
            }
            msg @ ServerMessage::ParticipantLeft { .. } => {
                tracing::debug!("Setup: buffering ParticipantLeft event");
                buffered_events.push(msg);
            }
            msg @ ServerMessage::NewProducer { .. } => {
                tracing::debug!("Setup: buffering NewProducer event");
                buffered_events.push(msg);
            }
            msg @ ServerMessage::ProducerClosed { .. } => {
                tracing::debug!("Setup: buffering ProducerClosed event");
                buffered_events.push(msg);
            }
            // Ignore server-pushed informational messages during setup
            ServerMessage::ConnectionStats { .. } => {}
            ServerMessage::ConsumerLayersChanged { .. } => {}
            ServerMessage::IceRestarted { .. } => {}
            ServerMessage::ReconnectResult { .. } => {}
            ServerMessage::ConsumerResumed { .. } => {}
            ServerMessage::ConsumerPaused { .. } => {}
            ServerMessage::ProducerPaused { .. } => {}
            ServerMessage::ProducerResumed { .. } => {}
            ServerMessage::ChatReceived { .. } => {}
            msg => return Ok(msg),
        }
    }
}

fn extract_rtp_parameters(kind: MediaKind, ssrc: u32, video_bitrate_kbps: u32) -> RtpParameters {
    // Payload types must match the router's codec config (see config.rs)
    match kind {
        MediaKind::Audio => RtpParameters {
            mid: None,
            codecs: vec![RtpCodecParameters::Audio {
                mime_type: MimeTypeAudio::Opus,
                payload_type: 111,
                clock_rate: NonZeroU32::new(48000).unwrap(),
                channels: NonZeroU8::new(2).unwrap(),
                parameters: RtpCodecParametersParameters::default(),
                rtcp_feedback: vec![],
            }],
            header_extensions: vec![
                RtpHeaderExtensionParameters {
                    uri: RtpHeaderExtensionUri::Mid,
                    id: 1,
                    encrypt: false,
                },
            ],
            encodings: vec![RtpEncodingParameters {
                ssrc: Some(ssrc),
                ..Default::default()
            }],
            rtcp: RtcpParameters::default(),
        },
        MediaKind::Video => RtpParameters {
            mid: None,
            codecs: vec![RtpCodecParameters::Video {
                mime_type: MimeTypeVideo::Vp8,
                payload_type: 96,
                clock_rate: NonZeroU32::new(90000).unwrap(),
                parameters: RtpCodecParametersParameters::default(),
                rtcp_feedback: vec![],
            }],
            header_extensions: vec![
                RtpHeaderExtensionParameters {
                    uri: RtpHeaderExtensionUri::Mid,
                    id: 1,
                    encrypt: false,
                },
            ],
            encodings: vec![RtpEncodingParameters {
                ssrc: Some(ssrc),
                max_bitrate: Some(video_bitrate_kbps * 1000),
                ..Default::default()
            }],
            rtcp: RtcpParameters::default(),
        },
    }
}

fn print_usage() {
    println!("Load Test for SimplestChat");
    println!("\nUsage:");
    println!("  cargo run --bin load_test [OPTIONS]");
    println!("\nOptions:");
    println!("  -c, --clients <N>          Number of concurrent clients (default: 5)");
    println!("  -d, --duration <SECS>      Test duration in seconds (default: 30)");
    println!("  -r, --ramp-up <SECS>       Ramp-up period in seconds (default: 5)");
    println!("  -s, --server <URL>         Server WebSocket URL (default: ws://localhost:3000/ws)");
    println!("  --room <ID>                Room ID to join (default: load-test-room)");
    println!("  --rooms <N>                Number of rooms to distribute clients across (default: 1)");
    println!("  --publish-ratio <0.0-1.0>  Fraction of clients that publish media (default: 1.0)");
    println!("  --mode <MODE>              Preset publish ratios: webinar (1%), panel (10%),");
    println!("                             classroom (20%), conference (100%)");
    println!("  --churn-rate <N>           Clients churning (disconnect/reconnect) per second (default: 0)");
    println!("  --audio-only               Send only audio (no video)");
    println!("  --video-only               Send only video (no audio)");
    println!("  -q, --quality <PRESET>     Video quality: 480p (default), 720p, 1080p");
    println!("                             Sets resolution and bitrate to realistic values");
    println!("  --fps <15|30|60>           Frames per second (default: 30)");
    println!("                             Bitrate scales: 15fps=0.6x, 30fps=1x, 60fps=1.5x");
    println!("  -h, --help                 Print this help message");
    println!("\nExamples:");
    println!("  # Basic load test");
    println!("  cargo run --bin load_test -- --clients 10 --duration 60");
    println!("");
    println!("  # Multi-room: 1000 clients across 16 rooms (one per worker)");
    println!("  cargo run --bin load_test -- --clients 1000 --rooms 16 --duration 30");
    println!("");
    println!("  # Webinar: 10 presenters, 990 viewers");
    println!("  cargo run --bin load_test -- --clients 1000 --mode webinar --duration 60");
    println!("");
    println!("  # Churn test: 5 clients reconnecting per second");
    println!("  cargo run --bin load_test -- --clients 100 --churn-rate 5 --duration 60");
    println!("");
    println!("  # Panel discussions: 10% publish, 16 rooms");
    println!("  cargo run --bin load_test -- --clients 1000 --rooms 16 --mode panel");
    println!("\nEnvironment Variables:");
    println!("  RUST_LOG=debug          Enable debug logging");
    println!("  RUST_LOG=info           Enable info logging (default)");
}
