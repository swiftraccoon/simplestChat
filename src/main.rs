#![forbid(unsafe_code)]

mod auth;
mod db;
mod media;
mod metrics;
mod room;
mod signaling;
mod turn;

use anyhow::Result;
use media::MediaConfig;
use metrics::ServerMetrics;
use room::RoomManager;
use signaling::SignalingServer;
use std::sync::Arc;
use tracing::info;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use turn::TurnConfig;

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

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "simplestChat=info,mediasoup=warn".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    info!("SimplestChat - Starting server");

    // Create room manager (includes media server)
    let mut media_config = MediaConfig::from_env()?;

    // Set announced IP from environment variable (required for ICE candidates)
    // Falls back to detecting the first non-loopback IPv4 address
    if let Ok(ip) = std::env::var("ANNOUNCE_IP") {
        info!("Using ANNOUNCE_IP={}", ip);
        let addr = ip
            .parse()
            .map_err(|_| anyhow::anyhow!("Invalid ANNOUNCE_IP: {ip}"))?;
        media_config.webrtc_transport_config =
            media_config.webrtc_transport_config.with_public_ip(addr);
    } else {
        // Auto-detect: use 127.0.0.1 as fallback for localhost testing
        let default_ip: std::net::IpAddr = "127.0.0.1".parse().unwrap();
        info!("No ANNOUNCE_IP set, using {}", default_ip);
        media_config.webrtc_transport_config = media_config
            .webrtc_transport_config
            .with_public_ip(default_ip);
    }

    let metrics = ServerMetrics::new();
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
        SignalingServer::new(room_manager.clone(), turn_config, metrics, db_pool)?;
    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3000);

    info!("Starting signaling server on port {}", port);

    // Run server with graceful shutdown
    let server_result = tokio::select! {
        result = signaling_server.serve(port) => {
            result
        }
        signal = shutdown_signal() => {
            signal?;
            info!("Received shutdown signal, shutting down...");
            Ok(())
        }
    };

    room_manager.shutdown().await;
    server_result?;
    info!("Server shutdown complete");
    Ok(())
}
