#![forbid(unsafe_code)]

//! Load test binary - Spawn multiple synthetic clients to test server scalability
//!
//! Usage:
//!   cargo run --features load-test --bin load_test -- --clients 10 --duration 30
//!   cargo run --features load-test --bin load_test -- --clients 1000 --rooms 16 --duration 30
//!   cargo run --features load-test --bin load_test -- --clients 1000 --mode webinar --duration 60
//!   cargo run --features load-test --bin load_test -- --clients 100 --churn-rate 5 --duration 60

use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use mediasoup::prelude::*;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::time::sleep;
use tokio_tungstenite::{connect_async, tungstenite::Message};

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

use media_generator::{MediaConfig, MediaGenerator};
use metrics::{MeasurementWindow, MetricsCollector, TestSummary};
use rtc::shared::marshal::Unmarshal;
use std::num::{NonZeroU8, NonZeroU32};
use tokio::sync::Mutex;
use webrtc::media_stream::track_local::TrackLocal;
use webrtc_client::WebRtcSession;

#[derive(Debug, Clone)]
struct ClientConfig {
    server_url: String,
    room_id: String,
    participant_name: String,
    media_config: MediaConfig,
    session_duration: Duration,
    measurement_start: Instant,
    deadline: Instant,
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

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
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
    warmup_secs: u64,
    deadline_grace_secs: u64,
    output_dir: std::path::PathBuf,
    run_label: String,
    server_revision: String,
    generator_revision: String,
    diagnostics: bool,
}

const WATCHDOG_RUNNING: u8 = 0;
const WATCHDOG_FINISHED: u8 = 1;
const WATCHDOG_EXPIRED: u8 = 2;

/// The deadline thread never takes a collector lock or performs logging/file IO.
/// Reporting gets a separate thread and a bounded grace before process exit.
struct RunWatchdog {
    deadline: Instant,
    state: Arc<std::sync::atomic::AtomicU8>,
    stop: std::sync::mpsc::Sender<()>,
}

impl RunWatchdog {
    fn start(deadline: Instant, report: impl FnOnce() + Send + 'static) -> Self {
        let state = Arc::new(std::sync::atomic::AtomicU8::new(WATCHDOG_RUNNING));
        let (stop, receiver) = std::sync::mpsc::channel();
        let worker_state = state.clone();
        std::thread::spawn(move || {
            watchdog_wait(
                deadline,
                Duration::from_secs(2),
                worker_state,
                receiver,
                report,
                |code| std::process::exit(code),
            );
        });
        Self {
            deadline,
            state,
            stop,
        }
    }

    /// Call only after all normal report IO succeeds. Once expired, a late
    /// normal completion must not turn the process exit back into success.
    fn finish(&self) -> bool {
        use std::sync::atomic::Ordering;
        if Instant::now() >= self.deadline
            || self
                .state
                .compare_exchange(
                    WATCHDOG_RUNNING,
                    WATCHDOG_FINISHED,
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                )
                .is_err()
        {
            return false;
        }
        if Instant::now() >= self.deadline {
            self.state.store(WATCHDOG_EXPIRED, Ordering::SeqCst);
            return false;
        }
        let _ = self.stop.send(());
        true
    }
}

fn watchdog_wait(
    deadline: Instant,
    report_grace: Duration,
    state: Arc<std::sync::atomic::AtomicU8>,
    stop: std::sync::mpsc::Receiver<()>,
    report: impl FnOnce() + Send + 'static,
    exit: impl FnOnce(i32),
) {
    use std::sync::atomic::Ordering;
    match stop.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
        Ok(()) => return,
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
        // An early return/panic is already unsuccessful, but runtime teardown
        // must still be bounded. Do not mislabel it as deadline expiry early.
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
        }
    }
    if state
        .compare_exchange(
            WATCHDOG_RUNNING,
            WATCHDOG_EXPIRED,
            Ordering::SeqCst,
            Ordering::SeqCst,
        )
        .is_err()
    {
        return;
    }
    // A held collector mutex, blocked filesystem, or full stderr/stdout pipe
    // cannot prevent the independent deadline thread from reaching exit(124).
    let _ = std::thread::Builder::new()
        .name("load-test-timeout-report".into())
        .spawn(move || {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(report));
        });
    std::thread::sleep(report_grace);
    exit(124);
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
            warmup_secs: 10,
            deadline_grace_secs: 30,
            output_dir: ".".into(),
            run_label: String::new(),
            server_revision: "unknown".to_string(),
            generator_revision: "unknown".to_string(),
            diagnostics: false,
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    // Parse command line arguments
    let args: Vec<String> = std::env::args().collect();
    let mut config = TestConfig::default();
    let mut quality_override: Option<String> = None;
    let mut fps_override: Option<u8> = None;

    let mut i = 1;
    while i < args.len() {
        validate_cli_value(&args, i)?;
        match args[i].as_str() {
            "--diagnostics" => {
                config.diagnostics = true;
                i += 1;
            }
            "--warmup"
            | "--deadline-grace"
            | "--output-dir"
            | "--run-label"
            | "--server-revision"
            | "--generator-revision" => {
                let value = args
                    .get(i + 1)
                    .ok_or_else(|| anyhow::anyhow!("Missing value for {}", args[i]))?;
                match args[i].as_str() {
                    "--warmup" => config.warmup_secs = value.parse()?,
                    "--deadline-grace" => config.deadline_grace_secs = value.parse()?,
                    "--output-dir" => config.output_dir = value.into(),
                    "--run-label" => config.run_label = value.clone(),
                    "--server-revision" => config.server_revision = value.clone(),
                    "--generator-revision" => config.generator_revision = value.clone(),
                    _ => unreachable!(),
                }
                i += 2;
            }
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
                    config.publish_ratio =
                        args[i + 1].parse::<f64>().unwrap_or(1.0).clamp(0.0, 1.0);
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
                    config.max_audio_consumers =
                        args[i + 1].parse().unwrap_or(DEFAULT_MAX_AUDIO_CONSUMERS);
                    i += 2;
                } else {
                    i += 1;
                }
            }
            "--max-video" => {
                if i + 1 < args.len() {
                    config.max_video_consumers =
                        args[i + 1].parse().unwrap_or(DEFAULT_MAX_VIDEO_CONSUMERS);
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
                anyhow::bail!("Unknown option: {} (see --help)", args[i]);
            }
        }
    }

    let audio_enabled = config.media_config.audio_enabled;
    let video_enabled = config.media_config.video_enabled;
    // Apply quality preset (after all args parsed so order doesn't matter)
    if let Some(ref quality) = quality_override {
        let fps = fps_override.unwrap_or(30);
        let fps = match fps {
            15 | 30 | 60 => fps,
            _ => {
                eprintln!("Invalid FPS '{}', using 30", fps);
                30
            }
        };
        config.media_config = MediaConfig::from_preset(quality, fps);
    } else if let Some(fps) = fps_override {
        let fps = match fps {
            15 | 30 | 60 => fps,
            _ => {
                eprintln!("Invalid FPS '{}', using 30", fps);
                30
            }
        };
        config.media_config.video_fps = fps;
        let multiplier = match fps {
            15 => 0.6,
            60 => 1.5,
            _ => 1.0,
        };
        config.media_config.video_bitrate_kbps =
            (config.media_config.video_bitrate_kbps as f64 * multiplier) as u32;
    }

    config.media_config.audio_enabled = audio_enabled;
    config.media_config.video_enabled = video_enabled;
    run_load_test(config).await
}

