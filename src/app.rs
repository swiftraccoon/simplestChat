//! Process entry: configuration preflight, runtime construction, startup and
//! the bounded shutdown choreography. The binary in `src/main.rs` only calls
//! [`main`], so every log line carries this crate's target.

use crate::{
    config::{Config, LogFormat},
    db,
    diagnostics::Diagnostics,
    metrics::ServerMetrics,
    room::{ManagerSettings, RoomManager},
    saturation::{SaturationMonitor, WorkerThreads},
    shutdown::run_stage,
    signaling::SignalingServer,
};
use anyhow::Result;
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

pub fn main() -> Result<()> {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    anyhow::ensure!(
        arguments.is_empty() || arguments == ["--check-config"],
        "Usage: simplestChat [--check-config]"
    );
    // Every setting is read and validated before native workers, migrations or
    // listeners can have side effects. Secrets never appear in the output.
    let config = Config::from_env()?;
    if !arguments.is_empty() {
        println!(
            "{}",
            serde_json::json!({
                "version": 2,
                "settings": config.settings(),
                "scope": "every setting parsed and validated; no listener, worker, database or relay is exercised"
            })
        );
        return Ok(());
    }
    // Unlike the implicit Tokio main teardown, this cannot wait indefinitely
    // for an already-running blocking password job after drain times out.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let result = runtime.block_on(run(config));
    runtime.shutdown_timeout(Duration::from_secs(1));
    result
}

async fn run(config: Config) -> Result<()> {
    // JSON survives aggregation without parsing human messages. Operators may
    // select text locally; credentials and arbitrary request data are not fields.
    let formatter = match config.observability.log_format {
        LogFormat::Json => tracing_subscriber::fmt::layer()
            .json()
            .with_ansi(false)
            .boxed(),
        LogFormat::Text => tracing_subscriber::fmt::layer().with_ansi(false).boxed(),
    };
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "simplestchat=info,mediasoup=warn,sqlx::pool=warn".into()),
        )
        .with(formatter)
        .init();

    let diagnostics = Diagnostics::configure(&config.diagnostics)?;
    let result = run_server(config, diagnostics.clone()).await;
    if !diagnostics.shutdown().await {
        warn!("Local diagnostic output incomplete; server outcome is unchanged");
    }
    result
}

async fn run_server(config: Config, diagnostics: Diagnostics) -> Result<()> {
    let revision = config
        .observability
        .source_revision
        .as_deref()
        .unwrap_or("unknown");
    info!(revision, "simplestChat server starting");

    let mut media_config = config.media.clone();
    crate::sizing::announce(
        media_config.worker_config.num_workers,
        config.limits.max_connections,
        config.limits.max_rooms,
    );

    // The addresses clients reach, one listener per family: ANNOUNCE_IP (IPv4 or
    // IPv6; loopback when unset, the local launcher selects an owned LAN address)
    // and, on a dual-stack host, ANNOUNCE_IPV6 beside an IPv4 ANNOUNCE_IP.
    info!("Announcing {}", config.network.announce_ip);
    media_config.webrtc_transport_config = media_config
        .webrtc_transport_config
        .with_public_ip(config.network.announce_ip);
    if let Some(address) = config.network.announce_ipv6 {
        info!("Announcing {address} beside the IPv4 address");
        media_config.webrtc_transport_config = media_config
            .webrtc_transport_config
            .with_announced_address(std::net::IpAddr::V6(address));
    }
    media_config.validate()?;

    let metrics = ServerMetrics::with_diagnostics(diagnostics);
    let db_pool = db::connect(&config.database).await?;
    if let Some(pool) = &db_pool {
        let _chat_retention = crate::room::history::spawn_retention(pool.clone());
        // Moderation history and closed reports age out on their own.
        let _retention =
            crate::room::moderation::spawn_retention(pool.clone(), config.rooms.retention);
    }

    let room_manager = Arc::new(
        RoomManager::new(
            ManagerSettings::from(&config),
            media_config,
            metrics.clone(),
            db_pool.clone(),
        )
        .await?,
    );
    room_manager.spawn_worker_recovery();
    match config.saturation.clone() {
        Some(saturation) => {
            let workers: Arc<dyn WorkerThreads> = room_manager.media_server().worker_manager();
            room_manager.attach_saturation(SaturationMonitor::spawn(
                saturation,
                metrics.clone(),
                Some(workers),
            ));
        }
        None => info!("CPU saturation monitor disabled by configuration"),
    }
    room_manager.spawn_quality_sampler(
        config.observability.quality_sample_interval,
        config.observability.quality_sample_max_transport_stats,
    );

    info!("Room manager and media server initialized");

    match &config.turn {
        Some(turn) => info!(
            "TURN configured: {} URL(s), TTL {}s",
            turn.urls.len(),
            turn.ttl_secs
        ),
        None => info!("No TURN configured (set TURN_URLS and TURN_SECRET to enable)"),
    }

    let signaling_server =
        SignalingServer::new(room_manager.clone(), metrics, db_pool.clone(), &config)?;
    if let Some(pool) = &db_pool {
        let _notifications =
            crate::push::spawn(pool.clone(), signaling_server.notification_origin()).await?;
    }

    // Keep the HTTP future alive on signal: dropping it would skip Axum's
    // graceful connection drain. Upgraded WebSockets are waited separately.
    let mut serving = Box::pin(signaling_server.clone().serve());
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
