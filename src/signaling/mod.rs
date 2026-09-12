#![forbid(unsafe_code)]

// Signaling module - WebSocket signaling server

pub mod connection;
mod media_diagnostics;
pub mod protocol;
mod readiness;

use crate::auth::webauthn::ChallengeStore;
use crate::metrics::ServerMetrics;
use crate::room::RoomManager;
use crate::turn::TurnConfig;
use axum::{
    Json, Router,
    body::Body,
    extract::{ConnectInfo, DefaultBodyLimit, Query, State, ws::WebSocketUpgrade},
    http::{HeaderMap, HeaderValue, Request, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
};
use serde::Deserialize;
use sqlx::PgPool;
use std::{
    collections::{HashMap, hash_map::RandomState},
    hash::{BuildHasher, Hash, Hasher},
    net::{IpAddr, SocketAddr},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::Semaphore;
use tower_http::services::ServeDir;
use tower_http::timeout::{RequestBodyTimeoutLayer, TimeoutLayer};
use tracing::{info, warn};

pub use connection::GracePeriodMap;

#[derive(Clone, Copy)]
pub(crate) struct ClientIp(pub(crate) IpAddr);

const DEFAULT_AUTH_REQUESTS_PER_MINUTE: u32 = 60;
const DEFAULT_AUTH_REQUESTS_PER_ACCOUNT_PER_MINUTE: u32 = 20;
const DEFAULT_AUTH_CONCURRENCY: usize = 16;
const DEFAULT_ROOM_API_REQUESTS_PER_MINUTE: u32 = 120;
const DEFAULT_ROOM_API_CONCURRENCY: usize = 32;
const DEFAULT_ROOM_CREATIONS_PER_ACCOUNT_PER_MINUTE: u32 = 10;
const DEFAULT_WS_HANDSHAKES_PER_MINUTE: u32 = 120;
const DEFAULT_CONNECTIONS_PER_IP: usize = 50;
const DEFAULT_MAX_USERS: i64 = 100_000;
const REGISTRATION_ENABLED_BY_DEFAULT: bool = false;
const MAX_TRACKED_AUTH_IPS: usize = 10_000;
const PRINCIPAL_RATE_WINDOW: Duration = Duration::from_secs(60);
// Four independently keyed 64K-wide rows cost 1 MiB per limiter. The
// process-secret hashes prevent callers from manufacturing collisions, while
// conservative updates make broad username churn much less polluting than a
// conventional count-min sketch.
const PRINCIPAL_SKETCH_DEPTH: usize = 4;
const PRINCIPAL_SKETCH_WIDTH: usize = 65_536;
const PRINCIPAL_SKETCH_WIDTH_U64: u64 = 65_536;
const HTTP_REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const HTTP_BODY_IDLE_TIMEOUT: Duration = Duration::from_secs(5);
const WS_AUTH_PROTOCOL_PREFIX: &str = "auth.";

fn rate_limit_ip(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V4(_) => ip,
        IpAddr::V6(address) => {
            if let Some(address) = address.to_ipv4_mapped() {
                return IpAddr::V4(address);
            }
            let segments = address.segments();
            IpAddr::V6(std::net::Ipv6Addr::new(
                segments[0],
                segments[1],
                segments[2],
                segments[3],
                0,
                0,
                0,
                0,
            ))
        }
    }
}

#[derive(Debug)]
struct AuthRateEntry {
    window_started: Instant,
    requests: u32,
}

#[derive(Clone)]
struct AuthGuard {
    entries: Arc<Mutex<HashMap<IpAddr, AuthRateEntry>>>,
    max_requests_per_minute: u32,
    concurrency: Arc<Semaphore>,
    trusted_proxy_secret: Arc<Option<String>>,
    allowed_origins: Arc<Vec<String>>,
}

impl AuthGuard {
    fn new(
        max_requests_per_minute: u32,
        max_concurrency: usize,
        trusted_proxy_secret: Arc<Option<String>>,
        allowed_origins: Arc<Vec<String>>,
    ) -> Self {
        Self {
            entries: Arc::new(Mutex::new(HashMap::new())),
            max_requests_per_minute,
            concurrency: Arc::new(Semaphore::new(max_concurrency)),
            trusted_proxy_secret,
            allowed_origins,
        }
    }

    fn allow(&self, ip: IpAddr) -> bool {
        let ip = rate_limit_ip(ip);
        let now = Instant::now();
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());

        if entries.len() >= MAX_TRACKED_AUTH_IPS && !entries.contains_key(&ip) {
            // Capacity is a memory bound, not a global deny switch. Constant-
            // time arbitrary eviction avoids both global lockout and an O(n)
            // cache scan for every attacker-chosen key after saturation.
            if let Some(evicted) = entries.keys().next().copied() {
                entries.remove(&evicted);
            }
        }

        let entry = entries.entry(ip).or_insert(AuthRateEntry {
            window_started: now,
            requests: 0,
        });
        if now.duration_since(entry.window_started) >= Duration::from_secs(60) {
            entry.window_started = now;
            entry.requests = 0;
        }
        if entry.requests >= self.max_requests_per_minute {
            return false;
        }
        entry.requests += 1;
        true
    }

    fn try_acquire_concurrency(&self) -> Option<tokio::sync::OwnedSemaphorePermit> {
        self.concurrency.clone().try_acquire_owned().ok()
    }
}

#[derive(Clone)]
struct PrincipalRateLimiter {
    state: Arc<Mutex<PrincipalRateState>>,
    max_requests_per_minute: u32,
}

struct PrincipalRateState {
    window_started: Instant,
    counters: Vec<u32>,
    hashers: [RandomState; PRINCIPAL_SKETCH_DEPTH],
}

impl PrincipalRateLimiter {
    fn new(max_requests_per_minute: u32) -> Self {
        Self {
            state: Arc::new(Mutex::new(PrincipalRateState {
                window_started: Instant::now(),
                counters: vec![0; PRINCIPAL_SKETCH_DEPTH * PRINCIPAL_SKETCH_WIDTH],
                hashers: std::array::from_fn(|_| RandomState::new()),
            })),
            max_requests_per_minute,
        }
    }