fn validate_cli_value(args: &[String], index: usize) -> Result<()> {
    let option = args[index].as_str();
    if matches!(
        option,
        "--audio-only" | "--video-only" | "--diagnostics" | "--help" | "-h"
    ) {
        return Ok(());
    }
    let value = args
        .get(index + 1)
        .ok_or_else(|| anyhow::anyhow!("Missing value for {option}"))?;
    anyhow::ensure!(!value.starts_with("--"), "Missing value for {option}");
    match option {
        "--clients" | "-c" | "--duration" | "-d" | "--ramp-up" | "-r" | "--rooms"
        | "--max-audio" | "--max-video" | "--warmup" | "--deadline-grace" => {
            value
                .parse::<u64>()
                .map_err(|_| anyhow::anyhow!("Invalid integer for {option}: {value}"))?;
        }
        "--publish-ratio" => {
            let ratio: f64 = value.parse()?;
            anyhow::ensure!(
                ratio.is_finite() && (0.0..=1.0).contains(&ratio),
                "--publish-ratio must be between 0 and 1"
            );
        }
        "--churn-rate" => {
            let rate: f64 = value.parse()?;
            anyhow::ensure!(
                rate.is_finite() && rate >= 0.0,
                "--churn-rate must be nonnegative and finite"
            );
        }
        "--mode" => anyhow::ensure!(
            matches!(
                value.as_str(),
                "conference" | "webinar" | "panel" | "classroom"
            ),
            "Unknown mode: {value}"
        ),
        "--quality" | "-q" => anyhow::ensure!(
            matches!(value.as_str(), "480p" | "720p" | "1080p"),
            "Unknown quality: {value}"
        ),
        "--fps" => anyhow::ensure!(
            matches!(value.as_str(), "15" | "30" | "60"),
            "FPS must be 15, 30 or 60"
        ),
        "--server" | "-s" => {
            let url = url::Url::parse(value)?;
            anyhow::ensure!(
                matches!(url.scheme(), "ws" | "wss")
                    && url.username().is_empty()
                    && url.password().is_none()
                    && url.query().is_none(),
                "Use a ws/wss URL without credentials or query parameters"
            );
        }
        "--room"
        | "--output-dir"
        | "--run-label"
        | "--server-revision"
        | "--generator-revision" => {}
        _ => anyhow::bail!("Unknown option: {option}"),
    }
    Ok(())
}

async fn run_load_test(config: TestConfig) -> Result<()> {
    anyhow::ensure!(
        config.num_clients > 0 && config.num_clients <= 10_000,
        "--clients must be between 1 and 10000"
    );
    anyhow::ensure!(
        (3..=3600).contains(&config.duration_secs),
        "--duration must be between 3 and 3600 seconds"
    );
    anyhow::ensure!(
        config.warmup_secs <= 600 && config.ramp_up_secs <= 3600,
        "Warmup/ramp are too long"
    );
    anyhow::ensure!(
        (1..=300).contains(&config.deadline_grace_secs),
        "--deadline-grace must be between 1 and 300 seconds"
    );
    anyhow::ensure!(
        config.publish_ratio.is_finite() && config.churn_rate.is_finite(),
        "Ratios must be finite"
    );
    std::fs::create_dir_all(&config.output_dir)?;
    let started_at = chrono::Utc::now().to_rfc3339();
    let provenance = generator_provenance(&config)?;
    let run_start = Instant::now();
    let measurement_start =
        run_start + Duration::from_secs(config.ramp_up_secs + config.warmup_secs);
    let window = Arc::new(MeasurementWindow::new(
        measurement_start,
        Duration::from_secs(config.duration_secs),
    ));

    // Arm before workload logging, allocation, or ramp-up: even a blocked output
    // pipe must not prevent this run's independent deadline from firing.
    let deadline_secs = config.ramp_up_secs
        + config.warmup_secs
        + config.duration_secs
        + config.deadline_grace_secs;
    let deadline_config = config.clone();
    let deadline_started_at = started_at.clone();
    let deadline_provenance = provenance.clone();
    let watchdog = RunWatchdog::start(run_start + Duration::from_secs(deadline_secs), move || {
        // Deliberately no collector access: the data itself may be locked by
        // the stalled work that caused expiry. Exit status remains authoritative
        // even if the filesystem prevents these best-effort artifacts.
        let _ = write_timeout_results(&deadline_config, &deadline_started_at, &deadline_provenance);
        eprintln!("Load test hard deadline reached after {deadline_secs}s; run is INCOMPLETE");
    });

    let num_publishers = ((config.num_clients as f64) * config.publish_ratio).ceil() as usize;
    let num_publishers = num_publishers.max(1).min(config.num_clients);
    let num_churners = if config.churn_rate > 0.0 {
        ((config.churn_rate * config.duration_secs as f64) as usize).min(config.num_clients)
    } else {
        0
    };
    let clients_per_room = std::num::NonZeroUsize::new(config.num_rooms)
        .map_or(config.num_clients, |rooms| {
            config.num_clients.div_ceil(rooms.get())
        });

    println!("\n=== Starting Load Test ===");
    println!("Clients: {}", config.num_clients);
    println!("Duration: {}s", config.duration_secs);
    println!("Ramp-up: {}s", config.ramp_up_secs);
    println!("Server: {}", config.server_url);
    println!(
        "Rooms: {} ({} clients/room avg)",
        config.num_rooms, clients_per_room
    );
    println!(
        "Publishers: {}/{} ({:.0}% publish ratio)",
        num_publishers,
        config.num_clients,
        config.publish_ratio * 100.0
    );
    if num_churners > 0 {
        println!("Churners: {} (rate: {}/s)", num_churners, config.churn_rate);
    }
    println!(
        "Media: Audio={}, Video={}",
        config.media_config.audio_enabled, config.media_config.video_enabled
    );
    println!(
        "Quality: {} ({} kbps video, {} kbps audio)",
        config.media_config.quality_label(),
        config.media_config.video_bitrate_kbps,
        config.media_config.audio_bitrate_kbps,
    );
    println!(
        "Max consumers: audio={}, video={}",
        config.max_audio_consumers, config.max_video_consumers
    );
    println!("========================\n");

    let session_duration = window.end.duration_since(run_start);
    let ramp_up_delay = if config.num_clients > 1 {
        Duration::from_secs(config.ramp_up_secs) / config.num_clients as u32
    } else {
        Duration::from_secs(0)
    };

    let mut handles = Vec::new();
    let metrics_collectors: Vec<_> = (0..config.num_clients)
        .map(|i| {
            let metrics = Arc::new(MetricsCollector::with_window(
                format!("client-{i}"),
                window.clone(),
            ));
            if config.diagnostics {
                metrics.enable_diagnostics();
            }
            metrics
        })
        .collect();

    // Churner clients are the LAST N clients
    let churner_start_idx = config.num_clients.saturating_sub(num_churners);

    // Spawn clients with gradual ramp-up
    for (i, collector) in metrics_collectors.iter().enumerate() {
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
            measurement_start,
            deadline: window.end,
            is_publisher,
            is_churner,
            churn_session_min_secs: 5,
            churn_session_max_secs: 30,
            max_audio_consumers: config.max_audio_consumers,
            max_video_consumers: config.max_video_consumers,
            consume_existing_producers: true,
        };

        let metrics = collector.clone();

        let handle = tokio::spawn(run_client(client_config, client_id, metrics));
        handles.push(handle);

        if i < config.num_clients - 1 {
            tracing::info!(
                "Waiting {}ms before spawning next client...",
                ramp_up_delay.as_millis()
            );
            sleep(ramp_up_delay).await;
        }
    }

    println!(
        "All clients spawned. Shared window: {}s warmup, {}s measurement.\n",
        config.warmup_secs, config.duration_secs
    );
    let mut failures = Vec::new();
    for (index, handle) in handles.into_iter().enumerate() {
        if let Err(error) = handle.await {
            metrics_collectors[index].record_error(format!("Client task failed: {error}"));
            metrics_collectors[index].end_session();
            failures.push(format!("client-{index}: task failed: {error}"));
        }
    }

    // If we get here, the runtime isn't stuck — generate results normally
    println!("All clients completed.");
    let passed = write_results_sync(
        &metrics_collectors,
        &config,
        &started_at,
        &provenance,
        true,
        failures,
    )?;
    if !watchdog.finish() {
        std::process::exit(124);
    }
    anyhow::ensure!(
        passed,
        "Load test failed; inspect load_test_summary.json and load_test_results.json"
    );
    Ok(())
}

