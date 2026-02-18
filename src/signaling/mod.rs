#![forbid(unsafe_code)]

// Signaling module - WebSocket signaling server

pub mod protocol;
pub mod connection;

use crate::auth::webauthn::ChallengeStore;
use crate::metrics::ServerMetrics;
use crate::room::RoomManager;
use crate::turn::TurnConfig;
use sqlx::PgPool;
use axum::{
    extract::{ws::WebSocketUpgrade, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use std::sync::Arc;
use tokio::sync::Semaphore;
use tower_http::services::ServeDir;
use tower_http::cors::CorsLayer;
use tracing::{info, warn};

pub use connection::GracePeriodMap;

/// Signaling server state
#[derive(Clone)]
pub struct SignalingServer {
    room_manager: Arc<RoomManager>,
    turn_config: Option<Arc<TurnConfig>>,
    grace_periods: GracePeriodMap,
    metrics: ServerMetrics,
    connection_semaphore: Arc<Semaphore>,
    db_pool: Option<PgPool>,
    jwt_secret: Option<String>,
    webauthn: Option<Arc<webauthn_rs::prelude::Webauthn>>,
    challenge_store: Option<Arc<ChallengeStore>>,
}

impl SignalingServer {
    /// Creates a new signaling server
    pub fn new(room_manager: Arc<RoomManager>, turn_config: Option<TurnConfig>, metrics: ServerMetrics, db_pool: Option<PgPool>) -> Self {
        let mut max_connections: usize = std::env::var("MAX_CONNECTIONS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(10_000);
        if max_connections == 0 {
            warn!("MAX_CONNECTIONS=0 would reject all connections, using default 10000");
            max_connections = 10_000;
        }
        info!("Max connections: {}", max_connections);

        let jwt_secret = std::env::var("JWT_SECRET").ok();
        if jwt_secret.is_some() {
            info!("JWT authentication enabled");
        } else {
            info!("JWT_SECRET not set — authentication disabled");
        }

        let (webauthn, challenge_store) = crate::auth::webauthn::init_webauthn()
            .map(|(w, c)| (Some(Arc::new(w)), Some(c)))
            .unwrap_or((None, None));

        Self {
            room_manager,
            turn_config: turn_config.map(Arc::new),
            grace_periods: GracePeriodMap::new(),
            metrics,
            connection_semaphore: Arc::new(Semaphore::new(max_connections)),
            db_pool,
            jwt_secret,
            webauthn,
            challenge_store,
        }
    }

    pub fn db_pool(&self) -> Option<&PgPool> {
        self.db_pool.as_ref()
    }

    pub fn jwt_secret(&self) -> Option<&str> {
        self.jwt_secret.as_deref()
    }

    pub fn webauthn(&self) -> Option<&webauthn_rs::prelude::Webauthn> {
        self.webauthn.as_deref()
    }

    pub fn challenge_store(&self) -> Option<&ChallengeStore> {
        self.challenge_store.as_deref()
    }

    /// Creates the Axum router for the signaling server
    pub fn router(self) -> Router {
        use axum::routing::post;

        let auth_routes = Router::new()
            .route("/register", post(crate::auth::routes::register))
            .route("/login", post(crate::auth::routes::login))
            .route("/refresh", post(crate::auth::routes::refresh))
            .route("/passkey/register/start", post(crate::auth::routes::passkey_register_start))
            .route("/passkey/register/finish", post(crate::auth::routes::passkey_register_finish))
            .route("/passkey/login/start", post(crate::auth::routes::passkey_login_start))
            .route("/passkey/login/finish", post(crate::auth::routes::passkey_login_finish));

        Router::new()
            .route("/ws", get(ws_handler))
            .route("/health", get(health_handler))
            .route("/metrics", get(metrics_handler))
            .nest("/api/auth", auth_routes)
            .with_state(self)
            .layer(CorsLayer::permissive())
            .fallback_service(ServeDir::new("web/dist"))
    }

    /// Starts the signaling server on the specified port
    ///
    /// # Errors
    /// Returns an error if the server fails to bind to the port
    pub async fn serve(self, port: u16) -> anyhow::Result<()> {
        let addr = format!("0.0.0.0:{port}");
        info!("Starting signaling server on {}", addr);

        let listener = tokio::net::TcpListener::bind(&addr).await?;
        let app = self.router();

        axum::serve(listener, app).await?;

        Ok(())
    }
}

/// Health check handler
async fn health_handler(
    State(server): State<SignalingServer>,
) -> Json<serde_json::Value> {
    let rooms = server.room_manager.room_count().await;
    let participants = server.room_manager.total_participant_count().await;
    Json(serde_json::json!({
        "status": "ok",
        "rooms": rooms,
        "participants": participants,
    }))
}

/// Metrics handler — Prometheus text exposition format.
/// Protected by optional METRICS_TOKEN env var (Bearer auth).
async fn metrics_handler(
    State(server): State<SignalingServer>,
    headers: HeaderMap,
) -> Response {
    // Check bearer token if METRICS_TOKEN is configured
    if let Ok(expected) = std::env::var("METRICS_TOKEN") {
        let provided = headers.get("authorization")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if provided != format!("Bearer {}", expected) {
            return StatusCode::UNAUTHORIZED.into_response();
        }
    }

    let rooms = server.room_manager.room_count().await;
    let participants = server.room_manager.total_participant_count().await;
    let body = server.metrics.render_prometheus(rooms, participants);
    (
        StatusCode::OK,
        [("content-type", "text/plain; version=0.0.4; charset=utf-8")],
        body,
    ).into_response()
}

/// WebSocket upgrade handler
async fn ws_handler(
    ws: WebSocketUpgrade,
    State(server): State<SignalingServer>,
) -> Response {
    // Acquire connection permit (non-blocking)
    let permit = match server.connection_semaphore.clone().try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => {
            warn!("Connection limit reached, rejecting WebSocket upgrade");
            return (StatusCode::SERVICE_UNAVAILABLE, "Too many connections").into_response();
        }
    };

    ws.max_message_size(65_536)
        .on_failed_upgrade(|error| {
            warn!("WebSocket upgrade failed: {}", error);
        })
        .on_upgrade(move |socket| {
            connection::handle_connection(
                socket,
                server.room_manager,
                server.turn_config,
                server.grace_periods,
                server.metrics,
                permit,
            )
        })
}
