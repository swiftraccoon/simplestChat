#![forbid(unsafe_code)]

use anyhow::Result;
use simplestChat::{
    db,
    diagnostics::Diagnostics,
    media::MediaConfig,
    metrics::ServerMetrics,
    room::RoomManager,
    saturation::{SaturationConfig, SaturationMonitor, WorkerThreads},
    shutdown::run_stage,
    signaling::SignalingServer,
    turn::TurnConfig,
};
use std::sync::Arc;
use std::time::Duration;
use tracing::{error, info, warn};
use tracing_subscriber::{Layer, layer::SubscriberExt, util::SubscriberInitExt};

async fn shutdown_signal() -> std::io::Result<()> {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! {
            result = tokio::signal::ctrl_c() => result?,
            _ = terminate.recv() => {},
        }
    }

    #[cfg(not(unix))]
    tokio::signal::ctrl_c().await?;

    Ok(())
}

fn main() -> Result<()> {
    // Unlike the implicit Tokio main teardown, this cannot wait indefinitely
    // for an already-running blocking password job after drain times out.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let result = runtime.block_on(run());
    runtime.shutdown_timeout(Duration::from_secs(1));
    result
}

async fn run() -> Result<()> {
    // JSON survives aggregation without parsing human messages. Operators may
    // select text locally; credentials and arbitrary request data are not fields.
    let format = std::env::var("LOG_FORMAT").unwrap_or_else(|_| "json".to_owned());
    let formatter = match format.as_str() {
        "json" => tracing_subscriber::fmt::layer()
            .json()
            .with_ansi(false)
            .boxed(),
        "text" => tracing_subscriber::fmt::layer().with_ansi(false).boxed(),
        _ => anyhow::bail!("LOG_FORMAT must be json or text"),
    };
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "simplestChat=info,mediasoup=warn,sqlx::pool=warn".into()),
        )
        .with(formatter)
        .init();

    let diagnostics = Diagnostics::from_env()?;
    let result = run_server(diagnostics.clone()).await;
    if !diagnostics.shutdown().await {
        warn!("Local diagnostic output incomplete; server outcome is unchanged");
    }
    result
}