/// Immutable metadata only. The separate marker cannot be overwritten by a
/// racing normal summary write; its presence always invalidates that run.
fn write_timeout_results(
    config: &TestConfig,
    started_at: &str,
    provenance: &serde_json::Value,
) -> Result<()> {
    let report = serde_json::json!({
        "schemaVersion": 2,
        "run": {
            "completed": false, "passed": false,
            "failureReasons": ["Hard deadline exceeded; detailed metrics unavailable"],
            "startedAt": started_at, "finishedAt": chrono::Utc::now().to_rfc3339(),
            "configuration": config, "provenance": provenance,
        },
    });
    let json = serde_json::to_vec_pretty(&report)?;
    std::fs::write(config.output_dir.join("load_test_timeout.json"), &json)?;
    std::fs::write(config.output_dir.join("load_test_summary.json"), &json)?;
    Ok(())
}

/// Write results to JSON files. Fully synchronous — safe to call from OS thread.
fn write_results_sync(
    collectors: &[Arc<MetricsCollector>],
    config: &TestConfig,
    started_at: &str,
    provenance: &serde_json::Value,
    completed: bool,
    mut failures: Vec<String>,
) -> Result<bool> {
    let mut all_metrics = Vec::new();
    for collector in collectors {
        all_metrics.push(collector.generate_report());
    }

    let summary = TestSummary::from_metrics(&all_metrics);
    summary.print_summary();
    if summary.failed_connections > 0 || summary.failed_connection_attempts > 0 {
        failures.push("One or more WebSocket/room connection attempts failed".into());
    }
    if summary.total_errors > 0 || summary.total_reconnection_failures > 0 {
        failures.push("Client, signaling or media errors were recorded".into());
    }
    if summary.failed_consumers > 0 {
        failures.push(format!(
            "{} consumers failed sustained media delivery",
            summary.failed_consumers
        ));
    }
    let publishers = ((config.num_clients as f64 * config.publish_ratio).ceil() as usize)
        .max(1)
        .min(config.num_clients);
    for (i, metrics) in all_metrics.iter().enumerate() {
        let other_publishers = (0..publishers)
            .filter(|publisher| {
                *publisher != i && publisher % config.num_rooms == i % config.num_rooms
            })
            .count();
        let expected = if config.media_config.audio_enabled {
            other_publishers.min(config.max_audio_consumers)
        } else {
            0
        } + if config.media_config.video_enabled {
            other_publishers.min(config.max_video_consumers)
        } else {
            0
        };
        if metrics.consumers_created < expected as u32 {
            failures.push(format!(
                "{}: expected at least {expected} consumers, created {}",
                metrics.client_id, metrics.consumers_created
            ));
        }
        let validated = metrics
            .consumer_delivery
            .iter()
            .filter(|consumer| consumer.passed)
            .count();
        if validated < expected {
            failures.push(format!(
                "{}: expected at least {expected} validated consumers, observed {validated}",
                metrics.client_id,
            ));
        }
        if i < publishers && metrics.measurement.packets_queued == 0 {
            failures.push(format!(
                "{}: publisher queued no media in the shared measurement window",
                metrics.client_id
            ));
        }
    }
    let passed = completed && failures.is_empty();
    let mut report = serde_json::to_value(&summary)?;
    report["schemaVersion"] = serde_json::json!(2);
    report["run"] = serde_json::json!({
        "completed": completed, "passed": passed, "failureReasons": failures,
        "startedAt": started_at, "finishedAt": chrono::Utc::now().to_rfc3339(),
        "configuration": config, "provenance": provenance,
    });
    std::fs::write(
        config.output_dir.join("load_test_results.json"),
        serde_json::to_vec_pretty(&all_metrics)?,
    )?;
    std::fs::write(
        config.output_dir.join("load_test_summary.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    println!(
        "Run {}. Results: {}",
        if passed { "PASSED" } else { "FAILED" },
        config.output_dir.display()
    );
    Ok(passed)
}

fn generator_provenance(config: &TestConfig) -> Result<serde_json::Value> {
    use sha2::Digest;
    let binary = std::env::current_exe()?;
    Ok(serde_json::json!({
        "generatorRevision": config.generator_revision,
        "serverRevision": config.server_revision,
        "generatorBinarySha256": hex::encode(sha2::Sha256::digest(std::fs::read(&binary)?)),
        "generatorVersion": env!("CARGO_PKG_VERSION"),
        "os": std::env::consts::OS, "architecture": std::env::consts::ARCH,
        "logicalCpus": std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1),
        "profile": if cfg!(debug_assertions) { "debug" } else { "release" },
        "payload": "synthetic Opus/VP8-shaped RTP; not a browser decode/encode quality test",
        "sentCounters": "packets accepted by the local RTP writer, not confirmed network egress",
        "byteCounters": "queued RTP packet bytes; received RTP payload bytes (not comparable wire-byte totals)",
        "latencyPercentiles": "nearest-rank exact millisecond histograms merged across every operation",
        "window": "shared steady-state interval after ramp-up plus warmup",
    }))
}

async fn run_client(config: ClientConfig, client_id: String, metrics: Arc<MetricsCollector>) {
    if config.is_churner {
        run_churner_client(config, client_id, metrics).await;
    } else {
        match run_client_inner(config, client_id.clone(), metrics.clone()).await {
            Ok(_) => tracing::info!("{}: Client completed successfully", client_id),
            Err(e) => {
                metrics.record_error(format!("Client failed: {e:#}"));
                metrics.diagnostic_failure("Client attempt failed before pre-close capture");
                tracing::error!("{}: Client failed: {}", client_id, e);
            }
        }
        metrics.end_session();
    }
}