    fn allow(&self, principal: &str) -> bool {
        self.allow_at(principal, Instant::now())
    }

    fn allow_at(&self, principal: &str, now: Instant) -> bool {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if now.duration_since(state.window_started) >= PRINCIPAL_RATE_WINDOW {
            state.counters.fill(0);
            state.window_started = now;
        }

        let indexes: [usize; PRINCIPAL_SKETCH_DEPTH] = std::array::from_fn(|row| {
            let mut hasher = state.hashers[row].build_hasher();
            row.hash(&mut hasher);
            principal.hash(&mut hasher);
            row * PRINCIPAL_SKETCH_WIDTH
                + usize::try_from(hasher.finish() % PRINCIPAL_SKETCH_WIDTH_U64)
                    .expect("sketch index fits usize")
        });
        let estimate = indexes
            .iter()
            .map(|index| state.counters[*index])
            .min()
            .unwrap_or(0);
        if estimate >= self.max_requests_per_minute {
            return false;
        }

        // Conservative update increments only cells at the current minimum.
        // A victim that has reached its limit can never be reset by collisions
        // or attacker-chosen principal churn within the active window.
        for index in indexes {
            if state.counters[index] == estimate {
                state.counters[index] = state.counters[index].saturating_add(1);
            }
        }
        true
    }
}

#[derive(Clone)]
struct IpConnectionLimiter {
    counts: Arc<Mutex<HashMap<IpAddr, usize>>>,
    max_per_ip: usize,
}

impl IpConnectionLimiter {
    fn new(max_per_ip: usize) -> Self {
        Self {
            counts: Arc::new(Mutex::new(HashMap::new())),
            max_per_ip,
        }
    }

    fn try_acquire(&self, ip: IpAddr) -> Option<IpConnectionPermit> {
        let ip = rate_limit_ip(ip);
        let mut counts = self.counts.lock().unwrap_or_else(|e| e.into_inner());
        let count = counts.entry(ip).or_default();
        if *count >= self.max_per_ip {
            return None;
        }
        *count += 1;
        Some(IpConnectionPermit {
            ip,
            limiter: self.clone(),
        })
    }
}

struct IpConnectionPermit {
    ip: IpAddr,
    limiter: IpConnectionLimiter,
}

impl Drop for IpConnectionPermit {
    fn drop(&mut self) {
        let mut counts = self
            .limiter
            .counts
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(count) = counts.get_mut(&self.ip) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                counts.remove(&self.ip);
            }
        }
    }
}

/// Signaling server state
#[derive(Clone)]
pub struct SignalingServer {
    room_manager: Arc<RoomManager>,
    turn_config: Option<Arc<TurnConfig>>,
    grace_periods: GracePeriodMap,
    metrics: ServerMetrics,
    readiness: readiness::ReadinessState,
    connection_semaphore: Arc<Semaphore>,
    max_connections: usize,
    ip_connection_limiter: IpConnectionLimiter,
    ws_handshake_guard: AuthGuard,
    auth_guard: AuthGuard,
    password_work: Arc<Semaphore>,
    max_password_work: usize,
    principal_auth_limiter: PrincipalRateLimiter,
    room_creation_limiter: PrincipalRateLimiter,
    room_api_guard: AuthGuard,
    db_pool: Option<PgPool>,
    jwt_secret: Option<String>,
    metrics_token: Option<String>,
    media_diagnostics: Option<Arc<media_diagnostics::MediaDiagnostics>>,
    allowed_origins: Arc<Vec<String>>,
    trusted_proxy_secret: Arc<Option<String>>,
    webauthn: Option<Arc<webauthn_rs::prelude::Webauthn>>,
    challenge_store: Option<Arc<ChallengeStore>>,
    registration_enabled: bool,
    max_users: i64,
    auth_revocations: tokio::sync::broadcast::Sender<(String, i64)>,
}

