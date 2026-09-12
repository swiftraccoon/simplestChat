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
use std::collections::{HashMap, HashSet};
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

mod subscriptions {
    include!("../clients/subscriptions.rs");
}

#[cfg(test)]
mod keyframe_tests {
    include!("../clients/keyframe_tests.rs");
}

use media_generator::{MediaConfig, MediaGenerator};
use metrics::{AttemptPlan, ClientMetrics, MeasurementWindow, MetricsCollector, TestSummary};
use rtc::shared::marshal::Unmarshal;
use std::num::{NonZeroU8, NonZeroU32};
use subscriptions::Subscriptions;
use tokio::sync::Mutex;
use webrtc::media_stream::track_local::TrackLocal;
use webrtc_client::WebRtcSession;

/// Departure is an experimental workload dimension, never an implicit change
/// to the default disconnect-and-reconnect-grace performance workload.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
enum Departure {
    #[default]
    Abrupt,
    ExplicitLeave,
}

impl Departure {
    fn parse(value: &str) -> Result<Self> {
        match value {
            "abrupt" => Ok(Self::Abrupt),
            "explicit-leave" => Ok(Self::ExplicitLeave),
            _ => anyhow::bail!("--departure must be abrupt or explicit-leave"),
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::Abrupt => "abrupt",
            Self::ExplicitLeave => "explicit-leave",
        }
    }
}

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
    /// Shared owned non-churning publishers in this room; expectations exclude self.
    stable_publishers: Arc<HashSet<String>>,
    churn_session_min_secs: u64,
    churn_session_max_secs: u64,
    max_audio_consumers: usize,
    max_video_consumers: usize,
    /// Whether to consume producers that already exist when joining.
    /// Only needed in webinar/panel mode where most clients don't publish.
    /// In conference mode, NewProducer events handle discovery naturally.
    consume_existing_producers: bool,
    departure: Departure,
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
    departure: Departure,
}

impl TestConfig {
    fn validate_departure(&self) -> Result<()> {
        anyhow::ensure!(
            self.departure == Departure::Abrupt || self.diagnostics,
            "--departure explicit-leave requires --diagnostics"
        );
        Ok(())
    }

    fn churner_count(&self) -> Result<usize> {
        anyhow::ensure!(
            self.churn_rate.is_finite() && self.churn_rate >= 0.0,
            "--churn-rate must be nonnegative and finite"
        );
        let count = ((self.churn_rate * self.duration_secs as f64) as usize).min(self.num_clients);
        anyhow::ensure!(
            self.churn_rate <= 0.0 || count > 0,
            "Positive --churn-rate selected no clients; increase the rate or duration"
        );
        Ok(count)
    }
}

fn stable_publishers_by_room(
    publishers: usize,
    churner_start: usize,
    rooms: usize,
) -> HashMap<usize, Arc<HashSet<String>>> {
    let rooms = rooms.max(1);
    let mut by_room: HashMap<usize, HashSet<String>> = HashMap::new();
    for publisher in 0..publishers.min(churner_start) {
        by_room
            .entry(publisher % rooms)
            .or_default()
            .insert(format!("client-{publisher}"));
    }
    by_room
        .into_iter()
        .map(|(room, publishers)| (room, Arc::new(publishers)))
        .collect()
}

fn stable_peer_count(publishers: &HashSet<String>, client: &str) -> usize {
    publishers.len() - usize::from(publishers.contains(client))
}

/// Additive evidence, not a claim that every changing publisher generation was
/// subscribed to. Legacy per-consumer checks still validate dynamic consumers.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct AttemptCoverageSummary {
    version: u8,
    scope: &'static str,
    available: bool,
    attempts: usize,
    passed_attempts: usize,
    failed_attempts: usize,
    skipped_short_tail_attempts: usize,
    missing_coverage_attempts: usize,
    requested_churners: usize,
    validated_churners: usize,
}