/// Churner client: repeatedly connects, runs for a random duration, disconnects, and reconnects
async fn run_churner_client(
    config: ClientConfig,
    client_id: String,
    metrics: Arc<MetricsCollector>,
) {
    let mut iteration = 0u32;

    while Instant::now() < config.deadline {
        iteration += 1;
        tracing::info!("{}: Churn iteration {} starting", client_id, iteration);

        // Random session duration between min and max
        let session_secs = config.churn_session_min_secs
            + (rand::random::<u64>()
                % (config.churn_session_max_secs - config.churn_session_min_secs + 1));
        let remaining = config.deadline.saturating_duration_since(Instant::now());
        let this_session = Duration::from_secs(session_secs).min(remaining);

        if this_session.as_secs() < 3 {
            break; // Not enough time for a meaningful session
        }

        let mut churn_config = config.clone();
        churn_config.session_duration = this_session;
        churn_config.deadline = (Instant::now() + this_session).min(config.deadline);
        // Every client participates in the same initial measured cohort before
        // intentional churn begins; otherwise short sessions could all expire
        // during warmup and a no-media run could appear to pass.
        if iteration == 1 {
            churn_config.deadline = churn_config
                .deadline
                .max(config.measurement_start + Duration::from_secs(5))
                .min(config.deadline);
        }

        match run_client_inner(churn_config, client_id.clone(), metrics.clone()).await {
            Ok(_) => {
                if iteration > 1 {
                    metrics.record_reconnection();
                }
                tracing::info!(
                    "{}: Churn iteration {} completed ({}s)",
                    client_id,
                    iteration,
                    this_session.as_secs()
                );
            }
            Err(e) => {
                metrics.record_reconnection_failure();
                metrics.record_error(format!("Churn attempt {iteration} failed: {e:#}"));
                metrics.diagnostic_failure("Client attempt failed before pre-close capture");
                tracing::warn!("{}: Churn iteration {} failed: {}", client_id, iteration, e);
            }
        }
        metrics.end_session();

        // Cooldown before reconnecting
        let remaining = config.deadline.saturating_duration_since(Instant::now());
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
    tracing::info!(
        "{}: Starting synthetic client (publisher={}, room={})",
        client_id,
        config.is_publisher,
        config.room_id
    );

    metrics.set_room_id(&config.room_id);
    metrics.begin_connection_attempt();

    // Connect to WebSocket signaling server
    let (ws_stream, _) = connect_async(&config.server_url).await.map_err(|e| {
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
        password: None,
    };
    send_message(&mut write, join_msg).await?;

    // Wait for room joined response
    let _participant_id = match receive_response(&mut read, &mut buffered_events).await? {
        ServerMessage::RoomJoined {
            participant_id,
            participants,
            ..
        } => {
            metrics.record_signaling_latency("join_room", t.elapsed().as_millis() as u64);
            tracing::info!(
                "{}: Joined room as {}, {} other participants",
                client_id,
                participant_id,
                participants.len()
            );
            metrics.mark_connection_successful();

            // In webinar/panel mode (publish_ratio < 1.0), viewers need to discover
            // producers that were created before they joined. In conference mode,
            // NewProducer events handle this naturally (everyone publishes, so late
            // joiners discover producers via broadcast events).
            if config.consume_existing_producers {
                let existing_producer_count: usize =
                    participants.iter().map(|p| p.producers.len()).sum();
                if existing_producer_count > 0 {
                    tracing::info!(
                        "{}: Room has {} existing producers from {} participants (consuming deferred)",
                        client_id,
                        existing_producer_count,
                        participants.len()
                    );
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
                let existing_producer_count: usize =
                    participants.iter().map(|p| p.producers.len()).sum();
                if existing_producer_count > 0 {
                    tracing::debug!(
                        "{}: Skipping {} existing producers (conference mode — NewProducer events will handle)",
                        client_id,
                        existing_producer_count
                    );
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
        serde_json::to_value(&router_caps).expect("Failed to serialize router caps"),
    )
    .expect("Failed to deserialize as RtpCapabilities");

    // Create WebRTC session (metrics are passed to recv transport's on_track handler)
    let webrtc_session = WebRtcSession::new(client_id.clone(), metrics.clone());
    let webrtc_session = Arc::new(Mutex::new(webrtc_session));

    // Publishers create send transport; viewers skip it
    let mut send_transport_id: Option<String> = None;
    if config.is_publisher {
        // Create send transport (timed)
        let t = Instant::now();
        send_message(&mut write, ClientMessage::CreateSendTransport).await?;
        let (st_id, send_ice_params, send_ice_cands, send_dtls_params) =
            match receive_response(&mut read, &mut buffered_events).await? {
                ServerMessage::TransportCreated {
                    transport_id,
                    ice_parameters,
                    ice_candidates,
                    dtls_parameters,
                    ..
                } => {
                    metrics.record_signaling_latency(
                        "create_send_transport",
                        t.elapsed().as_millis() as u64,
                    );
                    tracing::debug!("{}: Send transport created: {}", client_id, transport_id);
                    (
                        transport_id,
                        ice_parameters,
                        ice_candidates,
                        dtls_parameters,
                    )
                }
                ServerMessage::Error { message } => {
                    metrics.record_error(format!("Failed to create send transport: {}", message));
                    return Err(anyhow::anyhow!(
                        "Failed to create send transport: {}",
                        message
                    ));
                }
                msg => {
                    return Err(anyhow::anyhow!("Unexpected message: {:?}", msg));
                }
            };

        // Create REAL WebRTC send transport
        let local_send_dtls = webrtc_session
            .lock()
            .await
            .create_send_transport(
                st_id.clone(),
                send_ice_params,
                send_ice_cands,
                send_dtls_params,
            )
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
                metrics.record_signaling_latency(
                    "connect_send_transport",
                    t.elapsed().as_millis() as u64,
                );
                tracing::info!("{}: Send transport connected (REAL ICE/DTLS!)", client_id);
            }
            ServerMessage::Error { message } => {
                metrics.record_error(format!("Send transport connect failed: {}", message));
                return Err(anyhow::anyhow!(
                    "Send transport connect failed: {}",
                    message
                ));
            }
            _ => {}
        }

        send_transport_id = Some(st_id);
    }

    // Create receive transport (timed) — all clients need this
    let t = Instant::now();
    send_message(&mut write, ClientMessage::CreateRecvTransport).await?;
    let (recv_transport_id, recv_ice_params, recv_ice_cands, recv_dtls_params) =
        match receive_response(&mut read, &mut buffered_events).await? {
            ServerMessage::TransportCreated {
                transport_id,
                ice_parameters,
                ice_candidates,
                dtls_parameters,
                ..
            } => {
                metrics.record_signaling_latency(
                    "create_recv_transport",
                    t.elapsed().as_millis() as u64,
                );
                tracing::debug!("{}: Receive transport created: {}", client_id, transport_id);
                (
                    transport_id,
                    ice_parameters,
                    ice_candidates,
                    dtls_parameters,
                )
            }
            ServerMessage::Error { message } => {
                metrics.record_error(format!("Failed to create recv transport: {}", message));
                return Err(anyhow::anyhow!(
                    "Failed to create recv transport: {}",
                    message
                ));
            }
            msg => {
                return Err(anyhow::anyhow!("Unexpected message: {:?}", msg));
            }
        };

    // Create REAL WebRTC receive transport
    let local_recv_dtls = webrtc_session
        .lock()
        .await
        .create_recv_transport(
            recv_transport_id.clone(),
            recv_ice_params,
            recv_ice_cands,
            recv_dtls_params,
        )
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
            metrics
                .record_signaling_latency("connect_recv_transport", t.elapsed().as_millis() as u64);
            tracing::info!(
                "{}: Receive transport connected (REAL ICE/DTLS!)",
                client_id
            );
        }
        ServerMessage::Error { message } => {
            metrics.record_error(format!("Receive transport connect failed: {}", message));
            return Err(anyhow::anyhow!(
                "Receive transport connect failed: {}",
                message
            ));
        }
        _ => {}
    }

    // Publishers: produce media
    if config.is_publisher {
        let st_id = send_transport_id.as_ref().unwrap();

        // Get actual SSRCs from webrtc-rs (tracks were bound during send transport SDP negotiation)
        let (audio_ssrc, video_ssrc) = webrtc_session.lock().await.send_ssrcs().await?;
        tracing::info!(
            "{}: Send transport SSRCs - audio: {}, video: {}",
            client_id,
            audio_ssrc,
            video_ssrc
        );

        // Produce audio if enabled (timed)
        if config.media_config.audio_enabled {
            let t = Instant::now();
            let audio_params = extract_rtp_parameters(
                MediaKind::Audio,
                audio_ssrc,
                config.media_config.video_bitrate_kbps,
            );
            let produce_msg = ClientMessage::Produce {
                transport_id: st_id.clone(),
                kind: MediaKind::Audio,
                rtp_parameters: audio_params,
                source: Some("microphone".to_string()),
            };
            send_message(&mut write, produce_msg).await?;
            match receive_response(&mut read, &mut buffered_events).await? {
                ServerMessage::ProducerCreated { producer_id } => {
                    metrics
                        .record_signaling_latency("produce_audio", t.elapsed().as_millis() as u64);
                    tracing::info!("{}: Audio producer created: {}", client_id, producer_id);
                    metrics.record_producer_created();
                    metrics.record_publisher(&producer_id);
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
            let video_params = extract_rtp_parameters(
                MediaKind::Video,
                video_ssrc,
                config.media_config.video_bitrate_kbps,
            );
            let produce_msg = ClientMessage::Produce {
                transport_id: st_id.clone(),
                kind: MediaKind::Video,
                rtp_parameters: video_params,
                source: Some("camera".to_string()),
            };
            send_message(&mut write, produce_msg).await?;
            match receive_response(&mut read, &mut buffered_events).await? {
                ServerMessage::ProducerCreated { producer_id } => {
                    metrics
                        .record_signaling_latency("produce_video", t.elapsed().as_millis() as u64);
                    tracing::info!("{}: Video producer created: {}", client_id, producer_id);
                    metrics.record_producer_created();
                    metrics.record_publisher(&producer_id);
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
                        if audio_count < config.max_audio_consumers {
                            audio_count += 1;
                        }
                    }
                    MediaKind::Video => {
                        if video_count < config.max_video_consumers {
                            video_count += 1;
                        }
                    }
                }
            }
        }
        audio_count + video_count
    };

    if !buffered_events.is_empty() {
        tracing::info!(
            "{}: Replaying {} buffered events, expecting {} consumers",
            client_id,
            buffered_events.len(),
            expected_consumers
        );
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
            )
            .await;
        }
    }

    // Wait for ConsumerCreated responses and batch into a single SDP renegotiation.
    // Cap at 5s to avoid blocking setup. Any remaining consumers are handled by
    // receive_messages_loop which has its own batching.
    if expected_consumers > 0 {
        let overall_timeout =
            Duration::from_millis(((expected_consumers as u64) * 10).clamp(1000, 5000));
        let consumer_deadline = tokio::time::Instant::now() + overall_timeout;
        let mut received_consumers = pending_resumes.len();

        tracing::debug!(
            "{}: Waiting for {} consumers (have {}), timeout {}ms",
            client_id,
            expected_consumers,
            received_consumers,
            overall_timeout.as_millis()
        );

        while tokio::time::Instant::now() < consumer_deadline
            && received_consumers < expected_consumers
        {
            tokio::select! {
                msg = read.next() => {
                    if let Some(Ok(Message::Text(text))) = msg
                        && let Ok(server_msg) = serde_json::from_str::<ServerMessage>(&text)
                    {
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
                _ = tokio::time::sleep(Duration::from_millis(200)) => {
                    // 200ms of silence — consumers have stopped arriving
                    tracing::debug!("{}: Consumer collection idle timeout ({}/{})",
                        client_id, received_consumers, expected_consumers);
                    break;
                }
            }
        }
        tracing::info!(
            "{}: Collected {}/{} consumers",
            client_id,
            received_consumers,
            expected_consumers
        );
    }

    // Renegotiate SDP once for ALL consumers recorded during setup.
    if needs_renegotiation {
        webrtc_session.lock().await.renegotiate_consumers().await?;
        if metrics.diagnostics_enabled() {
            metrics.diagnostic_event(
                "renegotiation-applied",
                serde_json::json!({"consumerCount": pending_resumes.len()}),
            );
        }

        // Preserve the existing settling interval. In webrtc 0.20 core SSRC
        // registration is synchronous; driver IO/event delivery remains async.
        tokio::time::sleep(Duration::from_millis(100)).await;
        tracing::debug!(
            "{}: Consumer renegotiation settling interval completed",
            client_id
        );
    }

    // Resume consumers only after applying the complete SDP batch.
    let resume_count = pending_resumes.len();
    for consumer_id in pending_resumes.drain(..) {
        if metrics.diagnostics_enabled() {
            metrics.diagnostic_event(
                "resume-requested",
                serde_json::json!({"consumerId": consumer_id}),
            );
        }
        let resume_msg = ClientMessage::ResumeConsumer {
            consumer_id: consumer_id.clone(),
        };
        let json = serde_json::to_string(&resume_msg)?;
        write.feed(Message::Text(json.into())).await?;
    }
    if resume_count > 0 {
        write.flush().await?;
        tracing::info!(
            "{}: Resumed {} consumers after initial SDP renegotiation",
            client_id,
            resume_count
        );
    }

    // write_rtp in webrtc 0.20 only enqueues. Wait before generating media so
    // the first keyframe and packet counters start after the SRTP handshake.
    if config.is_publisher {
        webrtc_session.lock().await.wait_send_connected().await?;
    }
    if !config.is_churner && Instant::now() > config.measurement_start {
        metrics.record_error("Setup exceeded shared ramp-up/warmup window; increase warmup before comparing performance".into());
    }

    tracing::info!(
        "{}: Setup complete, starting media session for {}s",
        client_id,
        config.session_duration.as_secs()
    );

    // Start media sending task (publishers only)
    let media_task = if config.is_publisher {
        let media_gen = MediaGenerator::new(config.media_config.clone());
        let metrics_send = metrics.clone();
        let media_config = config.media_config.clone();
        let client_id_send = client_id.clone();
        let webrtc_session_send = Arc::clone(&webrtc_session);
        Some(tokio::spawn(async move {
            send_real_media_loop(
                webrtc_session_send,
                media_gen,
                media_config,
                metrics_send,
                client_id_send,
            )
            .await;
        }))
    } else {
        None
    };

    let metrics_recv = metrics.clone();
    let client_id_recv = client_id.clone();
    // Receive loop gets extra time so the abort (not timeout) controls shutdown
    let recv_timeout =
        config.deadline.saturating_duration_since(Instant::now()) + Duration::from_secs(5);

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
        )
        .await;
    });

    // Every client ends at the same instant; setup/ramp no longer inflate rates.
    tokio::time::sleep_until(config.deadline.into()).await;

    // Publish the intentional lifetime boundary before dropping signaling or
    // closing peers. Other clients may observe ProducerClosed immediately.
    metrics.end_session();
    if metrics.diagnostics_enabled() {
        match bounded_diagnostic_snapshot(Duration::from_secs(2), async {
            webrtc_session.lock().await.diagnostic_snapshot().await
        })
        .await
        {
            Ok(snapshot) => metrics.diagnostic_snapshot(snapshot),
            Err(error) => metrics.diagnostic_failure(&error.to_string()),
        }
    }
    // Clean shutdown — explicitly close PeerConnections to avoid slow async drop.
    tracing::info!("{}: Session duration completed, shutting down", client_id);
    if let Some(task) = media_task {
        task.abort();
        if let Err(error) = task.await
            && !error.is_cancelled()
        {
            metrics.record_error(format!("Media task failed: {error}"));
        }
    }
    receive_task.abort();
    if let Err(error) = receive_task.await
        && !error.is_cancelled()
    {
        metrics.record_error(format!("Receive task failed: {error}"));
    }

    // Close PeerConnections synchronously before returning
    if let Err(e) = webrtc_session.lock().await.close().await {
        metrics.record_error(format!("PeerConnection cleanup failed: {e}"));
    }

    Ok(())
}