impl SignalingServer {
    /// Creates a new signaling server
    pub fn new(
        room_manager: Arc<RoomManager>,
        turn_config: Option<TurnConfig>,
        metrics: ServerMetrics,
        db_pool: Option<PgPool>,
    ) -> anyhow::Result<Self> {
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
        if let Some(secret) = jwt_secret.as_deref()
            && secret.len() < 32
        {
            anyhow::bail!("JWT_SECRET must contain at least 32 bytes");
        }
        if jwt_secret.is_some() {
            info!("JWT authentication enabled");
        } else {
            info!("JWT_SECRET not set — authentication disabled");
        }

        let metrics_token = std::env::var("METRICS_TOKEN").ok();
        if let Some(token) = metrics_token.as_deref()
            && token.len() < 32
        {
            anyhow::bail!("METRICS_TOKEN must contain at least 32 bytes");
        }
        let media_diagnostics = media_diagnostics::MediaDiagnostics::configure(
            env_bool("MEDIA_DIAGNOSTICS_ENABLED", false)?,
            metrics_token.as_deref(),
        )?;

        let trusted_proxy_secret = std::env::var("TRUSTED_PROXY_SECRET")
            .ok()
            .filter(|secret| !secret.is_empty());
        if let Some(secret) = trusted_proxy_secret.as_deref()
            && secret.len() < 32
        {
            anyhow::bail!("TRUSTED_PROXY_SECRET must contain at least 32 bytes");
        }
        let trusted_proxy_secret = Arc::new(trusted_proxy_secret);

        let max_connections_per_ip =
            env_usize("MAX_CONNECTIONS_PER_IP", DEFAULT_CONNECTIONS_PER_IP);
        let auth_requests_per_minute =
            env_u32("AUTH_REQUESTS_PER_MINUTE", DEFAULT_AUTH_REQUESTS_PER_MINUTE);
        let auth_requests_per_account_per_minute = env_u32(
            "AUTH_REQUESTS_PER_ACCOUNT_PER_MINUTE",
            DEFAULT_AUTH_REQUESTS_PER_ACCOUNT_PER_MINUTE,
        );
        let auth_concurrency = env_usize("AUTH_MAX_CONCURRENCY", DEFAULT_AUTH_CONCURRENCY);
        let room_api_requests_per_minute = env_u32(
            "ROOM_API_REQUESTS_PER_MINUTE",
            DEFAULT_ROOM_API_REQUESTS_PER_MINUTE,
        );
        let room_api_concurrency =
            env_usize("ROOM_API_MAX_CONCURRENCY", DEFAULT_ROOM_API_CONCURRENCY);
        let room_creations_per_account = env_u32(
            "ROOM_CREATIONS_PER_ACCOUNT_PER_MINUTE",
            DEFAULT_ROOM_CREATIONS_PER_ACCOUNT_PER_MINUTE,
        );
        let ws_handshakes_per_minute =
            env_u32("WS_HANDSHAKES_PER_MINUTE", DEFAULT_WS_HANDSHAKES_PER_MINUTE);
        let allowed_origins = Arc::new(parse_allowed_origins()?);
        let bind_addr = std::env::var("BIND_ADDR").unwrap_or_else(|_| "127.0.0.1".to_string());
        let externally_reachable = bind_addr
            .parse::<IpAddr>()
            .map_or(true, |address| !address.is_loopback());
        if externally_reachable && allowed_origins.is_empty() {
            anyhow::bail!("ALLOWED_ORIGINS is required when BIND_ADDR is not loopback");
        }
        // A loopback listener can still be internet-facing through a reverse
        // proxy, including the bundled Caddy deployment.
        let registration_enabled =
            env_bool("REGISTRATION_ENABLED", REGISTRATION_ENABLED_BY_DEFAULT)?;
        let max_users = std::env::var("MAX_USERS")
            .ok()
            .and_then(|value| value.parse().ok())
            .filter(|value| (1..=10_000_000).contains(value))
            .unwrap_or(DEFAULT_MAX_USERS);

        let (webauthn, challenge_store) = crate::auth::webauthn::init_webauthn()?
            .map(|(w, c)| (Some(Arc::new(w)), Some(c)))
            .unwrap_or((None, None));

        let readiness = readiness::ReadinessState::new(room_manager.drain_signal());
        Ok(Self {
            room_manager,
            turn_config: turn_config.map(Arc::new),
            // Grace state does not hold a live-connection permit. Bound it
            // separately at the same configured process limit so disconnect
            // churn cannot grow retained room/media sessions without limit.
            grace_periods: GracePeriodMap::with_capacity(max_connections),
            metrics,
            readiness,
            connection_semaphore: Arc::new(Semaphore::new(max_connections)),
            max_connections,
            ip_connection_limiter: IpConnectionLimiter::new(max_connections_per_ip),
            ws_handshake_guard: AuthGuard::new(
                ws_handshakes_per_minute,
                1,
                trusted_proxy_secret.clone(),
                allowed_origins.clone(),
            ),
            auth_guard: AuthGuard::new(
                auth_requests_per_minute,
                auth_concurrency,
                trusted_proxy_secret.clone(),
                allowed_origins.clone(),
            ),
            // Keep the permit alive inside each spawn_blocking Argon2 job. The
            // outer HTTP timeout may cancel a handler, but it must not release
            // capacity while that CPU-heavy job is still running.
            password_work: Arc::new(Semaphore::new(auth_concurrency)),
            max_password_work: auth_concurrency,
            principal_auth_limiter: PrincipalRateLimiter::new(auth_requests_per_account_per_minute),
            room_creation_limiter: PrincipalRateLimiter::new(room_creations_per_account),
            room_api_guard: AuthGuard::new(
                room_api_requests_per_minute,
                room_api_concurrency,
                trusted_proxy_secret.clone(),
                allowed_origins.clone(),
            ),
            db_pool,
            jwt_secret,
            metrics_token,
            media_diagnostics,
            allowed_origins,
            trusted_proxy_secret,
            webauthn,
            challenge_store,
            registration_enabled,
            max_users,
            auth_revocations: tokio::sync::broadcast::channel(128).0,
        })
    }

    pub fn room_manager(&self) -> &RoomManager {
        &self.room_manager
    }

    /// Closes room/upgrade admission, wakes socket writers, stops HTTP accepts,
    /// and cancels retained reconnect timers. Room/media cleanup is coordinated
    /// separately, including membership retained by those cancelled timers.
    pub fn begin_draining(&self) {
        self.readiness.begin_draining();
        let retained_sessions = self.grace_periods.close();
        info!(
            retained_sessions,
            "Signaling admission closed; reconnect grace cancelled"
        );
    }

