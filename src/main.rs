#![forbid(unsafe_code)]

use anyhow::Result;
use simplestChat::{
    db, diagnostics::Diagnostics, media::MediaConfig, metrics::ServerMetrics, room::RoomManager,
    shutdown::run_stage, signaling::SignalingServer, turn::TurnConfig,
};
use std::sync::Arc;
use std::time::Duration;
use tracing::{error, info, warn};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

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
    // Initialize tracing
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "simplestChat=info,mediasoup=warn".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let diagnostics = Diagnostics::from_env()?;
    let result = run_server(diagnostics.clone()).await;
    if !diagnostics.shutdown().await {
        warn!("Local diagnostic output incomplete; server outcome is unchanged");
    }
    result
}

async fn run_server(diagnostics: Diagnostics) -> Result<()> {
    info!("SimplestChat - Starting server");

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