fn gate_attempt_coverage(
    clients: &[ClientMetrics],
    requested_churners: usize,
    failures: &mut Vec<String>,
) -> AttemptCoverageSummary {
    let mut summary = AttemptCoverageSummary {
        version: 1,
        scope: "stable-publishers",
        available: true,
        attempts: 0,
        passed_attempts: 0,
        failed_attempts: 0,
        skipped_short_tail_attempts: 0,
        missing_coverage_attempts: 0,
        requested_churners,
        validated_churners: 0,
    };
    let churner_start = clients.len().saturating_sub(requested_churners);
    for (index, client) in clients.iter().enumerate() {
        if client.connection_attempts.is_empty() {
            summary.available = false;
            failures.push(format!(
                "{}: no connection attempts recorded; coverage unavailable",
                client.client_id
            ));
        }
        let mut validated_churn = false;
        for (attempt_index, attempt) in client.connection_attempts.iter().enumerate() {
            summary.attempts += 1;
            let Some(coverage) = &attempt.coverage else {
                summary.available = false;
                summary.missing_coverage_attempts += 1;
                failures.push(format!(
                    "{}: attempt {} has no delivery coverage plan",
                    client.client_id,
                    attempt_index + 1
                ));
                continue;
            };
            if coverage.passed
                && !coverage.skipped_short_tail
                && coverage.failure_reasons.is_empty()
                && attempt.room_join_ms.is_some()
                && coverage.planned_eligible_seconds > 0
                && coverage.validated_audio >= coverage.expected_audio
                && coverage.validated_video >= coverage.expected_video
            {
                summary.passed_attempts += 1;
                if attempt_index > 0
                    && attempt.room_join_ms.is_some()
                    && coverage.planned_eligible_seconds > 0
                    && (coverage.expected_audio > 0 || coverage.expected_video > 0)
                {
                    validated_churn = true;
                }
            } else if coverage.skipped_short_tail
                && !coverage.passed
                && coverage.planned_eligible_seconds == 0
                && coverage.failure_reasons.is_empty()
                && attempt.room_join_ms.is_some()
            {
                summary.skipped_short_tail_attempts += 1;
            } else {
                summary.failed_attempts += 1;
                let reason = if coverage.failure_reasons.is_empty() {
                    "missing or inconsistent measured delivery evidence".to_string()
                } else {
                    coverage.failure_reasons.join("; ")
                };
                failures.push(format!(
                    "{}: attempt {} delivery coverage failed: {reason}",
                    client.client_id,
                    attempt_index + 1
                ));
            }
        }
        if index >= churner_start {
            if validated_churn {
                summary.validated_churners += 1;
            } else {
                failures.push(format!("{}: insufficient measured churn; no later joined, eligible, passing attempt against a stable publisher", client.client_id));
            }
        }
    }
    if requested_churners > clients.len() {
        summary.available = false;
        failures.push("Requested churners exceed available client evidence".into());
    }
    summary
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
            departure: Departure::Abrupt,
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
            "--departure" => {
                config.departure = Departure::parse(&args[i + 1])?;
                i += 2;
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
        "--departure" => {
            Departure::parse(value)?;
        }
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
    config.validate_departure()?;
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
    let num_churners = config.churner_count()?;
    std::fs::create_dir_all(&config.output_dir)?;
    let provenance = generator_provenance(&config)?;
    // Anchor reported wall time next to the monotonic workload schedule, after
    // provenance hashing. Cross-process phase correlation is still approximate:
    // wall-clock adjustments and this small capture gap are not measured here.
    let started_at = chrono::Utc::now().to_rfc3339();
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
    // Store each stable identity once, not once per client or churn attempt.
    // Empty rooms share a single empty set, with no allocation by room count.
    let stable_publishers =
        stable_publishers_by_room(num_publishers, churner_start_idx, config.num_rooms);
    let no_stable_publishers = Arc::new(HashSet::new());

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
            stable_publishers: Arc::clone(
                stable_publishers
                    .get(&(i % config.num_rooms.max(1)))
                    .unwrap_or(&no_stable_publishers),
            ),
            churn_session_min_secs: 5,
            churn_session_max_secs: 30,
            max_audio_consumers: config.max_audio_consumers,
            max_video_consumers: config.max_video_consumers,
            consume_existing_producers: true,
            departure: config.departure,
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
        "attemptCoverage": {
            "version": 1, "scope": "stable-publishers", "available": false,
            "attempts": 0, "passedAttempts": 0, "failedAttempts": 0,
            "skippedShortTailAttempts": 0, "missingCoverageAttempts": 0,
            "requestedChurners": config.churner_count()?, "validatedChurners": 0,
        },
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
    let attempt_coverage =
        gate_attempt_coverage(&all_metrics, config.churner_count()?, &mut failures);
    let passed = completed && failures.is_empty();
    let mut report = serde_json::to_value(&summary)?;
    report["schemaVersion"] = serde_json::json!(2);
    report["attemptCoverage"] = serde_json::to_value(attempt_coverage)?;
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
    // Freeze expectations before any signaling/consumer response can suppress a
    // missing attempt. Churn clones retain the same stable publisher cohort.
    let stable_peers = stable_peer_count(&config.stable_publishers, &client_id);
    let attempt = metrics.begin_planned_attempt(AttemptPlan {
        deadline: config.deadline,
        expected_audio: if config.media_config.audio_enabled {
            stable_peers.min(config.max_audio_consumers)
        } else {
            0
        },
        expected_video: if config.media_config.video_enabled {
            stable_peers.min(config.max_video_consumers)
        } else {
            0
        },
        stable_publishers: Arc::clone(&config.stable_publishers),
        is_publisher: config.is_publisher,
    });

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
                    metrics.record_publisher(&producer_id, true);
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
                    metrics.record_publisher(&producer_id, false);
                }
                ServerMessage::Error { message } => {
                    metrics.record_error(format!("Video producer failed: {}", message));
                }
                _ => {}
            }
        }
    }

    // Snapshot discovery precedes events buffered during setup. Replay both into
    // the same bounded queue, so a later close cancels its earlier discovery and
    // live producers cannot bypass older work when a subscription slot opens.
    existing_producer_events.extend(buffered_events);

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
                attempt,
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
    let (departure_request, departure_receiver) = if config.departure == Departure::ExplicitLeave {
        let (request, receiver) = tokio::sync::oneshot::channel();
        (Some(request), Some(receiver))
    } else {
        (None, None)
    };
    let receive_task = tokio::spawn(async move {
        receive_messages_loop(
            read,
            write,
            metrics_recv,
            client_id_recv,
            recv_timeout,
            rtp_caps_for_consume,
            webrtc_session_recv,
            max_audio,
            max_video,
            existing_producer_events,
            departure_receiver,
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
        metrics.diagnostic_event(
            "departure-started",
            serde_json::json!({"departure": config.departure}),
        );
        tracing::info!(
            event = "departure_started",
            departure = config.departure.as_str(),
            client_id = %client_id,
            "Client departure started after the session boundary and pre-close snapshot"
        );
    }
    if let Some(request) = departure_request
        && let Err(error) = request_explicit_leave(request, DEPARTURE_TIMEOUT).await
    {
        metrics.record_error(format!("Explicit leave failed: {error:#}"));
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

const DEPARTURE_TIMEOUT: Duration = Duration::from_secs(2);
type DepartureRequest = tokio::sync::oneshot::Sender<Result<()>>;

/// Request a write from the task that owns the signaling sink. This deadline
/// includes waiting for that task: a stalled negotiation cannot delay cleanup
/// indefinitely. Success confirms a local write, not server LeaveRoom handling;
/// the signaling protocol has no leave acknowledgment.
async fn request_explicit_leave(
    request: tokio::sync::oneshot::Sender<DepartureRequest>,
    limit: Duration,
) -> Result<()> {
    let (completion, result) = tokio::sync::oneshot::channel();
    request
        .send(completion)
        .map_err(|_| anyhow::anyhow!("Signaling task unavailable for explicit leave"))?;
    tokio::time::timeout(limit, result)
        .await
        .context("Explicit leave write timed out")?
        .context("Signaling task stopped before completing explicit leave")?
}

/// Flush exactly the protocol leave message before returning the local write
/// result. Keep the native WebRTC peers open until the caller gets this result
/// (or its enclosing command deadline expires).
async fn send_explicit_leave<S>(write: &mut S, limit: Duration) -> Result<()>
where
    S: futures_util::Sink<Message, Error = tokio_tungstenite::tungstenite::Error> + Unpin,
{
    let message = serde_json::to_string(&ClientMessage::LeaveRoom)?;
    tokio::time::timeout(limit, write.send(Message::Text(message.into())))
        .await
        .context("Explicit leave write timed out")?
        .context("Explicit leave signaling write failed")
}

async fn send_real_media_loop(
    webrtc_session: Arc<Mutex<WebRtcSession>>,
    mut media_gen: MediaGenerator,
    config: MediaConfig,
    metrics: Arc<MetricsCollector>,
    client_id: String,
    attempt: usize,
) {
    tracing::debug!("{}: Starting REAL media send loop", client_id);

    // Get tracks from WebRTC session
    let (audio_track, video_track, keyframe_requests, ssrcs) = {
        let session = webrtc_session.lock().await;
        (
            session.audio_track(),
            session.video_track(),
            session.video_keyframe_requests(),
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
    let Some(keyframe_requests) = keyframe_requests else {
        metrics.record_error_for_attempt(
            attempt,
            "Cannot send without owned keyframe request state".into(),
        );
        return;
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
                            metrics.record_packet_sent_for_attempt(attempt, packet_bytes.len());
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
                    let frame = media_gen.generate_video_frame(&keyframe_requests);
                    let mut send_error = false;
                    for packet_bytes in frame.packets {
                        let packet = negotiated_rtp_packet(&packet_bytes, video_ssrc);
                        let result = match packet {
                            Ok(packet) => track.write_rtp(packet).await,
                            Err(error) => Err(error),
                        };
                        match result {
                            Ok(_) => {
                                metrics.record_packet_sent_for_attempt(attempt, packet_bytes.len());
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
                    if frame.is_keyframe && metrics.diagnostics_enabled() {
                        metrics.diagnostic_event_for_attempt(attempt, "video-keyframe-queued", serde_json::json!({
                            "ssrc": video_ssrc,
                            "frameIndex": frame.frame_index,
                            "rtpTimestamp": frame.rtp_timestamp,
                            "requested": frame.requested,
                        }));
                    }
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
mod attempt_coverage_gate_tests {
    use super::*;

    fn attempt(passed: bool) -> metrics::ConnectionAttempt {
        serde_json::from_value(serde_json::json!({
            "roomJoinMs": 1,
            "sendMediaReadyMs": 2,
            "receiveMediaReadyMs": 2,
            "coverage": {
                "plannedEligibleSeconds": 5,
                "expectedAudio": 1, "expectedVideo": 1,
                "validatedAudio": usize::from(passed),
                "validatedVideo": usize::from(passed),
                "packetsQueued": 10,
                "passed": passed, "skippedShortTail": false,
                "failureReasons": if passed { Vec::<String>::new() } else { vec!["No media in this attempt".into()] },
            },
        })).unwrap()
    }

    fn client(id: usize, attempts: Vec<metrics::ConnectionAttempt>) -> ClientMetrics {
        let mut report = MetricsCollector::new(format!("client-{id}")).generate_report();
        report.connection_attempts = attempts;
        report
    }

    #[test]
    fn positive_churn_must_select_at_least_one_client() {
        let mut config = TestConfig {
            num_clients: 3,
            duration_secs: 120,
            ..TestConfig::default()
        };
        assert_eq!(config.churner_count().unwrap(), 0);
        config.churn_rate = 1.0 / 120.0;
        assert_eq!(config.churner_count().unwrap(), 1);
        config.churn_rate = 100.0;
        assert_eq!(config.churner_count().unwrap(), 3);
        for invalid in [0.5 / 120.0, -1.0, f64::NAN, f64::INFINITY] {
            config.churn_rate = invalid;
            assert!(config.churner_count().is_err());
        }
    }

    #[test]
    fn room_publisher_sets_are_shared_and_peer_counts_exclude_self() {
        let rooms = stable_publishers_by_room(5, 4, 2);
        assert_eq!(rooms.len(), 2);
        assert_eq!(
            rooms
                .values()
                .map(|publishers| publishers.len())
                .sum::<usize>(),
            4
        );
        let room = rooms.get(&0).unwrap();
        assert_eq!(
            room.as_ref(),
            &HashSet::from(["client-0".into(), "client-2".into()])
        );
        assert_eq!(stable_peer_count(room, "client-4"), 2);
        assert_eq!(stable_peer_count(room, "client-2"), 1);
        let another_client = Arc::clone(room);
        let later_attempt = Arc::clone(&another_client);
        assert!(Arc::ptr_eq(room, &another_client));
        assert!(Arc::ptr_eq(room, &later_attempt));
        assert!(
            !rooms
                .values()
                .any(|publishers| publishers.contains("client-4"))
        );
        assert_eq!(stable_peer_count(rooms.get(&1).unwrap(), "client-3"), 1);
        assert!(stable_publishers_by_room(3, 0, 1).is_empty());
        assert_eq!(stable_publishers_by_room(3, 3, usize::MAX).len(), 3);
    }

    #[test]
    fn later_attempt_cannot_borrow_initial_delivery_success() {
        let mut failures = Vec::new();
        let summary = gate_attempt_coverage(
            &[client(0, vec![attempt(true), attempt(false)])],
            1,
            &mut failures,
        );
        assert_eq!(summary.passed_attempts, 1);
        assert_eq!(summary.failed_attempts, 1);
        assert_eq!(summary.validated_churners, 0);
        assert!(
            failures
                .iter()
                .any(|reason| reason.contains("attempt 2 delivery coverage failed"))
        );
        assert!(
            failures
                .iter()
                .any(|reason| reason.contains("insufficient measured churn"))
        );
    }

    #[test]
    fn later_eligible_joined_delivery_validates_a_churner() {
        let mut failures = Vec::new();
        let summary = gate_attempt_coverage(
            &[client(0, vec![attempt(true), attempt(true)])],
            1,
            &mut failures,
        );
        assert!(failures.is_empty());
        assert_eq!(summary.validated_churners, 1);
        let json = serde_json::to_value(summary).unwrap();
        assert_eq!(json["version"], 1);
        assert_eq!(json["scope"], "stable-publishers");
        assert_eq!(json["attempts"], 2);
        assert_eq!(json["requestedChurners"], 1);
        assert_eq!(json["validatedChurners"], 1);
    }

    #[test]
    fn planned_short_tail_is_not_measured_churn_or_an_error_excuse() {
        let mut tail = attempt(false);
        let coverage = tail.coverage.as_mut().unwrap();
        coverage.planned_eligible_seconds = 0;
        coverage.skipped_short_tail = true;
        coverage.failure_reasons.clear();
        let mut failures = Vec::new();
        let summary = gate_attempt_coverage(
            &[client(0, vec![attempt(true), tail.clone()])],
            1,
            &mut failures,
        );
        assert_eq!(summary.skipped_short_tail_attempts, 1);
        assert_eq!(summary.failed_attempts, 0);
        assert_eq!(summary.validated_churners, 0);
        tail.coverage
            .as_mut()
            .unwrap()
            .failure_reasons
            .push("Setup failed".into());
        let mut failures = Vec::new();
        let summary =
            gate_attempt_coverage(&[client(0, vec![attempt(true), tail])], 1, &mut failures);
        assert_eq!(summary.skipped_short_tail_attempts, 0);
        assert_eq!(summary.failed_attempts, 1);
    }

    #[test]
    fn missing_coverage_and_unjoined_or_receiver_free_repeats_do_not_pass() {
        let mut missing = attempt(true);
        missing.coverage = None;
        let mut failures = Vec::new();
        let summary = gate_attempt_coverage(&[client(0, vec![missing])], 0, &mut failures);
        assert!(!summary.available);
        assert_eq!(summary.missing_coverage_attempts, 1);
        assert!(!failures.is_empty());
        for receiver_free in [false, true] {
            let mut later = attempt(true);
            if receiver_free {
                let coverage = later.coverage.as_mut().unwrap();
                coverage.expected_audio = 0;
                coverage.expected_video = 0;
            } else {
                later.room_join_ms = None;
            }
            let mut failures = Vec::new();
            let summary =
                gate_attempt_coverage(&[client(0, vec![attempt(true), later])], 1, &mut failures);
            assert_eq!(summary.validated_churners, 0);
            assert!(
                failures
                    .iter()
                    .any(|reason| reason.contains("insufficient measured churn"))
            );
        }
    }

    #[test]
    fn every_selected_churner_needs_its_own_measured_repeat() {
        let mut failures = Vec::new();
        let summary = gate_attempt_coverage(
            &[
                client(0, vec![attempt(true), attempt(true)]),
                client(1, vec![attempt(true)]),
            ],
            2,
            &mut failures,
        );
        assert_eq!(summary.validated_churners, 1);
        assert!(
            failures
                .iter()
                .any(|reason| reason.starts_with("client-1: insufficient measured churn"))
        );
    }

    #[test]
    fn contradictory_pass_and_skip_fields_fail_closed() {
        for inconsistent in [
            "zero-window",
            "missing-audio",
            "missing-video",
            "unjoined",
            "passed-and-skipped",
        ] {
            let mut later = attempt(true);
            let coverage = later.coverage.as_mut().unwrap();
            match inconsistent {
                "zero-window" => coverage.planned_eligible_seconds = 0,
                "missing-audio" => coverage.validated_audio = 0,
                "missing-video" => coverage.validated_video = 0,
                "unjoined" => later.room_join_ms = None,
                "passed-and-skipped" => coverage.skipped_short_tail = true,
                _ => unreachable!(),
            }
            let mut failures = Vec::new();
            let summary =
                gate_attempt_coverage(&[client(0, vec![attempt(true), later])], 1, &mut failures);
            assert_eq!(summary.passed_attempts, 1, "{inconsistent}");
            assert_eq!(summary.failed_attempts, 1, "{inconsistent}");
            assert_eq!(summary.validated_churners, 0, "{inconsistent}");
            assert!(
                failures
                    .iter()
                    .any(|reason| reason.contains("inconsistent measured delivery")),
                "{inconsistent}"
            );
        }
        let mut tail = attempt(false);
        tail.room_join_ms = None;
        let coverage = tail.coverage.as_mut().unwrap();
        coverage.planned_eligible_seconds = 0;
        coverage.skipped_short_tail = true;
        coverage.failure_reasons.clear();
        let mut failures = Vec::new();
        let summary =
            gate_attempt_coverage(&[client(0, vec![attempt(true), tail])], 1, &mut failures);
        assert_eq!(summary.skipped_short_tail_attempts, 0);
        assert_eq!(summary.failed_attempts, 1);
    }
}

#[cfg(test)]
mod packet_migration_tests {
    use super::*;

    #[test]
    fn producer_video_advertises_supported_feedback_without_changing_audio() {
        let audio = extract_rtp_parameters(MediaKind::Audio, 12_345, 1000);
        let video = extract_rtp_parameters(MediaKind::Video, 54_321, 1000);
        let RtpCodecParameters::Audio { rtcp_feedback, .. } = &audio.codecs[0] else {
            panic!("audio codec expected");
        };
        assert!(rtcp_feedback.is_empty());
        let RtpCodecParameters::Video { rtcp_feedback, .. } = &video.codecs[0] else {
            panic!("video codec expected");
        };
        assert_eq!(
            rtcp_feedback,
            &[
                RtcpFeedback::Nack,
                RtcpFeedback::NackPli,
                RtcpFeedback::CcmFir
            ]
        );
        assert_eq!(video.encodings[0].ssrc, Some(54_321));
        assert_eq!(video.encodings[0].max_bitrate, Some(1_000_000));
    }

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
        let mut video = generator
            .generate_video_frame(&media_generator::KeyframeRequests::default())
            .packets;
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

/// Signaling metadata remains pending until its live SDP batch is installed.
/// A server closure before then must not create an unnecessary transceiver.
struct PendingConsumer {
    consumer_id: String,
    producer_id: String,
    kind: MediaKind,
    rtp_parameters: RtpParameters,
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
    max_audio: usize,
    max_video: usize,
    existing_producer_events: Vec<ServerMessage>,
    mut departure_receiver: Option<tokio::sync::oneshot::Receiver<DepartureRequest>>,
) {
    let deadline = tokio::time::Instant::now() + timeout;
    let mut needs_renegotiation = false;
    let mut pending_resumes: Vec<PendingConsumer> = Vec::new();
    let mut renegotiation_since: Option<tokio::time::Instant> = None;
    let mut total_resumed: usize = 0;

    let mut subscriptions = Subscriptions::new(max_audio, max_video);
    for event in existing_producer_events {
        handle_server_message(
            event,
            &metrics,
            &client_id,
            &mut needs_renegotiation,
            &mut pending_resumes,
            &mut subscriptions,
        )
        .await;
    }
    // This is a work timer, separate from the signaling-idle coalescing timer.
    // A ready queue gets an immediate first turn without a continuously ready
    // branch that could drain every producer or repeatedly renegotiate SDP.
    let mut next_subscription_work = tokio::time::Instant::now();
    let attempt = metrics.diagnostic_attempt();

    while tokio::time::Instant::now() < deadline {
        let got_message = tokio::select! {
            request = async {
                match departure_receiver.as_mut() {
                    Some(receiver) => receiver.await,
                    None => std::future::pending().await,
                }
            }, if departure_receiver.is_some() => {
                match request {
                    Ok(completion) => {
                        let result = send_explicit_leave(&mut write, DEPARTURE_TIMEOUT).await;
                        let _ = completion.send(result);
                        break;
                    }
                    Err(_) => {
                        departure_receiver = None;
                        continue;
                    }
                }
            }
            msg = read.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        match serde_json::from_str::<ServerMessage>(&text) {
                            Ok(server_msg) => {
                                handle_server_message(
                                    server_msg,
                                    &metrics,
                                    &client_id,
                                    &mut needs_renegotiation,
                                    &mut pending_resumes,
                                    &mut subscriptions,
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
            _ = tokio::time::sleep_until(next_subscription_work), if subscriptions.has_work() => {
                if !metrics.attempt_accepts_work(attempt) {
                    subscriptions.stop();
                    continue;
                }
                for _ in 0..2 {
                    if !metrics.attempt_accepts_work(attempt) {
                        subscriptions.stop();
                        break;
                    }
                    let Some(request) = subscriptions.next_request(|id| metrics.producer_retired(id)) else {
                        continue;
                    };
                    if !metrics.subscribe(&request.producer_id, request.kind == MediaKind::Audio) {
                        metrics.record_error("Subscription state attempted a duplicate Consume".into());
                        subscriptions.stop();
                        break;
                    }
                    if metrics.diagnostics_enabled() {
                        metrics.diagnostic_event("consume-requested", serde_json::json!({
                            "producerId": request.producer_id, "kind": request.kind,
                        }));
                    }
                    if let Err(error) = send_message(&mut write, ClientMessage::Consume {
                        producer_id: request.producer_id,
                        rtp_capabilities: rtp_capabilities.clone(),
                    }).await {
                        // A failed write has ambiguous remote receipt. Keep the
                        // reservation and stop, never release-and-retry it.
                        metrics.record_error(format!("Consume request failed: {error}"));
                        subscriptions.stop();
                        break;
                    }
                }
                next_subscription_work = tokio::time::Instant::now() + Duration::from_millis(100);
                true
            }
            _ = tokio::time::sleep(Duration::from_millis(2000)) => {
                false
            }
        };

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
            if !metrics.attempt_accepts_work(attempt) {
                subscriptions.stop();
            }
            pending_resumes.retain(|consumer| subscriptions.can_resume(&consumer.consumer_id));
            if pending_resumes.is_empty() {
                needs_renegotiation = false;
                renegotiation_since = None;
                continue;
            }
            let applied = {
                let mut session = webrtc_session.lock().await;
                if !metrics.attempt_accepts_work(attempt) {
                    subscriptions.stop();
                    continue;
                }
                let mut recorded = Ok(());
                for consumer in &pending_resumes {
                    recorded = session.record_consumer(
                        consumer.producer_id.clone(),
                        consumer.kind,
                        &consumer.rtp_parameters,
                    );
                    if recorded.is_err() {
                        break;
                    }
                }
                match recorded {
                    Ok(()) => session.renegotiate_consumers().await,
                    Err(error) => Err(error),
                }
            };
            // Single renegotiation for all still-live consumers in this batch.
            if let Err(e) = applied {
                metrics.record_error(format!("Consumer renegotiation failed: {e}"));
                tracing::error!("{}: Failed to renegotiate consumers: {}", client_id, e);
                // The pending batch has no installed receive description. Stop
                // signaling receipt without draining it or asking for media.
                break;
            } else if metrics.diagnostics_enabled() {
                metrics.diagnostic_event(
                    "renegotiation-applied",
                    serde_json::json!({"consumerCount": pending_resumes.len()}),
                );
            }

            // Snapshot the installed batch before reading more signaling. A
            // closure during settling cannot invalidate a later drain range,
            // and newly created consumers still need their own SDP batch.
            let to_resume = std::mem::take(&mut pending_resumes);
            let batch_size = to_resume.len();

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
                                        &mut needs_renegotiation,
                                        &mut pending_resumes,
                                        &mut subscriptions,
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
            if !metrics.attempt_accepts_work(attempt) {
                subscriptions.stop();
            }
            let mut resume_count = 0;
            for consumer in to_resume
                .into_iter()
                .filter(|consumer| subscriptions.can_resume(&consumer.consumer_id))
            {
                if !metrics.attempt_accepts_work(attempt) {
                    break;
                }
                let consumer_id = consumer.consumer_id;
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
                resume_count += 1;
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

async fn handle_server_message(
    msg: ServerMessage,
    metrics: &Arc<MetricsCollector>,
    client_id: &str,
    needs_renegotiation: &mut bool,
    pending_resumes: &mut Vec<PendingConsumer>,
    subscriptions: &mut Subscriptions,
) {
    match msg {
        ServerMessage::NewProducer {
            participant_id: _,
            producer_id,
            kind,
            ..
        } => {
            let retired = metrics.producer_retired(&producer_id);
            if let Err(error) = subscriptions.discover(producer_id, kind, retired) {
                metrics.record_error(format!("Producer discovery failed: {error}"));
            }
        }
        ServerMessage::ConsumerCreated {
            consumer_id,
            producer_id,
            kind,
            rtp_parameters,
        } => {
            if !metrics.attempt_accepts_work(metrics.diagnostic_attempt()) {
                subscriptions.stop();
                return;
            }
            match subscriptions.created(&producer_id, kind, &consumer_id) {
                Ok(true) => {}
                Ok(false) => return,
                Err(error) => {
                    metrics.record_error(format!("Invalid consumer response: {error}"));
                    subscriptions.stop();
                    return;
                }
            }
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

            pending_resumes.push(PendingConsumer {
                consumer_id,
                producer_id,
                kind,
                rtp_parameters,
            });
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
            if let Err(error) = subscriptions.close(&producer_id) {
                metrics.record_error(format!("Producer closure failed: {error}"));
            }
            pending_resumes.retain(|consumer| subscriptions.can_resume(&consumer.consumer_id));
            let (_, unexpected) = metrics.close_producer(&producer_id);
            if unexpected {
                metrics.record_error(format!("Server closed active generated producer {producer_id} before its planned lifetime ended"));
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
                rtcp_feedback: vec![
                    RtcpFeedback::Nack,
                    RtcpFeedback::NackPli,
                    RtcpFeedback::CcmFir,
                ],
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
    println!(
        "  --departure <MODE>         abrupt (default) or explicit-leave (requires --diagnostics)"
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

#[cfg(test)]
mod departure_tests {
    use super::*;

    #[test]
    fn cli_departure_is_validated_and_explicit_leave_requires_diagnostics() {
        for value in ["abrupt", "explicit-leave"] {
            let args = ["load_test".into(), "--departure".into(), value.into()];
            assert!(validate_cli_value(&args, 1).is_ok());
        }
        for values in [
            vec!["load_test", "--departure"],
            vec!["load_test", "--departure", "leave"],
            vec!["load_test", "--departure", "--diagnostics"],
        ] {
            let args = values.into_iter().map(str::to_string).collect::<Vec<_>>();
            assert!(validate_cli_value(&args, 1).is_err());
        }
        let mut config = TestConfig::default();
        assert_eq!(config.departure, Departure::Abrupt);
        assert!(config.validate_departure().is_ok());
        assert_eq!(
            serde_json::to_value(&config).unwrap()["departure"],
            "abrupt"
        );
        config.departure = Departure::ExplicitLeave;
        assert!(config.validate_departure().is_err());
        config.diagnostics = true;
        assert!(config.validate_departure().is_ok());
        assert_eq!(
            serde_json::to_value(&config).unwrap()["departure"],
            "explicit-leave"
        );
    }

    #[tokio::test]
    async fn explicit_leave_uses_owned_signaling_writer_before_socket_drop() -> Result<()> {
        tokio::time::timeout(Duration::from_secs(3), async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
            let address = listener.local_addr()?;
            let (client, server) = tokio::join!(connect_async(format!("ws://{address}/")), async {
                let (stream, _) = listener.accept().await?;
                Ok::<_, anyhow::Error>(tokio_tungstenite::accept_async(stream).await?)
            });
            let (client, _) = client?;
            let mut server = server?;
            let (mut write, read) = client.split();
            send_message(&mut write, ClientMessage::GetRouterRtpCapabilities).await?;
            let metrics = Arc::new(MetricsCollector::new("departure-test".into()));
            let session = Arc::new(Mutex::new(WebRtcSession::new(
                "departure-test".into(),
                metrics.clone(),
            )));
            let (request, receiver) = tokio::sync::oneshot::channel();
            // JoinSet aborts the owned receive loop on assertion/timeout as well.
            let mut tasks = tokio::task::JoinSet::new();
            tasks.spawn(receive_messages_loop(
                read,
                write,
                metrics.clone(),
                "departure-test".into(),
                Duration::from_secs(10),
                RtpCapabilities::default(),
                session,
                4,
                4,
                Vec::new(),
                Some(receiver),
            ));
            request_explicit_leave(request, Duration::from_secs(1)).await?;
            // No server response is sent: the completion must be a write
            // result, and cannot be mistaken for a protocol acknowledgment.
            for expected in [
                ClientMessage::GetRouterRtpCapabilities,
                ClientMessage::LeaveRoom,
            ] {
                let frame = server.next().await.context("Missing signaling frame")??;
                let Message::Text(text) = frame else {
                    anyhow::bail!("Socket closed before the expected signaling message");
                };
                assert_eq!(
                    serde_json::from_str::<serde_json::Value>(&text)?,
                    serde_json::to_value(expected)?
                );
            }
            tasks.join_next().await.context("Missing receive task")??;
            assert!(metrics.generate_report().errors.is_empty());
            Ok::<_, anyhow::Error>(())
        })
        .await?
    }

    #[tokio::test]
    async fn explicit_leave_command_timeout_and_task_loss_are_reported() {
        let (request, receiver) = tokio::sync::oneshot::channel();
        let started = Instant::now();
        let error = request_explicit_leave(request, Duration::from_millis(10))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("timed out"));
        assert!(started.elapsed() < Duration::from_secs(1));
        drop(receiver);

        let (request, receiver) = tokio::sync::oneshot::channel();
        drop(receiver);
        assert!(
            request_explicit_leave(request, Duration::from_secs(1))
                .await
                .unwrap_err()
                .to_string()
                .contains("unavailable")
        );

        let (request, receiver) = tokio::sync::oneshot::channel();
        let (result, ()) = tokio::join!(
            request_explicit_leave(request, Duration::from_secs(1)),
            async { drop(receiver.await.unwrap()) },
        );
        assert!(result.unwrap_err().to_string().contains("stopped"));
    }

    #[tokio::test]
    async fn explicit_leave_write_timeout_and_transport_failure_are_reported() {
        let stalled = futures_util::sink::unfold((), |(), _: Message| {
            std::future::pending::<std::result::Result<(), tokio_tungstenite::tungstenite::Error>>()
        });
        tokio::pin!(stalled);
        let started = Instant::now();
        assert!(
            send_explicit_leave(&mut stalled, Duration::from_millis(10))
                .await
                .unwrap_err()
                .to_string()
                .contains("timed out")
        );
        assert!(started.elapsed() < Duration::from_secs(1));

        let failed = futures_util::sink::unfold((), |(), _: Message| async {
            Err::<(), _>(tokio_tungstenite::tungstenite::Error::ConnectionClosed)
        });
        tokio::pin!(failed);
        assert!(
            send_explicit_leave(&mut failed, Duration::from_secs(1))
                .await
                .unwrap_err()
                .to_string()
                .contains("write failed")
        );
    }
}

#[cfg(test)]
mod incremental_receive_tests {
    use super::*;
    use futures_util::FutureExt;
    use mediasoup_types::data_structures::{DtlsFingerprint, DtlsRole, IceCandidateType};
    use std::panic::AssertUnwindSafe;
    use tokio::task::JoinHandle;

    /// Abort on unwind/timeout as well as the normal explicit cleanup path.
    struct OwnedTask(Option<JoinHandle<()>>);

    impl OwnedTask {
        fn abort(&self) {
            if let Some(task) = &self.0 {
                task.abort();
            }
        }

        async fn finish(&mut self) -> Result<()> {
            if let Some(task) = self.0.as_mut() {
                let result = task.await;
                self.0.take();
                if let Err(error) = result {
                    anyhow::ensure!(error.is_cancelled(), "Fixture task failed: {error}");
                }
            }
            Ok(())
        }
    }

    impl Drop for OwnedTask {
        fn drop(&mut self) {
            self.abort();
        }
    }

    async fn exercise_incremental_batch(closed_peer: bool) {
        let metrics = Arc::new(MetricsCollector::new("incremental-receiver".into()));
        metrics.enable_diagnostics();
        metrics.begin_connection_attempt();
        let session = Arc::new(Mutex::new(WebRtcSession::new(
            "incremental-receiver".into(),
            metrics.clone(),
        )));
        let mut tasks = Vec::<OwnedTask>::new();

        // Keep cleanup outside the bounded body so a failed assertion or a
        // timeout cannot detach the receive loop or leave its real peer open.
        let outcome = AssertUnwindSafe(tokio::time::timeout(Duration::from_secs(10), async {
            let udp = tokio::net::UdpSocket::bind("127.0.0.1:0").await?;
            let udp_port = udp.local_addr()?.port();
            tasks.push(OwnedTask(Some(tokio::spawn(async move {
                let mut packet = [0; 2048];
                while udp.recv_from(&mut packet).await.is_ok() {}
            }))));
            session
                .lock()
                .await
                .create_recv_transport(
                    "incremental-recv-transport".into(),
                    IceParameters {
                        username_fragment: "testufrag".into(),
                        password: "test-password-at-least-twenty-two-bytes".into(),
                        ice_lite: Some(true),
                    },
                    vec![IceCandidate {
                        foundation: "owned-loopback".into(),
                        priority: 2_130_706_431,
                        address: "127.0.0.1".into(),
                        protocol: Protocol::Udp,
                        port: udp_port,
                        r#type: IceCandidateType::Host,
                        tcp_type: None,
                    }],
                    DtlsParameters {
                        role: DtlsRole::Server,
                        fingerprints: vec![DtlsFingerprint::Sha256 { value: [0x42; 32] }],
                    },
                )
                .await?;
            if closed_peer {
                // close(&self) retains the transport handle: record_consumer
                // succeeds, but the real peer rejects SDP renegotiation.
                session.lock().await.close().await?;
            }

            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
            let address = listener.local_addr()?;
            let (client, server) = tokio::join!(connect_async(format!("ws://{address}/")), async {
                let (stream, _) = listener.accept().await?;
                Ok::<_, anyhow::Error>(tokio_tungstenite::accept_async(stream).await?)
            },);
            let (client, _) = client?;
            let mut server = server?;
            drop(listener);
            let (write, read) = client.split();
            tasks.push(OwnedTask(Some(tokio::spawn(receive_messages_loop(
                read,
                write,
                metrics.clone(),
                "incremental-receiver".into(),
                Duration::from_secs(30),
                RtpCapabilities::default(),
                session.clone(),
                4,
                4,
                vec![ServerMessage::NewProducer {
                    participant_id: "batch-publisher".into(),
                    producer_id: "batch-producer".into(),
                    kind: MediaKind::Audio,
                    source: None,
                }],
                None,
            )))));

            let request = server.next().await.context("Missing Consume request")??;
            let Message::Text(request) = request else {
                anyhow::bail!("Expected a text Consume request");
            };
            anyhow::ensure!(matches!(serde_json::from_str::<ClientMessage>(&request)?,
                ClientMessage::Consume { producer_id, .. } if producer_id == "batch-producer"));
            server
                .send(Message::Text(
                    serde_json::to_string(&ServerMessage::ConsumerCreated {
                        consumer_id: "batch-consumer".into(),
                        producer_id: "batch-producer".into(),
                        kind: MediaKind::Audio,
                        rtp_parameters: RtpParameters {
                            encodings: vec![RtpEncodingParameters {
                                ssrc: Some(123_456),
                                ..Default::default()
                            }],
                            ..Default::default()
                        },
                    })?
                    .into(),
                ))
                .await?;

            if closed_peer {
                // The loop's own deadline is longer than the enclosing test
                // deadline: reaching this point proves the failure exited it.
                tokio::try_join!(tasks[1].finish(), async {
                    while let Some(Ok(message)) = server.next().await {
                        if let Message::Text(text) = message {
                            let message: ClientMessage = serde_json::from_str(&text)?;
                            anyhow::ensure!(
                                !matches!(message, ClientMessage::ResumeConsumer { .. }),
                                "Failed renegotiation must not send ResumeConsumer",
                            );
                        }
                    }
                    Ok::<_, anyhow::Error>(())
                })?;
            } else {
                loop {
                    let message = server
                        .next()
                        .await
                        .context("Receiver closed before resume")??;
                    if let Message::Text(text) = message {
                        let message: ClientMessage = serde_json::from_str(&text)?;
                        if let ClientMessage::ResumeConsumer { consumer_id } = message {
                            anyhow::ensure!(consumer_id == "batch-consumer");
                            break;
                        }
                    }
                }
                // Stop the live receive loop before dropping the scripted
                // server, which would otherwise record an unrelated WS error.
                tasks[1].abort();
                tasks[1].finish().await?;
            }

            let report = metrics.generate_report();
            let events = &report
                .diagnostics
                .as_ref()
                .context("Missing diagnostics")?
                .events;
            anyhow::ensure!(report.consumers_created == 1);
            anyhow::ensure!(events.iter().any(|event| event.kind == "consumer-created"));
            if closed_peer {
                anyhow::ensure!(
                    report.errors.len() == 1,
                    "Unexpected errors: {:?}",
                    report.errors
                );
                anyhow::ensure!(report.errors[0].starts_with("Consumer renegotiation failed:"));
                anyhow::ensure!(
                    !events
                        .iter()
                        .any(|event| event.kind == "renegotiation-applied")
                );
                anyhow::ensure!(!events.iter().any(|event| event.kind == "resume-requested"));
                anyhow::ensure!(TestSummary::from_metrics(&[report]).total_errors == 1);
            } else {
                anyhow::ensure!(
                    report.errors.is_empty(),
                    "Unexpected errors: {:?}",
                    report.errors
                );
                let applied = events
                    .iter()
                    .position(|event| event.kind == "renegotiation-applied")
                    .context("Missing applied-renegotiation evidence")?;
                let resumed = events
                    .iter()
                    .position(|event| event.kind == "resume-requested")
                    .context("Missing resume evidence")?;
                anyhow::ensure!(applied < resumed);
                anyhow::ensure!(events[resumed].details["consumerId"] == "batch-consumer");
            }
            Ok::<_, anyhow::Error>(())
        }))
        .catch_unwind()
        .await;

        for task in &tasks {
            task.abort();
        }
        let cleanup = tokio::time::timeout(Duration::from_secs(3), async {
            let mut task_error = None;
            for task in &mut tasks {
                if let Err(error) = task.finish().await {
                    task_error = Some(error);
                }
            }
            let closed = session.lock().await.close().await;
            closed?;
            if let Some(error) = task_error {
                return Err(error);
            }
            Ok::<_, anyhow::Error>(())
        })
        .await;

        match outcome {
            Ok(result) => result
                .expect("Incremental receive test timed out")
                .expect("Incremental receive control flow failed"),
            Err(panic) => std::panic::resume_unwind(panic),
        }
        cleanup
            .expect("Fixture cleanup timed out")
            .expect("Fixture cleanup failed");
    }

    #[tokio::test]
    async fn closed_peer_renegotiation_exits_without_resuming_pending_consumers() {
        exercise_incremental_batch(true).await;
    }

    #[tokio::test]
    async fn applied_incremental_renegotiation_resumes_its_pending_batch() {
        exercise_incremental_batch(false).await;
    }
}

#[cfg(test)]
mod subscription_loop_tests {
    use super::*;

    struct Fixture {
        server: tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
        metrics: Arc<MetricsCollector>,
        tasks: tokio::task::JoinSet<()>,
        departure: tokio::sync::oneshot::Sender<DepartureRequest>,
    }

    fn producer(id: &str, kind: MediaKind) -> ServerMessage {
        ServerMessage::NewProducer {
            participant_id: "scripted-publisher".into(),
            producer_id: id.into(),
            kind,
            source: None,
        }
    }

    impl Fixture {
        async fn new(
            events: Vec<ServerMessage>,
            max_audio: usize,
            max_video: usize,
        ) -> Result<Self> {
            let metrics = Arc::new(MetricsCollector::new("queue-receiver".into()));
            metrics.begin_connection_attempt();
            metrics.enable_diagnostics();
            let session = Arc::new(Mutex::new(WebRtcSession::new(
                "queue-receiver".into(),
                metrics.clone(),
            )));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
            let address = listener.local_addr()?;
            let (client, server) = tokio::join!(connect_async(format!("ws://{address}/")), async {
                let (stream, _) = listener.accept().await?;
                Ok::<_, anyhow::Error>(tokio_tungstenite::accept_async(stream).await?)
            });
            let (client, _) = client?;
            let server = server?;
            let (write, read) = client.split();
            let (departure, receiver) = tokio::sync::oneshot::channel();
            let mut tasks = tokio::task::JoinSet::new();
            tasks.spawn(receive_messages_loop(
                read,
                write,
                metrics.clone(),
                "queue-receiver".into(),
                Duration::from_secs(10),
                RtpCapabilities::default(),
                session,
                max_audio,
                max_video,
                events,
                Some(receiver),
            ));
            Ok(Self {
                server,
                metrics,
                tasks,
                departure,
            })
        }

        async fn read(&mut self) -> Result<ClientMessage> {
            let message = tokio::time::timeout(Duration::from_secs(1), self.server.next())
                .await?
                .context("Receiver closed unexpectedly")??;
            let Message::Text(text) = message else {
                anyhow::bail!("Expected text signaling");
            };
            Ok(serde_json::from_str(&text)?)
        }

        async fn consume(&mut self, expected: &str) -> Result<()> {
            anyhow::ensure!(
                matches!(self.read().await?, ClientMessage::Consume { producer_id, .. }
                if producer_id == expected),
                "Unexpected queued request"
            );
            Ok(())
        }

        async fn send(&mut self, message: ServerMessage) -> Result<()> {
            self.server
                .send(Message::Text(serde_json::to_string(&message)?.into()))
                .await?;
            Ok(())
        }

        async fn finish(self) -> Result<()> {
            let Self {
                mut server,
                metrics,
                mut tasks,
                departure,
            } = self;
            request_explicit_leave(departure, Duration::from_secs(1)).await?;
            let message = server.next().await.context("Missing LeaveRoom")??;
            let Message::Text(text) = message else {
                anyhow::bail!("Expected LeaveRoom text");
            };
            anyhow::ensure!(matches!(
                serde_json::from_str::<ClientMessage>(&text)?,
                ClientMessage::LeaveRoom
            ));
            tasks.join_next().await.context("Missing receive task")??;
            anyhow::ensure!(metrics.generate_report().errors.is_empty());
            Ok(())
        }
    }

    #[tokio::test]
    async fn quiet_discovery_starts_promptly_and_keeps_two_item_work_budget() -> Result<()> {
        tokio::time::timeout(Duration::from_secs(5), async {
            let mut fixture = Fixture::new(
                vec![
                    producer("a1", MediaKind::Audio),
                    producer("a2", MediaKind::Audio),
                    producer("v1", MediaKind::Video),
                    producer("v2", MediaKind::Video),
                ],
                4,
                4,
            )
            .await?;
            for id in ["a1", "v1", "a2", "v2"] {
                fixture.consume(id).await?;
            }
            let report = fixture.metrics.generate_report();
            let events: Vec<_> = report
                .diagnostics
                .as_ref()
                .unwrap()
                .events
                .iter()
                .filter(|event| event.kind == "consume-requested")
                .collect();
            anyhow::ensure!(events.len() == 4);
            anyhow::ensure!(
                events[2].elapsed_ms.saturating_sub(events[1].elapsed_ms) >= 90,
                "A work tick exceeded its two-item budget"
            );
            anyhow::ensure!(!report.diagnostics.as_ref().unwrap().events.iter().any(
                |event| matches!(
                    event.kind.as_str(),
                    "renegotiation-applied" | "resume-requested"
                )
            ));
            fixture.finish().await
        })
        .await?
    }

    #[tokio::test]
    async fn full_caps_retain_fifo_and_refill_once_after_real_closure() -> Result<()> {
        tokio::time::timeout(Duration::from_secs(5), async {
            let mut fixture = Fixture::new(
                vec![
                    producer("a1", MediaKind::Audio),
                    producer("a2", MediaKind::Audio),
                    producer("v1", MediaKind::Video),
                    producer("v2", MediaKind::Video),
                ],
                1,
                1,
            )
            .await?;
            fixture.consume("a1").await?;
            fixture.consume("v1").await?;
            anyhow::ensure!(
                tokio::time::timeout(Duration::from_millis(150), fixture.server.next())
                    .await
                    .is_err()
            );
            fixture.send(producer("a3", MediaKind::Audio)).await?;
            fixture
                .send(ServerMessage::ProducerClosed {
                    producer_id: "a1".into(),
                })
                .await?;
            fixture
                .send(ServerMessage::ProducerClosed {
                    producer_id: "a1".into(),
                })
                .await?;
            fixture.send(producer("a1", MediaKind::Audio)).await?;
            fixture.consume("a2").await?;
            fixture
                .send(ServerMessage::ProducerClosed {
                    producer_id: "v1".into(),
                })
                .await?;
            fixture.consume("v2").await?;
            fixture
                .send(ServerMessage::ProducerClosed {
                    producer_id: "a2".into(),
                })
                .await?;
            fixture.consume("a3").await?;
            fixture.finish().await
        })
        .await?
    }

    #[tokio::test]
    async fn buffered_close_cancels_discovery_before_first_dispatch() -> Result<()> {
        tokio::time::timeout(Duration::from_secs(5), async {
            let mut fixture = Fixture::new(
                vec![
                    producer("closed", MediaKind::Audio),
                    ServerMessage::ProducerClosed {
                        producer_id: "closed".into(),
                    },
                    producer("closed", MediaKind::Audio),
                    producer("live", MediaKind::Video),
                ],
                1,
                1,
            )
            .await?;
            fixture.consume("live").await?;
            fixture.finish().await
        })
        .await?
    }

    #[tokio::test]
    async fn closed_pending_consumer_never_reaches_native_setup_or_resume() -> Result<()> {
        tokio::time::timeout(Duration::from_secs(5), async {
            let mut fixture =
                Fixture::new(vec![producer("closing", MediaKind::Audio)], 1, 0).await?;
            fixture.consume("closing").await?;
            fixture
                .send(ServerMessage::ConsumerCreated {
                    consumer_id: "closing-consumer".into(),
                    producer_id: "closing".into(),
                    kind: MediaKind::Audio,
                    rtp_parameters: RtpParameters {
                        encodings: vec![RtpEncodingParameters {
                            ssrc: Some(12345),
                            ..Default::default()
                        }],
                        ..Default::default()
                    },
                })
                .await?;
            fixture
                .send(ServerMessage::ProducerClosed {
                    producer_id: "closing".into(),
                })
                .await?;
            // This fixture has no native receive transport: attempting native
            // setup would record an error, even if no resume were sent.
            anyhow::ensure!(
                tokio::time::timeout(Duration::from_millis(2300), fixture.server.next())
                    .await
                    .is_err()
            );
            let report = fixture.metrics.generate_report();
            anyhow::ensure!(report.consumers_created == 1);
            anyhow::ensure!(report.errors.is_empty());
            anyhow::ensure!(!report.diagnostics.as_ref().unwrap().events.iter().any(
                |event| matches!(
                    event.kind.as_str(),
                    "renegotiation-applied" | "resume-requested"
                )
            ));
            fixture.finish().await
        })
        .await?
    }

    #[tokio::test]
    async fn departure_stops_new_work_without_losing_explicit_leave() -> Result<()> {
        tokio::time::timeout(Duration::from_secs(5), async {
            let mut fixture = Fixture::new(
                vec![
                    producer("first", MediaKind::Audio),
                    producer("second", MediaKind::Audio),
                ],
                1,
                0,
            )
            .await?;
            fixture.consume("first").await?;
            fixture.metrics.end_session();
            fixture
                .send(ServerMessage::ProducerClosed {
                    producer_id: "first".into(),
                })
                .await?;
            anyhow::ensure!(
                tokio::time::timeout(Duration::from_millis(150), fixture.server.next())
                    .await
                    .is_err()
            );
            fixture.finish().await
        })
        .await?
    }
}

#[cfg(test)]
mod native_connect_order_tests {
    use super::*;
    use futures_util::FutureExt;
    use mediasoup_types::data_structures::{DtlsFingerprint, DtlsRole, DtlsState, IceState};
    use simplestChat::media::config::{RouterConfig, WebRtcTransportConfig};
    use simplestChat::media::transport_manager::TransportManager;
    use simplestChat::media::types::MediaError;
    use std::net::{IpAddr, Ipv4Addr};
    use std::panic::AssertUnwindSafe;

    /// Withhold signaling until the owned peer has made the native transport
    /// start DTLS from ICE. Connecting must not masquerade as an earlier
    /// successful connect() request: the remote fingerprint is still missing.
    async fn exercise_connect_order(is_send: bool, start_ice: bool, invalid_first: bool) {
        let participant_id = "owned-native-connect-regression";
        let manager = TransportManager::new();
        let metrics = Arc::new(MetricsCollector::new(participant_id.into()));
        let session = Arc::new(Mutex::new(WebRtcSession::new(
            participant_id.into(),
            metrics,
        )));
        let outcome = AssertUnwindSafe(tokio::time::timeout(Duration::from_secs(10), async {
            let workers = WorkerManager::new();
            let worker = workers.create_worker(WorkerSettings::default()).await?;
            let router = worker
                .create_router(RouterConfig::default().to_router_options())
                .await?;
            // Resolve a concrete owned loopback port, then hand it to the
            // native listener. A competing bind is a fixture error, not a retry
            // against an unrelated running service.
            let reservation = std::net::UdpSocket::bind("127.0.0.1:0")?;
            let port = reservation.local_addr()?.port();
            drop(reservation);
            let server = worker
                .create_webrtc_server(WebRtcServerOptions::new(WebRtcServerListenInfos::new(
                    ListenInfo {
                        protocol: Protocol::Udp,
                        ip: IpAddr::V4(Ipv4Addr::LOCALHOST),
                        announced_address: None,
                        expose_internal_ip: false,
                        port: Some(port),
                        port_range: None,
                        flags: None,
                        send_buffer_size: None,
                        recv_buffer_size: None,
                    },
                )))
                .await?;
            let captured = Arc::new(std::sync::Mutex::new(None));
            let capture_handler = server.on_new_webrtc_transport({
                let captured = captured.clone();
                move |transport| *captured.lock().unwrap() = Some(transport.clone())
            });
            let configuration = WebRtcTransportConfig::default();
            let info = if is_send {
                manager
                    .create_send_transport(participant_id.into(), &router, server, &configuration)
                    .await?
            } else {
                manager
                    .create_recv_transport(participant_id.into(), &router, server, &configuration)
                    .await?
            };
            let native = captured
                .lock()
                .unwrap()
                .take()
                .context("Native transport callback did not retain the owned handle")?;
            // Stop capture before awaiting: no retained callback-owned strong
            // transport handle may interfere with the test's final cleanup.
            drop(capture_handler);
            assert_eq!(native.dtls_state(), DtlsState::New);

            let parameters = if start_ice {
                if is_send {
                    session
                        .lock()
                        .await
                        .create_send_transport(
                            info.id.clone(),
                            info.ice_parameters,
                            info.ice_candidates,
                            info.dtls_parameters,
                        )
                        .await?
                } else {
                    session
                        .lock()
                        .await
                        .create_recv_transport(
                            info.id.clone(),
                            info.ice_parameters,
                            info.ice_candidates,
                            info.dtls_parameters,
                        )
                        .await?
                }
            } else {
                // Native parameter acceptance does not require ICE activity.
                // This case exercises idempotence while DTLS is still New.
                DtlsParameters {
                    role: DtlsRole::Client,
                    fingerprints: vec![DtlsFingerprint::Sha256 { value: [0x42; 32] }],
                }
            };
            if start_ice {
                while native.dtls_state() != DtlsState::Connecting
                    || !matches!(
                        native.ice_state(),
                        IceState::Connected | IceState::Completed
                    )
                {
                    sleep(Duration::from_millis(10)).await;
                }
                let stats = native.get_stats().await?;
                assert_eq!(
                    stats
                        .first()
                        .context("Native transport stats missing")?
                        .dtls_state,
                    DtlsState::Connecting
                );
            }
            if invalid_first {
                assert!(
                    manager
                        .connect_transport(
                            participant_id,
                            &info.id,
                            DtlsParameters {
                                role: DtlsRole::Client,
                                fingerprints: Vec::new()
                            },
                        )
                        .await
                        .is_err(),
                    "Invalid first native connect must fail rather than becoming an applied retry"
                );
            }
            assert!(
                manager
                    .connect_transport(participant_id, &info.id, parameters.clone())
                    .await?,
                "First valid signaling connect must perform native IPC even after DTLS starts"
            );
            assert!(
                !manager
                    .connect_transport(participant_id, &info.id, parameters)
                    .await?,
                "A successful native parameter application makes its immediate retry a no-op"
            );
            if start_ice {
                loop {
                    let stats = native.get_stats().await?;
                    if stats
                        .first()
                        .context("Native transport stats missing")?
                        .dtls_state
                        == DtlsState::Connected
                    {
                        break;
                    }
                    sleep(Duration::from_millis(10)).await;
                }
            } else {
                assert_eq!(native.dtls_state(), DtlsState::New);
            }
            Ok::<_, anyhow::Error>(())
        }))
        .catch_unwind()
        .await;

        // Execute both cleanup steps even when a test assertion, IPC error, or
        // timeout interrupts the body. If explicit cleanup itself times out,
        // the test fails and its owned Tokio runtime retires remaining tasks.
        let cleanup = tokio::time::timeout(Duration::from_secs(3), async {
            let peer_cleanup = session.lock().await.close().await;
            let native_cleanup = manager.remove_participant(participant_id).await;
            peer_cleanup?;
            match native_cleanup {
                Ok(()) | Err(MediaError::ParticipantNotFound(_)) => Ok(()),
                Err(error) => Err(anyhow::Error::new(error)),
            }
        })
        .await;
        match outcome {
            Ok(result) => result
                .expect("Native connect order regression timed out")
                .expect("Native connect order fixture failed"),
            Err(panic) => std::panic::resume_unwind(panic),
        }
        cleanup
            .expect("Native connect order cleanup timed out")
            .expect("Native connect order cleanup failed");
    }

    #[tokio::test]
    async fn send_transport_connects_when_ice_starts_dtls_before_signaling() {
        exercise_connect_order(true, true, false).await;
    }

    #[tokio::test]
    async fn receive_transport_connects_when_ice_starts_dtls_before_signaling() {
        exercise_connect_order(false, true, false).await;
    }

    #[tokio::test]
    async fn failed_native_parameters_do_not_consume_the_first_connect_attempt() {
        exercise_connect_order(false, true, true).await;
    }

    #[tokio::test]
    async fn successful_native_connect_is_idempotent_while_dtls_is_new() {
        exercise_connect_order(true, false, false).await;
    }
}