    /// Upgraded WebSockets and blocking password jobs can outlive their HTTP
    /// callers. Wait for their owned permits too; the coordinator bounds this.
    pub async fn wait_for_connections(&self) {
        while self.connection_count() > 0 || self.pending_password_work() > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

    /// Includes accepted upgrades and any authentication work holding a permit.
    pub fn connection_count(&self) -> usize {
        self.max_connections
            .saturating_sub(self.connection_semaphore.available_permits())
    }

    /// Password permits remain owned by blocking jobs even after cancellation
    /// of a request. Counting both lanes makes incomplete CPU cleanup visible.
    pub fn pending_password_work(&self) -> usize {
        self.max_password_work
            .saturating_sub(self.password_work.available_permits())
            + self.room_manager.pending_password_work()
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

    pub(crate) fn registration_enabled(&self) -> bool {
        self.registration_enabled
    }

    pub(crate) fn max_users(&self) -> i64 {
        self.max_users
    }

    pub(crate) fn allow_auth_principal(&self, principal: &str) -> bool {
        self.principal_auth_limiter.allow(principal)
    }

    pub(crate) fn allow_room_creation(&self, user_id: &str) -> bool {
        self.room_creation_limiter.allow(user_id)
    }

    pub(crate) fn try_acquire_auth_request(&self) -> Option<tokio::sync::OwnedSemaphorePermit> {
        self.auth_guard.try_acquire_concurrency()
    }

    pub(crate) fn try_acquire_room_api_request(&self) -> Option<tokio::sync::OwnedSemaphorePermit> {
        self.room_api_guard.try_acquire_concurrency()
    }

    pub(crate) fn try_acquire_password_work(&self) -> Option<tokio::sync::OwnedSemaphorePermit> {
        self.password_work.clone().try_acquire_owned().ok()
    }

    /// Notify active sockets and release media retained for reconnect. Version
    /// matching leaves a newer login intact if a notification is delivered late.
    pub(crate) fn revoke_account_sessions(&self, user_id: String, minimum_version: i64) {
        let _ = self
            .auth_revocations
            .send((user_id.clone(), minimum_version));
        let entries = self.grace_periods.take_revoked(&user_id, minimum_version);
        let manager = self.room_manager.clone();
        tokio::spawn(async move {
            for (room_id, participant_id, sender) in entries {
                if let Err(error) = manager
                    .remove_participant_for_sender(&room_id, &participant_id, &sender)
                    .await
                {
                    warn!(%error, "Failed to remove a revoked reconnect session");
                }
            }
        });
    }

    /// Creates the Axum router for the signaling server
    pub fn router(self) -> Router {
        use axum::routing::{delete, patch, post};

        let auth_routes = Router::new()
            .route("/register", post(crate::auth::routes::register))
            .route("/login", post(crate::auth::routes::login))
            .route("/refresh", post(crate::auth::routes::refresh))
            .route("/logout", post(crate::auth::routes::logout))
            .route(
                "/profile",
                get(crate::auth::account::get_profile)
                    .patch(crate::auth::account::update_profile)
                    .layer(DefaultBodyLimit::max(256 * 1024)),
            )
            .route("/profiles/{id}", get(crate::auth::account::public_profile))
            .route("/password", post(crate::auth::account::change_password))
            .route(
                "/recovery/key",
                post(crate::auth::account::create_recovery_key),
            )
            .route(
                "/recovery/redeem",
                post(crate::auth::account::redeem_recovery),
            )
            .route(
                "/passkey/register/start",
                post(crate::auth::routes::passkey_register_start),
            )
            .route(
                "/passkey/register/finish",
                post(crate::auth::routes::passkey_register_finish),
            )
            .route(
                "/passkey/login/start",
                post(crate::auth::routes::passkey_login_start),
            )
            .route(
                "/passkey/login/finish",
                post(crate::auth::routes::passkey_login_finish),
            )
            .layer(DefaultBodyLimit::max(16 * 1024))
            .layer(RequestBodyTimeoutLayer::new(HTTP_BODY_IDLE_TIMEOUT))
            .layer(middleware::from_fn_with_state(
                self.auth_guard.clone(),
                guarded_api_request,
            ))
            .layer(TimeoutLayer::with_status_code(
                StatusCode::REQUEST_TIMEOUT,
                HTTP_REQUEST_TIMEOUT,
            ));

        let room_routes = Router::new()
            .route("/", get(crate::room::api::list_rooms))
            .route("/", post(crate::room::api::create_room))
            .route("/mine", get(crate::room::api::owned_rooms))
            .route(
                "/{id}/identity",
                patch(crate::room::api::update_room_identity)
                    .layer(DefaultBodyLimit::max(256 * 1024)),
            )
            .route("/{id}", delete(crate::room::api::delete_room))
            .layer(RequestBodyTimeoutLayer::new(HTTP_BODY_IDLE_TIMEOUT))
            .layer(middleware::from_fn_with_state(
                self.room_api_guard.clone(),
                guarded_api_request,
            ))
            .layer(TimeoutLayer::with_status_code(
                StatusCode::REQUEST_TIMEOUT,
                HTTP_REQUEST_TIMEOUT,
            ));

        let routes = Router::new()
            .route("/ws", get(ws_handler))
            .route("/health", get(health_handler))
            .route("/ready", get(readiness_handler))
            .route("/metrics", get(metrics_handler))
            .route("/diagnostics/media", get(media_diagnostics::handler))
            .nest("/api/auth", auth_routes)
            .nest("/api/rooms", room_routes)
            .layer(middleware::from_fn_with_state(
                self.room_manager.drain_signal(),
                drain_admission,
            ))
            .with_state(self);

        with_static_fallback_and_security(routes)
    }

    /// Starts the signaling server on the specified port
    ///
    /// # Errors
    /// Returns an error if the server fails to bind to the port
    pub async fn serve(self, port: u16) -> anyhow::Result<()> {
        let bind_addr = std::env::var("BIND_ADDR").unwrap_or_else(|_| "127.0.0.1".to_string());
        let addr = format!("{bind_addr}:{port}");
        info!("Starting signaling server on {}", addr);

        let listener = tokio::net::TcpListener::bind(&addr).await?;
        let drain = self.room_manager.drain_signal();
        let app = self.router();

        // with_connect_info exposes the peer SocketAddr to ws_handler (guest ban IPs)
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .with_graceful_shutdown(async move { drain.wait().await })
        .await?;

        Ok(())
    }
}

/// Existing HTTP mutations may finish during the bounded drain; new room
/// creation and upgrade requests must not enter their handlers afterwards.
async fn drain_admission(
    State(drain): State<crate::shutdown::DrainSignal>,
    request: axum::extract::Request,
    next: middleware::Next,
) -> Response {
    let path = request.uri().path();
    if drain.is_draining()
        && (path == "/ws"
            || (request.method() == axum::http::Method::POST
                && matches!(path, "/api/rooms" | "/api/rooms/")))
    {
        return (StatusCode::SERVICE_UNAVAILABLE, "Server shutting down").into_response();
    }
    next.run(request).await
}

fn with_static_fallback_and_security(router: Router) -> Router {
    // Router::layer only wraps routes and fallbacks that already exist, so the
    // static fallback must be installed before the security middleware.
    router
        .fallback_service(ServeDir::new("web/dist"))
        .layer(DefaultBodyLimit::max(64 * 1024))
        .layer(middleware::from_fn(security_headers))
}

/// Process liveness only; dependency availability belongs to `/ready`.
async fn health_handler() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "status": "ok"
    }))
}