async fn run_server(diagnostics: Diagnostics) -> Result<()> {
    let revision = std::env::var("SOURCE_REVISION")
        .ok()
        .filter(|value| value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .unwrap_or_else(|| "unknown".to_owned());
    info!(revision, "simplestChat server starting");

    // Create room manager (includes media server)
    let mut media_config = MediaConfig::from_env()?;

    // Set announced IP from environment variable (required for ICE candidates)
    // Falls back to loopback; the local launcher can select an owned LAN address.
    if let Ok(ip) = std::env::var("ANNOUNCE_IP") {
        info!("Using ANNOUNCE_IP={}", ip);
        let addr = ip
            .parse()
            .map_err(|_| anyhow::anyhow!("Invalid ANNOUNCE_IP: {ip}"))?;
        media_config.webrtc_transport_config =
            media_config.webrtc_transport_config.with_public_ip(addr);
    } else {
        // Use 127.0.0.1 as fallback for localhost testing.
        let default_ip: std::net::IpAddr = "127.0.0.1".parse().unwrap();
        info!("No ANNOUNCE_IP set, using {}", default_ip);
        media_config.webrtc_transport_config = media_config
            .webrtc_transport_config
            .with_public_ip(default_ip);
    }

    let metrics = ServerMetrics::with_diagnostics(diagnostics);
    // Connect to database (optional)
    let db_pool = db::connect().await?;

    let room_manager =
        Arc::new(RoomManager::new(media_config, metrics.clone(), db_pool.clone()).await?);
    room_manager.spawn_worker_recovery();
    match SaturationConfig::from_env()? {
        Some(config) => {
            let workers: Arc<dyn WorkerThreads> = room_manager.media_server().worker_manager();
            room_manager.attach_saturation(SaturationMonitor::spawn(
                config,
                metrics.clone(),
                Some(workers),
            ));
        }
        None => info!("CPU saturation monitor disabled by configuration"),
    }
    room_manager.spawn_quality_sampler(
        Duration::from_secs(quality_sample_interval_secs()?),
        quality_sample_max_transport_stats()?,
    );

    info!("Room manager and media server initialized");

    // Load TURN config from environment (optional)
    let turn_config = TurnConfig::from_env()?;
    if let Some(ref tc) = turn_config {
        info!(
            "TURN configured: {} URL(s), TTL {}s",
            tc.urls.len(),
            tc.ttl_secs
        );
    } else {
        info!("No TURN configured (set TURN_URLS and TURN_SECRET to enable)");
    }

    // Create and start signaling server
    let signaling_server =
        SignalingServer::new(room_manager.clone(), turn_config, metrics, db_pool.clone())?;
    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3000);

    info!("Starting signaling server on port {}", port);

    // Keep the HTTP future alive on signal: dropping it would skip Axum's
    // graceful connection drain. Upgraded WebSockets are waited separately.
    let mut serving = Box::pin(signaling_server.clone().serve(port));
    let (server_completed, first_result) = tokio::select! {
        result = &mut serving => (true, result),
        signal = shutdown_signal() => {
            info!("Received shutdown signal; beginning bounded drain");
            (false, signal.map_err(anyhow::Error::from))
        }
    };

    signaling_server.begin_draining();
    let draining_server = signaling_server.clone();
    let network = async move {
        if !server_completed {
            serving.await?;
        }
        draining_server.wait_for_connections().await;
        Ok::<(), anyhow::Error>(())
    };
    // Eight seconds shared by HTTP/upgraded sockets and room membership,
    // followed by six seconds for media and two seconds for the database.
    let (network_result, rooms_result) = tokio::join!(
        run_stage("HTTP and WebSockets", Duration::from_secs(8), network),
        room_manager.shutdown(),
    );
    if network_result.is_err() {
        error!(
            remaining_connections = signaling_server.connection_count(),
            remaining_password_jobs = signaling_server.pending_password_work(),
            "Connection drain incomplete"
        );
    }
    let media_result = room_manager.media_server().shutdown().await;
    let database_result = run_stage("database pool", Duration::from_secs(2), async {
        if let Some(pool) = db_pool {
            pool.close().await;
        }
        Ok::<(), anyhow::Error>(())
    })
    .await;
    first_result?;
    anyhow::ensure!(
        network_result.is_ok()
            && rooms_result.is_ok()
            && media_result.is_ok()
            && database_result.is_ok(),
        "Server shutdown incomplete; see stage errors"
    );
    info!("Server shutdown complete");
    Ok(())
}

/// `QUALITY_SAMPLE_INTERVAL_SECS`: seconds between server-side media quality samples (5–300, default 15).
fn quality_sample_interval_secs() -> Result<u64> {
    match std::env::var("QUALITY_SAMPLE_INTERVAL_SECS") {
        Ok(value) => value
            .trim()
            .parse::<u64>()
            .ok()
            .filter(|v| (5..=300).contains(v))
            .ok_or_else(|| {
                anyhow::anyhow!("QUALITY_SAMPLE_INTERVAL_SECS must be between 5 and 300")
            }),
        Err(_) => Ok(15),
    }
}

/// `QUALITY_SAMPLE_MAX_TRANSPORT_STATS`: receive transports asked for statistics per sample (0–10000, default 100).
fn quality_sample_max_transport_stats() -> Result<usize> {
    match std::env::var("QUALITY_SAMPLE_MAX_TRANSPORT_STATS") {
        Ok(value) => value
            .trim()
            .parse::<usize>()
            .ok()
            .filter(|v| *v <= 10_000)
            .ok_or_else(|| {
                anyhow::anyhow!("QUALITY_SAMPLE_MAX_TRANSPORT_STATS must be between 0 and 10000")
            }),
        Err(_) => Ok(100),
    }
}