/// Include lock acquisition and both peers in the diagnostic timeout. This
/// opt-in snapshot never substitutes for measured consumer-delivery checks.
async fn bounded_diagnostic_snapshot(
    limit: Duration,
    snapshot: impl std::future::Future<Output = Result<serde_json::Value>>,
) -> Result<serde_json::Value> {
    tokio::time::timeout(limit, snapshot)
        .await
        .context("Pre-close diagnostic snapshot timed out; evidence is incomplete")?
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
    let (audio_track, video_track, ssrcs) = {
        let session = webrtc_session.lock().await;
        (
            session.audio_track(),
            session.video_track(),
            session.send_ssrcs().await,
        )
    };
    let (audio_ssrc, video_ssrc) = match ssrcs {
        Ok(ssrcs) => ssrcs,
        Err(error) => {
            metrics.record_error(format!("Cannot send without negotiated SSRCs: {error}"));
            return;
        }
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

                    let packet = negotiated_rtp_packet(&packet_bytes, audio_ssrc);
                    let result = match packet {
                        Ok(packet) => track.write_rtp(packet).await,
                        Err(error) => Err(error),
                    };
                    match result {
                        Ok(_) => {
                            metrics.record_packet_sent(packet_bytes.len());
                            if first_packet {
                                metrics.mark_first_media_sent();
                                first_packet = false;
                                tracing::info!("{}: First REAL RTP packet sent!", client_id);
                            }
                        }
                        Err(e) => {
                            metrics.record_error(format!("RTP write failed: {e}"));
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
                        let packet = negotiated_rtp_packet(&packet_bytes, video_ssrc);
                        let result = match packet {
                            Ok(packet) => track.write_rtp(packet).await,
                            Err(error) => Err(error),
                        };
                        match result {
                            Ok(_) => {
                                metrics.record_packet_sent(packet_bytes.len());
                                if first_packet {
                                    metrics.mark_first_media_sent();
                                    first_packet = false;
                                    tracing::info!("{}: First REAL RTP packet sent!", client_id);
                                }
                            }
                            Err(e) => {
                                metrics.record_error(format!("RTP write failed: {e}"));
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

/// The 0.20 track API requires packets to carry the negotiated SSRC. Unlike
/// the old raw-byte writer, it does not rewrite the generator's random SSRC.
fn negotiated_rtp_packet(
    packet_bytes: &[u8],
    ssrc: u32,
) -> webrtc::error::Result<rtc::rtp::Packet> {
    let mut source = packet_bytes;
    let mut packet = rtc::rtp::Packet::unmarshal(&mut source)?;
    packet.header.ssrc = ssrc;
    Ok(packet)
}

#[cfg(test)]
mod packet_migration_tests {
    use super::*;

    #[test]
    fn cli_rejects_malformed_and_unknown_workload_options() {
        for args in [
            vec!["load_test", "--clients", "nope"],
            vec!["load_test", "--duration"],
            vec!["load_test", "--clients", "--duration"],
            vec!["load_test", "--mode", "confernece"],
            vec!["load_test", "--publish-ratio", "NaN"],
            vec!["load_test", "--fps", "999"],
            vec!["load_test", "--unknown", "1"],
            vec!["load_test", "--server", "ws://user:password@localhost/ws"],
        ] {
            let args = args.into_iter().map(str::to_string).collect::<Vec<_>>();
            assert!(validate_cli_value(&args, 1).is_err(), "{args:?}");
        }
        assert!(validate_cli_value(&["load_test".into(), "--audio-only".into()], 1).is_ok());
        assert!(validate_cli_value(&["load_test".into(), "--diagnostics".into()], 1).is_ok());
        assert!(!TestConfig::default().diagnostics);
    }

    #[tokio::test]
    async fn diagnostic_snapshot_timeout_cancels_capture_and_releases_locks() {
        let lock = Arc::new(Mutex::new(()));
        let start = Instant::now();
        let result = bounded_diagnostic_snapshot(Duration::from_millis(10), async {
            let _guard = lock.lock().await;
            std::future::pending::<Result<serde_json::Value>>().await
        })
        .await;
        assert!(result.unwrap_err().to_string().contains("timed out"));
        assert!(start.elapsed() < Duration::from_secs(1));
        assert!(lock.try_lock().is_ok());
        assert_eq!(
            bounded_diagnostic_snapshot(Duration::from_secs(1), async {
                Ok(serde_json::json!({"captured": true}))
            })
            .await
            .unwrap()["captured"],
            true
        );
    }

    #[test]
    fn generated_packets_use_negotiated_ssrc_without_altering_media_payload() {
        let mut generator = MediaGenerator::new(MediaConfig::default());
        let audio = generator.generate_audio_packet();
        let mut video = generator.generate_video_frame();
        assert!(!video.is_empty());
        for (bytes, ssrc, payload_type) in [(audio, 12_345, 111), (video.remove(0), 54_321, 96)] {
            let before = rtc::rtp::Packet::unmarshal(&mut bytes.as_slice()).unwrap();
            let after = negotiated_rtp_packet(&bytes, ssrc).unwrap();
            assert_eq!(after.header.ssrc, ssrc);
            assert_eq!(after.header.payload_type, payload_type);
            assert_eq!(after.header.sequence_number, before.header.sequence_number);
            assert_eq!(after.header.timestamp, before.header.timestamp);
            assert_eq!(after.payload, before.payload);
        }
        assert!(negotiated_rtp_packet(&[0], 12_345).is_err());
    }
}

#[cfg(test)]
mod watchdog_tests {
    use super::*;
    use std::sync::{Mutex as StdMutex, atomic::AtomicU8, mpsc};

    #[test]
    fn blocked_reporter_cannot_block_deadline_exit() {
        let collector_lock = Arc::new(StdMutex::new(()));
        let held = collector_lock.lock().unwrap();
        let reporter_lock = collector_lock.clone();
        let (_stop, receiver) = mpsc::channel();
        let (report_started, report_started_rx) = mpsc::channel();
        let (exited, exited_rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            watchdog_wait(
                Instant::now() + Duration::from_millis(10),
                Duration::from_millis(20),
                Arc::new(AtomicU8::new(WATCHDOG_RUNNING)),
                receiver,
                move || {
                    let _ = report_started.send(());
                    let _blocked = reporter_lock.lock().unwrap();
                },
                move |code| {
                    let _ = exited.send(code);
                },
            );
        });
        assert!(
            report_started_rx
                .recv_timeout(Duration::from_secs(1))
                .is_ok()
        );
        let status = exited_rx.recv_timeout(Duration::from_secs(1));
        drop(held);
        assert_eq!(status.unwrap(), 124);
        worker.join().unwrap();
    }

    #[test]
    fn expired_deadline_cannot_be_completed_successfully() {
        let (stop, _receiver) = mpsc::channel();
        let expired = RunWatchdog {
            deadline: Instant::now() - Duration::from_millis(1),
            state: Arc::new(AtomicU8::new(WATCHDOG_RUNNING)),
            stop,
        };
        assert!(!expired.finish());
        let (stop, _receiver) = mpsc::channel();
        let already_expired = RunWatchdog {
            deadline: Instant::now() + Duration::from_secs(1),
            state: Arc::new(AtomicU8::new(WATCHDOG_EXPIRED)),
            stop,
        };
        assert!(!already_expired.finish());
    }

    #[test]
    fn timeout_artifacts_fail_closed_without_collectors() {
        let directory = std::env::temp_dir().join(format!(
            "simplestchat-timeout-unit-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir(&directory).unwrap();
        let config = TestConfig {
            output_dir: directory.clone(),
            ..Default::default()
        };
        write_timeout_results(&config, "test-start", &serde_json::json!({ "test": true })).unwrap();
        for name in ["load_test_timeout.json", "load_test_summary.json"] {
            let path = directory.join(name);
            let report: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            assert_eq!(report["run"]["completed"], false);
            assert_eq!(report["run"]["passed"], false);
            assert_eq!(report["run"]["configuration"]["numClients"], 5);
            std::fs::remove_file(path).unwrap();
        }
        std::fs::remove_dir(directory).unwrap();
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "The owned receiver task takes its session resources, limits and pending producer events explicitly."
)]
async fn receive_messages_loop(
    mut read: futures_util::stream::SplitStream<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    >,
    mut write: futures_util::stream::SplitSink<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        Message,
    >,
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
    let mut deferred_events: std::collections::VecDeque<ServerMessage> =
        existing_producer_events.into();
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
                        metrics.record_error(format!("Unexpected signaling binary frame ({} bytes), not RTP", data.len()));
                        true
                    }
                    Some(Err(e)) => {
                        tracing::error!("{}: WebSocket error: {}", client_id, e);
                        metrics.record_error(format!("WebSocket error: {}", e));
                        break;
                    }
                    None => {
                        metrics.record_error("WebSocket closed before the session deadline".into());
                        tracing::info!("{}: WebSocket closed", client_id);
                        break;
                    }
                    Some(Ok(Message::Close(_))) => {
                        metrics.record_error("Server closed WebSocket before the session deadline".into());
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
                    )
                    .await;
                    deferred_batch_size += 1;
                }
            }
            if deferred_events.is_empty() && deferred_batch_size > 0 {
                tracing::debug!(
                    "{}: Finished processing {} deferred existing-producer events",
                    client_id,
                    deferred_batch_size
                );
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
                metrics.record_error(format!("Consumer renegotiation failed: {e}"));
                tracing::error!("{}: Failed to renegotiate consumers: {}", client_id, e);
            } else if metrics.diagnostics_enabled() {
                metrics.diagnostic_event(
                    "renegotiation-applied",
                    serde_json::json!({"consumerCount": pending_resumes.len()}),
                );
            }

            let batch_size = pending_resumes.len();

            // Adaptive SSRC wait
            let ssrc_wait_ms = (batch_size as u64 * 5).clamp(50, 200);
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
                                metrics.record_error(format!("Unexpected signaling binary frame ({} bytes), not RTP", data.len()));
                            }
                            Some(Err(e)) => {
                                tracing::error!("{}: WebSocket error during SSRC wait: {}", client_id, e);
                                metrics.record_error(format!("WebSocket error: {}", e));
                                ws_dead = true;
                                break;
                            }
                            None => {
                                metrics.record_error("WebSocket closed during consumer setup".into());
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
            if ws_dead {
                break;
            }

            // Only resume consumers from the CURRENT batch
            let to_resume: Vec<String> = pending_resumes.drain(..batch_size).collect();
            let resume_count = to_resume.len();
            for consumer_id in to_resume {
                if metrics.diagnostics_enabled() {
                    metrics.diagnostic_event(
                        "resume-requested",
                        serde_json::json!({"consumerId": consumer_id}),
                    );
                }
                let resume_msg = ClientMessage::ResumeConsumer {
                    consumer_id: consumer_id.clone(),
                };
                let json = serde_json::to_string(&resume_msg).unwrap();
                if let Err(e) = write.feed(Message::Text(json.into())).await {
                    metrics.record_error(format!("Consumer resume write failed: {e}"));
                    tracing::error!(
                        "{}: Failed to feed resume for {}: {}",
                        client_id,
                        consumer_id,
                        e
                    );
                }
            }
            if resume_count > 0 {
                if let Err(e) = write.flush().await {
                    metrics.record_error(format!("Consumer resume flush failed: {e}"));
                    tracing::error!("{}: Failed to flush resumes: {}", client_id, e);
                }
                total_resumed += resume_count;
                tracing::debug!(
                    "{}: Renegotiated + resumed {} consumers (total: {}, timer: {}ms)",
                    client_id,
                    resume_count,
                    total_resumed,
                    max_timer_ms
                );
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

#[expect(
    clippy::too_many_arguments,
    reason = "The shared initial/live signaling handler borrows subscription state explicitly without transferring ownership."
)]
async fn handle_server_message(
    msg: ServerMessage,
    metrics: &Arc<MetricsCollector>,
    client_id: &str,
    write: &mut futures_util::stream::SplitSink<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
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
        ServerMessage::NewProducer {
            participant_id: _,
            producer_id,
            kind,
            ..
        } => {
            // Smart subscription: separate caps for audio and video
            let at_cap = match kind {
                MediaKind::Audio => *audio_consumes_sent >= max_audio,
                MediaKind::Video => *video_consumes_sent >= max_video,
            };

            if at_cap {
                return;
            }
            if !metrics.subscribe(&producer_id, kind == MediaKind::Audio) {
                return;
            }

            tracing::debug!(
                "{}: New producer available: {} ({:?}), creating consumer...",
                client_id,
                producer_id,
                kind
            );

            let consume_msg = ClientMessage::Consume {
                producer_id: producer_id.clone(),
                rtp_capabilities: rtp_capabilities.clone(),
            };

            if let Err(e) = send_message(write, consume_msg).await {
                metrics.record_error(format!("Consume request failed: {e}"));
                tracing::error!("{}: Failed to send Consume message: {}", client_id, e);
                return;
            }

            match kind {
                MediaKind::Audio => *audio_consumes_sent += 1,
                MediaKind::Video => *video_consumes_sent += 1,
            }

            let total = *audio_consumes_sent + *video_consumes_sent;
            tracing::debug!(
                "{}: Sent Consume request (audio:{}/{}, video:{}/{}, total:{}) for producer {}",
                client_id,
                audio_consumes_sent,
                max_audio,
                video_consumes_sent,
                max_video,
                total,
                producer_id
            );
        }
        ServerMessage::ConsumerCreated {
            consumer_id,
            producer_id,
            kind,
            rtp_parameters,
        } => {
            metrics.record_consumer_created();
            let ssrc = rtp_parameters
                .encodings
                .first()
                .and_then(|encoding| encoding.ssrc);
            if let Some(ssrc) = ssrc {
                metrics.record_consumer(&consumer_id, &producer_id, ssrc);
                if metrics.diagnostics_enabled() {
                    metrics.diagnostic_event("consumer-created", serde_json::json!({
                        "consumerId": consumer_id, "producerId": producer_id, "kind": kind, "ssrc": ssrc,
                    }));
                }
            } else {
                metrics.record_error(format!("Consumer {consumer_id} has no SSRC"));
            }

            // Record consumer info WITHOUT renegotiating SDP yet.
            if let Err(e) = webrtc_session.lock().await.record_consumer(
                producer_id.clone(),
                kind,
                &rtp_parameters,
            ) {
                metrics.record_error(format!("Consumer setup failed: {e}"));
                tracing::error!("{}: Failed to record consumer: {}", client_id, e);
                return;
            }

            pending_resumes.push(consumer_id.clone());
            *needs_renegotiation = true;
        }
        ServerMessage::ParticipantJoined {
            participant_id,
            participant_name,
            ..
        } => {
            tracing::debug!(
                "{}: Participant joined: {} ({})",
                client_id,
                participant_name,
                participant_id
            );
        }
        ServerMessage::ParticipantLeft { participant_id } => {
            tracing::debug!("{}: Participant left: {}", client_id, participant_id);
        }
        ServerMessage::ProducerClosed { producer_id } => {
            let (kind, unexpected) = metrics.close_producer(&producer_id);
            if unexpected {
                metrics.record_error(format!("Server closed active generated producer {producer_id} before its planned lifetime ended"));
            }
            match kind {
                Some(true) => *audio_consumes_sent = audio_consumes_sent.saturating_sub(1),
                Some(false) => *video_consumes_sent = video_consumes_sent.saturating_sub(1),
                None => {}
            }
            tracing::debug!("{}: Producer closed: {}", client_id, producer_id);
        }
        ServerMessage::ProducerPaused { producer_id } => {
            tracing::debug!("{}: Producer paused: {}", client_id, producer_id);
        }
        ServerMessage::ProducerResumed { producer_id } => {
            tracing::debug!("{}: Producer resumed: {}", client_id, producer_id);
        }
        ServerMessage::ConsumerResumed { consumer_id } => {
            if metrics.diagnostics_enabled() {
                metrics
                    .diagnostic_event("resume-ack", serde_json::json!({"consumerId": consumer_id}));
            }
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
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
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
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
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
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
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
            ServerMessage::ActiveSpeaker { .. } => {}
            ServerMessage::AudioLevels { .. } => {}
            ServerMessage::ForceClosedProducer { .. } => {}
            ServerMessage::CamBanned { .. } => {}
            ServerMessage::CamUnbanned { .. } => {}
            ServerMessage::TextMuted { .. } => {}
            ServerMessage::TextUnmuted { .. } => {}
            ServerMessage::ParticipantKicked { .. } => {}
            ServerMessage::ParticipantBanned { .. } => {}
            ServerMessage::RoleChanged { .. } => {}
            ServerMessage::VoiceRequested { .. } => {}
            ServerMessage::RoomSettingsChanged { .. } => {}
            ServerMessage::TopicChanged { .. } => {}
            ServerMessage::LobbyWaiting { .. } => {}
            ServerMessage::LobbyJoin { .. } => {}
            ServerMessage::LobbyDenied { .. } => {}
            ServerMessage::LobbyAdmitted => {}
            msg => return Ok(msg),
        }
    }
}

fn extract_rtp_parameters(kind: MediaKind, ssrc: u32, video_bitrate_kbps: u32) -> RtpParameters {
    // Payload types must match the router's codec config (see config.rs)
    match kind {
        MediaKind::Audio => RtpParameters {
            mid: None,
            msid: None,
            codecs: vec![RtpCodecParameters::Audio {
                mime_type: MimeTypeAudio::Opus,
                payload_type: 111,
                clock_rate: NonZeroU32::new(48000).unwrap(),
                channels: NonZeroU8::new(2).unwrap(),
                parameters: RtpCodecParametersParameters::default(),
                rtcp_feedback: vec![],
            }],
            header_extensions: vec![RtpHeaderExtensionParameters {
                uri: RtpHeaderExtensionUri::Mid,
                id: 1,
                encrypt: false,
            }],
            encodings: vec![RtpEncodingParameters {
                ssrc: Some(ssrc),
                ..Default::default()
            }],
            rtcp: RtcpParameters::default(),
        },
        MediaKind::Video => RtpParameters {
            mid: None,
            msid: None,
            codecs: vec![RtpCodecParameters::Video {
                mime_type: MimeTypeVideo::Vp8,
                payload_type: 96,
                clock_rate: NonZeroU32::new(90000).unwrap(),
                parameters: RtpCodecParametersParameters::default(),
                rtcp_feedback: vec![],
            }],
            header_extensions: vec![RtpHeaderExtensionParameters {
                uri: RtpHeaderExtensionUri::Mid,
                id: 1,
                encrypt: false,
            }],
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
    println!("  cargo run --features load-test --bin load_test -- [OPTIONS]");
    println!("\nOptions:");
    println!("  -c, --clients <N>          Number of concurrent clients (default: 5)");
    println!("  -d, --duration <SECS>      Shared measurement duration, 3–3600s (default: 30)");
    println!("  -r, --ramp-up <SECS>       Ramp-up period in seconds (default: 5)");
    println!(
        "  --warmup <SECS>            Warmup after ramp, before shared counters (default: 10)"
    );
    println!(
        "  --deadline-grace <SECS>    Watchdog grace after measurement (default: 30; expiry exits 124)"
    );
    println!("  --output-dir <PATH>        Directory for both JSON reports (default: .)");
    println!(
        "  --diagnostics              Bounded pre-close RTC stats, sanitized SDP and lifecycle events"
    );
    println!("  --run-label <LABEL>        Human-readable run identifier");
    println!("  --server-revision <SHA>    Server source revision supplied by the runner");
    println!("  --generator-revision <SHA> Generator source revision supplied by the runner");
    println!("  -s, --server <URL>         Server WebSocket URL (default: ws://localhost:3000/ws)");
    println!("  --room <ID>                Room ID to join (default: load-test-room)");
    println!(
        "  --rooms <N>                Number of rooms to distribute clients across (default: 1)"
    );
    println!("  --publish-ratio <0.0-1.0>  Fraction of clients that publish media (default: 1.0)");
    println!("  --mode <MODE>              Preset publish ratios: webinar (1%), panel (10%),");
    println!("                             classroom (20%), conference (100%)");
    println!(
        "  --churn-rate <N>           Clients churning (disconnect/reconnect) per second (default: 0)"
    );
    println!("  --audio-only               Send only audio (no video)");
    println!("  --video-only               Send only video (no audio)");
    println!("  -q, --quality <PRESET>     Video quality: 480p (default), 720p, 1080p");
    println!("                             Sets resolution and bitrate to realistic values");
    println!("  --fps <15|30|60>           Frames per second (default: 30)");
    println!("                             Bitrate scales: 15fps=0.6x, 30fps=1x, 60fps=1.5x");
    println!("  -h, --help                 Print this help message");
    println!("\nExamples:");
    println!("  # Basic load test");
    println!("  cargo run --features load-test --bin load_test -- --clients 10 --duration 60");
    println!();
    println!("  # Multi-room: 1000 clients across 16 rooms (one per worker)");
    println!(
        "  cargo run --features load-test --bin load_test -- --clients 1000 --rooms 16 --duration 30"
    );
    println!();
    println!("  # Webinar: 10 presenters, 990 viewers");
    println!(
        "  cargo run --features load-test --bin load_test -- --clients 1000 --mode webinar --duration 60"
    );
    println!();
    println!("  # Churn test: 5 clients reconnecting per second");
    println!(
        "  cargo run --features load-test --bin load_test -- --clients 100 --churn-rate 5 --duration 60"
    );
    println!();
    println!("  # Panel discussions: 10% publish, 16 rooms");
    println!(
        "  cargo run --features load-test --bin load_test -- --clients 1000 --rooms 16 --mode panel"
    );
    println!("\nEnvironment Variables:");
    println!("  RUST_LOG=debug          Enable debug logging");
    println!("  RUST_LOG=info           Enable info logging (default)");
}