async fn readiness_handler(State(server): State<SignalingServer>) -> Response {
    let workers = server.room_manager.media_server().worker_manager();
    let ready = server
        .readiness
        .check(async {
            if workers.live_worker_count().await == 0 {
                return false;
            }
            if !readiness::database_ready(server.db_pool.as_ref()).await {
                return false;
            }
            // A worker may have closed while the database probe was pending.
            workers.live_worker_count().await > 0
        })
        .await;
    readiness_response(ready)
}

fn readiness_response(ready: bool) -> Response {
    (
        if ready {
            StatusCode::OK
        } else {
            StatusCode::SERVICE_UNAVAILABLE
        },
        [(header::CACHE_CONTROL, "no-store")],
        Json(serde_json::json!({"status": if ready { "ready" } else { "not_ready" }})),
    )
        .into_response()
}

/// Metrics handler — Prometheus text exposition format.
/// Protected by optional METRICS_TOKEN env var (Bearer auth).
async fn metrics_handler(State(server): State<SignalingServer>, headers: HeaderMap) -> Response {
    let Some(expected) = server.metrics_token.as_deref() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let provided = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    if !provided.is_some_and(|provided| constant_time_eq(provided.as_bytes(), expected.as_bytes()))
    {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let rooms = server.room_manager.room_count().await;
    let participants = server.room_manager.try_total_participant_count().await;
    let workers = server.room_manager.media_server().worker_manager();
    let live_workers = tokio::time::timeout(readiness::PROBE_TIMEOUT, workers.live_worker_count())
        .await
        .ok();
    let body = server
        .metrics
        .render_prometheus_snapshot(rooms, participants, live_workers);
    (
        StatusCode::OK,
        [("content-type", "text/plain; version=0.0.4; charset=utf-8")],
        body,
    )
        .into_response()
}

#[derive(Deserialize)]
struct WsParams {
    token: Option<String>,
}

/// WebSocket upgrade handler
async fn ws_handler(
    Query(params): Query<WsParams>,
    mut ws: WebSocketUpgrade,
    headers: HeaderMap,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    State(server): State<SignalingServer>,
) -> Response {
    if server.room_manager.drain_signal().is_draining() {
        return (StatusCode::SERVICE_UNAVAILABLE, "Server shutting down").into_response();
    }
    // Query strings are routinely captured by proxy and APM logs. Reject old
    // clients explicitly so an authenticated user is never silently treated as
    // a guest after bearer tokens move to the WebSocket subprotocol header.
    if params.token.is_some() {
        return (
            StatusCode::BAD_REQUEST,
            "WebSocket query tokens are no longer accepted",
        )
            .into_response();
    }

    // Client IP for guest ban enforcement. X-Forwarded-For is client-supplied
    // and trivially spoofable, so it is honored only for a loopback peer when
    // no shared secret is configured, or for a proxy presenting that secret.
    // Take the rightmost entry appended by the trusted proxy, never the
    // client-controlled leftmost. Direct connections use the socket address.
    let client_ip = trusted_client_ip(peer, &headers, server.trusted_proxy_secret.as_deref());
    if !origin_allowed(&headers, &server.allowed_origins) {
        warn!(%client_ip, "Rejected WebSocket upgrade with disallowed Origin");
        return (StatusCode::FORBIDDEN, "Origin not allowed").into_response();
    }
    if !server.ws_handshake_guard.allow(client_ip) {
        warn!(%client_ip, "WebSocket handshake rate limit reached");
        return (
            StatusCode::TOO_MANY_REQUESTS,
            [(header::RETRY_AFTER, "60")],
            "Too many connection attempts",
        )
            .into_response();
    }
    // Acquire connection permit (non-blocking)
    let permit = match server.connection_semaphore.clone().try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => {
            warn!("Connection limit reached, rejecting WebSocket upgrade");
            return (StatusCode::SERVICE_UNAVAILABLE, "Too many connections").into_response();
        }
    };

    let ip_permit = match server.ip_connection_limiter.try_acquire(client_ip) {
        Some(permit) => permit,
        None => {
            warn!(%client_ip, "Per-IP connection limit reached");
            return (StatusCode::TOO_MANY_REQUESTS, "Too many connections").into_response();
        }
    };

    let auth_token = match websocket_auth_token(&headers) {
        Ok(token) => token,
        Err(message) => return (StatusCode::BAD_REQUEST, message).into_response(),
    };

    // A supplied token must validate. Silently treating a bad token as a guest
    // creates dangerous client/server authorization state confusion.
    let auth_revocations = server.auth_revocations.subscribe();
    let authenticated_user = match auth_token.as_deref() {
        None => None,
        Some(token) => {
            let Some(secret) = server.jwt_secret() else {
                return (StatusCode::UNAUTHORIZED, "Authentication unavailable").into_response();
            };
            match crate::auth::jwt::validate_token(token, secret) {
                Ok(claims) => {
                    let Some(pool) = server.db_pool() else {
                        return (StatusCode::UNAUTHORIZED, "Authentication unavailable")
                            .into_response();
                    };
                    let Some(_auth_permit) = server.try_acquire_auth_request() else {
                        return crate::auth::types::AuthError::ServiceBusy.into_response();
                    };
                    if let Err(error) =
                        crate::auth::jwt::validate_current_claims(pool, &claims).await
                    {
                        return error.into_response();
                    }
                    Some(claims)
                }
                Err(_) => {
                    return (StatusCode::UNAUTHORIZED, "Invalid or expired token").into_response();
                }
            }
        }
    };

    // Serialize the upgrade commit with drain, after all authentication awaits.
    // An already committed upgrade observes drain in its independent writer.
    let drain = server.room_manager.drain_signal();
    let Ok(_admission) = drain.admit() else {
        return (StatusCode::SERVICE_UNAVAILABLE, "Server shutting down").into_response();
    };
    ws = ws.protocols(["simplestchat"]);
    ws.max_message_size(65_536)
        .max_frame_size(65_536)
        .on_failed_upgrade(|error| {
            warn!("WebSocket upgrade failed: {}", error);
        })
        .on_upgrade(move |socket| async move {
            let _ip_permit = ip_permit;
            connection::handle_connection(
                socket,
                server.room_manager,
                server.turn_config,
                server.grace_periods,
                server.metrics,
                permit,
                authenticated_user,
                Some(client_ip),
                server.db_pool,
                auth_revocations,
            )
            .await;
        })
}

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

fn env_u32(name: &str, default: u32) -> u32 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

fn env_bool(name: &str, default: bool) -> anyhow::Result<bool> {
    match std::env::var(name) {
        Ok(value) => parse_bool_value(name, Some(&value), default),
        Err(std::env::VarError::NotPresent) => parse_bool_value(name, None, default),
        Err(std::env::VarError::NotUnicode(_)) => anyhow::bail!("{name} must be valid UTF-8"),
    }
}

fn parse_bool_value(name: &str, value: Option<&str>, default: bool) -> anyhow::Result<bool> {
    match value {
        Some(value) if value.eq_ignore_ascii_case("true") || value == "1" => Ok(true),
        Some(value) if value.eq_ignore_ascii_case("false") || value == "0" => Ok(false),
        Some(_) => anyhow::bail!("{name} must be true, false, 1, or 0"),
        None => Ok(default),
    }
}

fn parse_allowed_origins() -> anyhow::Result<Vec<String>> {
    let Some(value) = std::env::var("ALLOWED_ORIGINS").ok() else {
        return Ok(Vec::new());
    };
    value
        .split(',')
        .map(str::trim)
        .filter(|origin| !origin.is_empty())
        .map(|origin| {
            let parsed = url::Url::parse(origin)
                .map_err(|_| anyhow::anyhow!("ALLOWED_ORIGINS contains an invalid origin"))?;
            if !matches!(parsed.scheme(), "http" | "https")
                || !parsed.username().is_empty()
                || parsed.password().is_some()
                || parsed.query().is_some()
                || parsed.fragment().is_some()
                || parsed.path() != "/"
            {
                anyhow::bail!(
                    "ALLOWED_ORIGINS entries must be exact http(s) origins without paths"
                );
            }
            Ok(parsed.origin().ascii_serialization())
        })
        .collect()
}

fn trusted_client_ip(
    peer: SocketAddr,
    headers: &HeaderMap,
    trusted_proxy_secret: Option<&str>,
) -> IpAddr {
    let proxy_authenticated = trusted_proxy_secret.is_some_and(|expected| {
        headers
            .get("x-simplestchat-proxy")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|provided| constant_time_eq(provided.as_bytes(), expected.as_bytes()))
    });
    let trust_forwarded_headers = if trusted_proxy_secret.is_some() {
        proxy_authenticated
    } else {
        peer.ip().is_loopback()
    };
    if trust_forwarded_headers {
        headers
            .get("x-forwarded-for")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(',').next_back())
            .and_then(|value| value.trim().parse().ok())
            .unwrap_or_else(|| peer.ip())
    } else {
        peer.ip()
    }
}

async fn guarded_api_request(
    State(guard): State<AuthGuard>,
    mut request: Request<Body>,
    next: Next,
) -> Response {
    if !origin_allowed(request.headers(), &guard.allowed_origins) {
        return (StatusCode::FORBIDDEN, "Origin not allowed").into_response();
    }
    let client_ip = request_client_ip(&request, guard.trusted_proxy_secret.as_deref());
    if !guard.allow(client_ip) {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            [(header::RETRY_AFTER, "60")],
            "Too many requests",
        )
            .into_response();
    }

    request.extensions_mut().insert(ClientIp(client_ip));
    // Do not acquire an operation permit here. This middleware runs before
    // Axum's JSON extractor reads the request body, so a trickled body could
    // otherwise occupy the entire auth/room pool without reaching a handler.
    // Handlers acquire the matching permit after bounded extraction completes.
    next.run(request).await
}

fn request_client_ip(request: &Request<Body>, trusted_proxy_secret: Option<&str>) -> IpAddr {
    let peer = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|connect_info| connect_info.0)
        .unwrap_or_else(|| SocketAddr::from(([127, 0, 0, 1], 0)));
    trusted_client_ip(peer, request.headers(), trusted_proxy_secret)
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

fn websocket_auth_token(headers: &HeaderMap) -> Result<Option<String>, &'static str> {
    let mut token = None;
    for value in headers.get_all(header::SEC_WEBSOCKET_PROTOCOL) {
        let value = value
            .to_str()
            .map_err(|_| "Invalid WebSocket subprotocol")?;
        for protocol in value.split(',').map(str::trim) {
            let Some(candidate) = protocol.strip_prefix(WS_AUTH_PROTOCOL_PREFIX) else {
                continue;
            };
            if candidate.is_empty() || candidate.len() > 4_096 || token.is_some() {
                return Err("Invalid WebSocket authentication protocol");
            }
            if !candidate
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
            {
                return Err("Invalid WebSocket authentication protocol");
            }
            token = Some(candidate.to_string());
        }
    }
    Ok(token)
}

fn origin_allowed(headers: &HeaderMap, allowlist: &[String]) -> bool {
    let Some(origin) = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
    else {
        // Native/non-browser WebSocket clients generally omit Origin.
        return true;
    };
    let Ok(origin_url) = url::Url::parse(origin) else {
        return false;
    };
    if !matches!(origin_url.scheme(), "http" | "https")
        || !origin_url.username().is_empty()
        || origin_url.password().is_some()
        || origin_url.path() != "/"
        || origin_url.query().is_some()
        || origin_url.fragment().is_some()
    {
        return false;
    }
    let normalized_origin = origin_url.origin().ascii_serialization();
    if !allowlist.is_empty() {
        return allowlist
            .iter()
            .any(|allowed| allowed.eq_ignore_ascii_case(&normalized_origin));
    }

    let Some(host) = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    let Ok(host_authority) = host.parse::<axum::http::uri::Authority>() else {
        return false;
    };
    // With no explicit allowlist the application is in its loopback-only
    // development mode. Merely comparing Origin to Host is insufficient here:
    // an attacker-controlled DNS name can resolve to 127.0.0.1 and make both
    // headers agree. Require a literal loopback host; custom local names must
    // opt in through ALLOWED_ORIGINS just like production names do.
    if !is_loopback_host(host_authority.host()) {
        return false;
    }
    if !origin_url
        .host_str()
        .is_some_and(|origin_host| origin_host.eq_ignore_ascii_case(host_authority.host()))
    {
        return false;
    }
    match host_authority.port_u16() {
        Some(port) => origin_url.port_or_known_default() == Some(port),
        None => origin_url.port().is_none(),
    }
}

fn is_loopback_host(host: &str) -> bool {
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    let unbracketed = host
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .unwrap_or(host);
    unbracketed
        .parse::<IpAddr>()
        .is_ok_and(|address| address.is_loopback())
}

async fn security_headers(request: Request<Body>, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(
        header::STRICT_TRANSPORT_SECURITY,
        HeaderValue::from_static("max-age=31536000; includeSubDomains"),
    );
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'self'; base-uri 'none'; object-src 'none'; frame-ancestors 'none'; form-action 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; media-src 'self' blob:; connect-src 'self'",
        ),
    );
    headers.insert(
        header::HeaderName::from_static("permissions-policy"),
        HeaderValue::from_static("camera=(self), microphone=(self), display-capture=(self)"),
    );
    headers.insert(
        header::HeaderName::from_static("x-frame-options"),
        HeaderValue::from_static("DENY"),
    );
    headers.insert(
        header::HeaderName::from_static("cross-origin-opener-policy"),
        HeaderValue::from_static("same-origin"),
    );
    headers.insert(
        header::HeaderName::from_static("cross-origin-resource-policy"),
        HeaderValue::from_static("same-origin"),
    );
    if response.status().is_success()
        && response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|content_type| content_type.starts_with("application/json"))
    {
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
    response
}

#[cfg(test)]
mod security_tests {
    use super::*;

    #[tokio::test]
    async fn shutdown_rejects_new_room_and_socket_requests_but_keeps_liveness() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        async fn response(address: std::net::SocketAddr, method: &str, path: &str) -> String {
            let mut connection = tokio::net::TcpStream::connect(address).await.unwrap();
            connection.write_all(format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Length: 0\r\n\r\n").as_bytes()).await.unwrap();
            let mut body = String::new();
            connection.read_to_string(&mut body).await.unwrap();
            body
        }
        let drain = crate::shutdown::DrainSignal::default();
        let router = Router::new()
            .route("/ws", get(|| async { StatusCode::OK }))
            .route(
                "/api/rooms",
                get(|| async { StatusCode::OK }).post(|| async { StatusCode::OK }),
            )
            .route("/health", get(health_handler))
            .layer(middleware::from_fn_with_state(
                drain.clone(),
                drain_admission,
            ));
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let check = tokio::time::timeout(Duration::from_secs(2), async {
            let initial = response(address, "POST", "/api/rooms").await;
            drain.begin_draining();
            let mut responses = vec![(initial, 200)];
            for (method, path, status) in [
                ("POST", "/api/rooms", 503),
                ("GET", "/ws", 503),
                ("GET", "/api/rooms", 200),
                ("GET", "/health", 200),
            ] {
                responses.push((response(address, method, path).await, status));
            }
            responses
        })
        .await;
        server.abort();
        let _ = server.await;
        for (body, status) in check.unwrap() {
            assert!(body.starts_with(&format!("HTTP/1.1 {status} ")));
        }
    }

    #[tokio::test]
    async fn readiness_responses_are_uncached_generic_and_separate_from_liveness() {
        for (ready, expected_status, expected_body) in [
            (true, StatusCode::OK, r#"{"status":"ready"}"#),
            (
                false,
                StatusCode::SERVICE_UNAVAILABLE,
                r#"{"status":"not_ready"}"#,
            ),
        ] {
            let response = readiness_response(ready);
            assert_eq!(response.status(), expected_status);
            assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
            let body = axum::body::to_bytes(response.into_body(), 1024)
                .await
                .unwrap();
            assert_eq!(body, expected_body);
        }
        assert_eq!(
            health_handler().await.0,
            serde_json::json!({"status": "ok"})
        );
    }

    #[tokio::test]
    async fn stalled_json_body_does_not_hold_operation_concurrency() {
        use axum::routing::post;
        use tokio::io::AsyncWriteExt;

        async fn json_handler(Json(_body): Json<serde_json::Value>) -> StatusCode {
            StatusCode::NO_CONTENT
        }

        async fn announce_body_pipeline(
            State(entered): State<tokio::sync::mpsc::Sender<()>>,
            request: Request<Body>,
            next: Next,
        ) -> Response {
            let _ = entered.send(()).await;
            next.run(request).await
        }

        let guard = AuthGuard::new(10, 1, Arc::new(None), Arc::new(Vec::new()));
        let (entered_tx, mut entered_rx) = tokio::sync::mpsc::channel(1);
        let app = Router::new()
            .route("/json", post(json_handler))
            .layer(DefaultBodyLimit::max(16 * 1024))
            .layer(RequestBodyTimeoutLayer::new(Duration::from_secs(5)))
            // This probe is inside the request guard and immediately outside
            // the body extractor. Reaching it proves the guard has admitted the
            // request while the deliberately incomplete body still blocks JSON.
            .layer(middleware::from_fn_with_state(
                entered_tx,
                announce_body_pipeline,
            ))
            .layer(middleware::from_fn_with_state(
                guard.clone(),
                guarded_api_request,
            ));

        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let mut connection = tokio::net::TcpStream::connect(address).await.unwrap();
        connection
            .write_all(
                b"POST /json HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{",
            )
            .await
            .unwrap();

        tokio::time::timeout(Duration::from_secs(1), entered_rx.recv())
            .await
            .expect("request should enter the body pipeline")
            .expect("body-pipeline probe should remain connected");

        let permit = guard.try_acquire_concurrency();
        assert!(
            permit.is_some(),
            "an incomplete request body must not occupy operation concurrency"
        );

        drop(connection);
        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn static_fallback_is_wrapped_in_security_headers() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, with_static_fallback_and_security(Router::new()))
                .await
                .unwrap();
        });

        let mut connection = tokio::net::TcpStream::connect(address).await.unwrap();
        connection
            .write_all(
                b"GET /definitely-missing-security-header-test HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
            )
            .await
            .unwrap();
        let mut response = Vec::new();
        connection.read_to_end(&mut response).await.unwrap();
        server.abort();

        let response = String::from_utf8(response).unwrap().to_ascii_lowercase();
        assert!(response.starts_with("http/1.1 404"));
        assert!(response.contains("x-content-type-options: nosniff\r\n"));
        assert!(response.contains("content-security-policy:"));
    }

    #[test]
    fn auth_rate_guard_enforces_limit_and_recovers_next_window() {
        let guard = AuthGuard::new(2, 1, Arc::new(None), Arc::new(Vec::new()));
        let ip = IpAddr::from([192, 0, 2, 1]);
        assert!(guard.allow(ip));
        assert!(guard.allow(ip));
        assert!(!guard.allow(ip));
    }

    #[test]
    fn registration_defaults_closed_and_requires_explicit_opt_in() {
        assert!(
            !parse_bool_value(
                "REGISTRATION_ENABLED",
                None,
                REGISTRATION_ENABLED_BY_DEFAULT,
            )
            .unwrap()
        );
        assert!(
            parse_bool_value(
                "REGISTRATION_ENABLED",
                Some("true"),
                REGISTRATION_ENABLED_BY_DEFAULT,
            )
            .unwrap()
        );
    }

    #[test]
    fn account_rate_sketch_keeps_distinct_principals_usable() {
        let limiter = PrincipalRateLimiter::new(2);
        assert!(limiter.allow("alice@example.com"));
        assert!(limiter.allow("alice@example.com"));
        assert!(!limiter.allow("alice@example.com"));
        assert!(limiter.allow("bob@example.com"));
        assert!(limiter.allow("bob@example.com"));
        assert!(!limiter.allow("bob@example.com"));
    }

    #[test]
    fn account_rate_sketch_churn_cannot_reset_a_limited_victim() {
        let limiter = PrincipalRateLimiter::new(8);
        let now = Instant::now();
        for _ in 0..8 {
            assert!(limiter.allow_at("victim@example.test", now));
        }
        assert!(!limiter.allow_at("victim@example.test", now));

        for index in 0..(MAX_TRACKED_AUTH_IPS + 1_000) {
            let _ = limiter.allow_at(&format!("attacker-{index}@example.test"), now);
        }

        assert!(!limiter.allow_at("victim@example.test", now));
        assert!(limiter.allow_at("legitimate@example.test", now));
    }

    #[test]
    fn account_rate_sketch_resets_after_the_window() {
        let limiter = PrincipalRateLimiter::new(1);
        let now = Instant::now();
        assert!(limiter.allow_at("alice@example.test", now));
        assert!(!limiter.allow_at("alice@example.test", now));
        assert!(limiter.allow_at("alice@example.test", now + PRINCIPAL_RATE_WINDOW));
    }

    #[test]
    fn websocket_origin_defaults_to_literal_loopback_host() {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, HeaderValue::from_static("127.0.0.1:3000"));
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("http://127.0.0.1:3000"),
        );
        assert!(origin_allowed(&headers, &[]));

        // An attacker-controlled hostname may resolve to loopback. Matching
        // Origin and Host must not be enough to access a local server.
        headers.insert(header::HOST, HeaderValue::from_static("evil.example:3000"));
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("http://evil.example:3000"),
        );
        assert!(!origin_allowed(&headers, &[]));

        assert!(origin_allowed(
            &headers,
            &["http://evil.example:3000".to_string()]
        ));
    }

    #[test]
    fn forwarded_ip_requires_an_authenticated_proxy() {
        let peer = SocketAddr::from(([172, 18, 0, 1], 43210));
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", HeaderValue::from_static("198.51.100.7"));

        assert_eq!(trusted_client_ip(peer, &headers, None), peer.ip());
        headers.insert(
            "x-simplestchat-proxy",
            HeaderValue::from_static("a-random-proxy-secret-at-least-32-bytes"),
        );
        assert_eq!(
            trusted_client_ip(
                peer,
                &headers,
                Some("a-random-proxy-secret-at-least-32-bytes"),
            ),
            IpAddr::from([198, 51, 100, 7])
        );

        let loopback_peer = SocketAddr::from(([127, 0, 0, 1], 43210));
        headers.remove("x-simplestchat-proxy");
        assert_eq!(
            trusted_client_ip(
                loopback_peer,
                &headers,
                Some("a-random-proxy-secret-at-least-32-bytes")
            ),
            loopback_peer.ip()
        );
    }

    #[test]
    fn websocket_bearer_is_extracted_from_subprotocol_header() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::SEC_WEBSOCKET_PROTOCOL,
            HeaderValue::from_static("simplestchat, auth.header.payload.signature"),
        );
        assert_eq!(
            websocket_auth_token(&headers).unwrap().as_deref(),
            Some("header.payload.signature")
        );

        headers.insert(
            header::SEC_WEBSOCKET_PROTOCOL,
            HeaderValue::from_static("auth.first, auth.second"),
        );
        assert!(websocket_auth_token(&headers).is_err());
    }

    #[test]
    fn ipv6_rate_limits_are_scoped_to_a_64_bit_prefix() {
        let first: IpAddr = "2001:db8:1:2::1".parse().unwrap();
        let rotated: IpAddr = "2001:db8:1:2:ffff::42".parse().unwrap();
        assert_eq!(rate_limit_ip(first), rate_limit_ip(rotated));

        let mapped_first: IpAddr = "::ffff:192.0.2.1".parse().unwrap();
        let mapped_second: IpAddr = "::ffff:192.0.2.2".parse().unwrap();
        let canonical_first: IpAddr = "192.0.2.1".parse().unwrap();
        assert_eq!(rate_limit_ip(mapped_first), canonical_first);
        assert_ne!(rate_limit_ip(mapped_first), rate_limit_ip(mapped_second));
    }
}
