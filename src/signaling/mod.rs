#![forbid(unsafe_code)]

// Signaling module - WebSocket signaling server

#[cfg(test)]
mod authorization_tests;
pub(crate) mod connection;
mod media_diagnostics;
pub(crate) mod outbound;
pub mod protocol;
mod readiness;
mod routes;
#[cfg(test)]
mod security_tests;
pub(crate) mod telemetry;

use crate::auth::webauthn::ChallengeStore;
use crate::metrics::ServerMetrics;
use crate::room::RoomManager;
use crate::turn::TurnConfig;
use axum::{
    Json, Router,
    body::Body,
    extract::{ConnectInfo, DefaultBodyLimit, MatchedPath, Query, State, ws::WebSocketUpgrade},
    http::{HeaderMap, HeaderValue, Request, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
};
use serde::{Deserialize, Serialize};
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
use tracing::{Instrument, info, warn};

pub(crate) use connection::GracePeriodMap;

#[derive(Clone, Copy)]
pub(crate) struct ClientIp(pub(crate) IpAddr);

/// How long an avatar or room image request queues for the media lane.
const PROFILE_PERMIT_WAIT: Duration = Duration::from_secs(5);
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
const HTTP_UPLOAD_REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
const HTTP_BODY_IDLE_TIMEOUT: Duration = Duration::from_secs(5);
const WS_AUTH_PROTOCOL_PREFIX: &str = "ticket.";

#[derive(Debug)]
struct AuthRateEntry {
    window_started: Instant,
    requests: u32,
}

struct AuthRateTable {
    entries: lru::LruCache<IpAddr, AuthRateEntry>,
}

impl Default for AuthRateTable {
    fn default() -> Self {
        Self {
            entries: lru::LruCache::new(std::num::NonZeroUsize::new(MAX_TRACKED_AUTH_IPS).unwrap()),
        }
    }
}

#[derive(Clone)]
struct AuthGuard {
    entries: Arc<Mutex<AuthRateTable>>,
    max_requests_per_minute: u32,
    concurrency: Arc<Semaphore>,
    trusted_proxy_secret: Arc<Option<String>>,
    allowed_origins: Arc<Vec<String>>,
    metrics: ServerMetrics,
}

impl AuthGuard {
    fn new(
        max_requests_per_minute: u32,
        max_concurrency: usize,
        trusted_proxy_secret: Arc<Option<String>>,
        allowed_origins: Arc<Vec<String>>,
        metrics: ServerMetrics,
    ) -> Self {
        Self {
            entries: Arc::new(Mutex::new(AuthRateTable::default())),
            max_requests_per_minute,
            concurrency: Arc::new(Semaphore::new(max_concurrency)),
            trusted_proxy_secret,
            allowed_origins,
            metrics,
        }
    }

    fn allow(&self, ip: IpAddr) -> bool {
        self.allow_at(ip, Instant::now())
    }

    fn allow_at(&self, ip: IpAddr, now: Instant) -> bool {
        let ip = crate::net::cohort(ip);
        let mut table = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        // At capacity, evict a cold cohort rather than imposing one stranger's
        // exhausted allowance on every untracked address. Global semaphores
        // remain independent of this best-effort per-address rate memory.
        let entry = table.entries.get_or_insert_mut(ip, || AuthRateEntry {
            window_started: now,
            requests: 0,
        });
        if now.duration_since(entry.window_started) >= PRINCIPAL_RATE_WINDOW {
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
        let ip = crate::net::cohort(ip);
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
pub(crate) struct SignalingServer {
    room_manager: Arc<RoomManager>,
    turn_config: Option<Arc<TurnConfig>>,
    grace_periods: GracePeriodMap,
    metrics: ServerMetrics,
    readiness: readiness::ReadinessState,
    connection_semaphore: Arc<Semaphore>,
    max_connections: usize,
    /// `BIND_ADDR:PORT`.
    listen: std::net::SocketAddr,
    ip_connection_limiter: IpConnectionLimiter,
    ws_handshake_guard: AuthGuard,
    auth_guard: AuthGuard,
    account_guard: AuthGuard,
    profile_guard: AuthGuard,
    password_work: Arc<Semaphore>,
    max_password_work: usize,
    session_cleanup: crate::auth::session::SessionCleanup,
    principal_auth_limiter: PrincipalRateLimiter,
    room_creation_limiter: PrincipalRateLimiter,
    sign_in_failures: crate::auth::limiter::FailureLimiter,
    registration_limiter: Arc<crate::auth::limiter::WindowLimiter>,
    room_api_guard: AuthGuard,
    telemetry_guard: AuthGuard,
    db_pool: Option<PgPool>,
    jwt_secret: Option<String>,
    ws_tickets: Arc<crate::auth::ws_tickets::TicketStore>,
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
    pub(crate) fn new(
        room_manager: Arc<RoomManager>,
        metrics: ServerMetrics,
        db_pool: Option<PgPool>,
        config: &crate::config::Config,
    ) -> anyhow::Result<Self> {
        let max_connections = config.limits.max_connections;
        info!("Max connections: {}", max_connections);

        let jwt_secret = config
            .auth
            .jwt_secret
            .as_ref()
            .map(|secret| secret.expose_secret().to_owned());
        if jwt_secret.is_some() {
            info!("JWT authentication enabled");
        } else {
            info!("JWT_SECRET not set — authentication disabled");
        }
        let metrics_token = config
            .observability
            .metrics_token
            .as_ref()
            .map(|token| token.expose_secret().to_owned());
        let media_diagnostics = media_diagnostics::MediaDiagnostics::configure(
            config.observability.media_diagnostics_enabled,
            metrics_token.as_deref(),
        )?;
        let trusted_proxy_secret = Arc::new(
            config
                .network
                .trusted_proxy_secret
                .as_ref()
                .map(|secret| secret.expose_secret().to_owned()),
        );

        let rates = &config.rates;
        let max_connections_per_ip = config.limits.max_connections_per_ip;
        let auth_requests_per_minute = rates.auth_requests_per_minute;
        let auth_requests_per_account_per_minute = rates.auth_requests_per_account_per_minute;
        let registrations_per_ip_per_hour = rates.registrations_per_ip_per_hour;
        let auth_concurrency = rates.auth_concurrency;
        let account_requests_per_minute = rates.account_requests_per_minute;
        let account_concurrency = rates.account_concurrency;
        let password_workers = password_lane_capacity(
            config.limits.max_password_workers,
            auth_concurrency,
            std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get),
        );
        info!(
            password_workers,
            auth_concurrency, "Account password verification lane sized"
        );
        let profile_requests_per_minute = rates.profile_requests_per_minute;
        let room_api_requests_per_minute = rates.room_api_requests_per_minute;
        let room_api_concurrency = rates.room_api_concurrency;
        let room_creations_per_account = rates.room_creations_per_account_per_minute;
        let ws_handshakes_per_minute = rates.ws_handshakes_per_minute;
        let allowed_origins = Arc::new(config.network.allowed_origins.clone());
        let registration_enabled = config.auth.registration_enabled;
        let max_users = config.limits.max_users;

        let (webauthn, challenge_store) = match &config.auth.webauthn {
            Some(settings) => {
                let (webauthn, store) = crate::auth::webauthn::init_webauthn(settings)?;
                (Some(Arc::new(webauthn)), Some(store))
            }
            None => (None, None),
        };
        let turn_config = config.turn.clone();
        let listen = std::net::SocketAddr::new(config.network.bind_addr, config.network.port);

        let readiness = readiness::ReadinessState::new(room_manager.drain_signal());
        Ok(Self {
            room_manager,
            turn_config: turn_config.map(Arc::new),
            // Grace state does not hold a live-connection permit. Bound it
            // separately at the same configured process limit so disconnect
            // churn cannot grow retained room/media sessions without limit.
            grace_periods: GracePeriodMap::with_capacity(max_connections),
            metrics: metrics.clone(),
            readiness,
            connection_semaphore: Arc::new(Semaphore::new(max_connections)),
            max_connections,
            listen,
            ip_connection_limiter: IpConnectionLimiter::new(max_connections_per_ip),
            ws_handshake_guard: AuthGuard::new(
                ws_handshakes_per_minute,
                1,
                trusted_proxy_secret.clone(),
                allowed_origins.clone(),
                metrics.clone(),
            ),
            auth_guard: AuthGuard::new(
                auth_requests_per_minute,
                auth_concurrency,
                trusted_proxy_secret.clone(),
                allowed_origins.clone(),
                metrics.clone(),
            ),
            account_guard: AuthGuard::new(
                account_requests_per_minute,
                account_concurrency,
                trusted_proxy_secret.clone(),
                allowed_origins.clone(),
                metrics.clone(),
            ),
            profile_guard: AuthGuard::new(
                profile_requests_per_minute,
                auth_concurrency,
                trusted_proxy_secret.clone(),
                allowed_origins.clone(),
                metrics.clone(),
            ),
            // Keep the permit alive inside each spawn_blocking Argon2 job. The
            // outer HTTP timeout may cancel a handler, but it must not release
            // capacity while that CPU-heavy job is still running.
            password_work: Arc::new(Semaphore::new(password_workers)),
            max_password_work: password_workers,
            session_cleanup: crate::auth::session::SessionCleanup::default(),
            principal_auth_limiter: PrincipalRateLimiter::new(auth_requests_per_account_per_minute),
            room_creation_limiter: PrincipalRateLimiter::new(room_creations_per_account),
            sign_in_failures: crate::auth::limiter::FailureLimiter::new(),
            registration_limiter: Arc::new(crate::auth::limiter::WindowLimiter::new(
                registrations_per_ip_per_hour,
                Duration::from_secs(3600),
            )),
            room_api_guard: AuthGuard::new(
                room_api_requests_per_minute,
                room_api_concurrency,
                trusted_proxy_secret.clone(),
                allowed_origins.clone(),
                metrics.clone(),
            ),
            telemetry_guard: AuthGuard::new(
                30,
                8,
                trusted_proxy_secret.clone(),
                allowed_origins.clone(),
                metrics.clone(),
            ),
            db_pool,
            jwt_secret,
            ws_tickets: Arc::new(crate::auth::ws_tickets::TicketStore::new()),
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

    pub(crate) fn room_manager(&self) -> &RoomManager {
        &self.room_manager
    }

    /// Closes room/upgrade admission, wakes socket writers, stops HTTP accepts,
    /// and cancels retained reconnect timers. Room/media cleanup is coordinated
    /// separately, including membership retained by those cancelled timers.
    pub(crate) fn begin_draining(&self) {
        self.readiness.begin_draining();
        let retained_sessions = self.grace_periods.close();
        info!(
            retained_sessions,
            "Signaling admission closed; reconnect grace cancelled"
        );
    }

    /// Upgraded WebSockets and blocking password jobs can outlive their HTTP
    /// callers. Wait for their owned permits too; the coordinator bounds this.
    pub(crate) async fn wait_for_connections(&self) {
        while self.connection_count() > 0
            || self.pending_password_work() > 0
            || self.session_cleanup.pending() > 0
        {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

    /// The single-flight gate for the opportunistic expired-session sweep.
    pub(crate) fn session_cleanup(&self) -> &crate::auth::session::SessionCleanup {
        &self.session_cleanup
    }

    /// Includes accepted upgrades and any authentication work holding a permit.
    pub(crate) fn connection_count(&self) -> usize {
        self.max_connections
            .saturating_sub(self.connection_semaphore.available_permits())
    }

    /// Password permits remain owned by blocking jobs even after cancellation
    /// of a request. Counting both lanes makes incomplete CPU cleanup visible.
    pub(crate) fn pending_password_work(&self) -> usize {
        self.max_password_work
            .saturating_sub(self.password_work.available_permits())
            + self.room_manager.pending_password_work()
    }

    pub(crate) fn db_pool(&self) -> Option<&PgPool> {
        self.db_pool.as_ref()
    }

    /// Contact URI for VAPID; configured origins are already validated at startup.
    pub(crate) fn notification_origin(&self) -> Option<String> {
        self.allowed_origins
            .iter()
            .find(|origin| origin.starts_with("https://"))
            .cloned()
    }

    pub(crate) fn jwt_secret(&self) -> Option<&str> {
        self.jwt_secret.as_deref()
    }

    pub(crate) fn webauthn(&self) -> Option<&webauthn_rs::prelude::Webauthn> {
        self.webauthn.as_deref()
    }

    pub(crate) fn challenge_store(&self) -> Option<&ChallengeStore> {
        self.challenge_store.as_deref()
    }

    pub(crate) fn websocket_tickets(&self) -> &crate::auth::ws_tickets::TicketStore {
        &self.ws_tickets
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

    /// Whole seconds this client must wait before another password attempt on
    /// the account, earned by its earlier failures; `None` means try now.
    pub(crate) fn sign_in_wait(&self, address: IpAddr, account: &str) -> Option<u64> {
        self.sign_in_failures.wait(address, account)
    }

    pub(crate) fn note_sign_in_failure(&self, address: IpAddr, account: &str) {
        self.sign_in_failures.record_failure(address, account);
    }

    pub(crate) fn note_sign_in_success(&self, address: IpAddr, account: &str) {
        self.sign_in_failures.record_success(address, account);
    }

    /// One of this address's hourly registration answers, taken if available.
    pub(crate) fn allow_registration(&self, address: IpAddr) -> bool {
        self.registration_limiter.allow(address)
    }

    pub(crate) fn allow_room_creation(&self, user_id: &str) -> bool {
        self.room_creation_limiter.allow(user_id)
    }

    pub(crate) fn try_acquire_auth_request(&self) -> Option<tokio::sync::OwnedSemaphorePermit> {
        self.auth_guard.try_acquire_concurrency()
    }

    pub(crate) fn try_acquire_account_request(&self) -> Option<tokio::sync::OwnedSemaphorePermit> {
        self.account_guard.try_acquire_concurrency()
    }

    /// A permit from the public media lane (avatars and room images). A page's
    /// burst of thumbnails waits briefly for one instead of failing, and never
    /// spends the sign-in lane's permits.
    pub(crate) async fn acquire_profile_request(
        &self,
    ) -> Result<tokio::sync::OwnedSemaphorePermit, crate::auth::types::AuthError> {
        tokio::time::timeout(
            PROFILE_PERMIT_WAIT,
            self.profile_guard.concurrency.clone().acquire_owned(),
        )
        .await
        .ok()
        .and_then(Result::ok)
        .ok_or(crate::auth::types::AuthError::ServiceBusy)
    }

    pub(crate) fn try_acquire_room_api_request(&self) -> Option<tokio::sync::OwnedSemaphorePermit> {
        self.room_api_guard.try_acquire_concurrency()
    }

    pub(crate) fn try_acquire_password_work(&self) -> Option<tokio::sync::OwnedSemaphorePermit> {
        let permit = self.password_work.clone().try_acquire_owned().ok();
        if permit.is_none() {
            self.metrics.inc_password_work_rejected();
        }
        permit
    }

    /// Notify active sockets and release media retained for reconnect. Version
    /// matching leaves a newer login intact if a notification is delivered late.
    pub(crate) fn revoke_account_sessions(&self, user_id: String, minimum_version: i64) {
        if let Ok(account) = user_id.parse() {
            self.room_manager.account_updates().revalidate(account);
        }
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
    /// Serves HTTP and WebSockets on `BIND_ADDR:PORT` until the drain signal.
    ///
    /// # Errors
    /// Returns an error if the listener cannot bind.
    pub(crate) async fn serve(self) -> anyhow::Result<()> {
        let addr = self.listen;
        info!("Starting signaling server on {}", addr);

        let listener = tuned_listener(tokio::net::TcpListener::bind(addr).await?);
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

/// Public feature availability, independent of the caller's authorization.
/// It contains no deployment addresses, secrets or internal dependency errors.
async fn capabilities_handler(State(server): State<SignalingServer>) -> Response {
    let accounts = server.db_pool.is_some() && server.jwt_secret.is_some();
    let passkeys = accounts && server.webauthn.is_some();
    (
        [(header::CACHE_CONTROL, "no-store")],
        Json(Capabilities::new(
            accounts,
            passkeys,
            server.registration_enabled,
            server.db_pool.is_some(),
            server.room_manager.allow_ad_hoc_rooms(),
        )),
    )
        .into_response()
}

/// What `/api/capabilities` publishes: exactly the keys the web client's
/// `decodeServerCapabilities` reads, and nothing about addresses, secrets or
/// dependency errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct Capabilities {
    version: u8,
    accounts: bool,
    password_login: bool,
    passkey_login: bool,
    password_registration: RegistrationMode,
    passkey_registration: RegistrationMode,
    room_directory: bool,
    room_creation: bool,
    ad_hoc_rooms: bool,
}

/// How an account can be created, if at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum RegistrationMode {
    Open,
    Invite,
    Disabled,
}

impl Capabilities {
    fn new(
        accounts: bool,
        passkeys: bool,
        registration: bool,
        directory: bool,
        ad_hoc: bool,
    ) -> Self {
        let password_registration = if !accounts {
            RegistrationMode::Disabled
        } else if registration {
            RegistrationMode::Open
        } else {
            RegistrationMode::Invite
        };
        let passkey_registration = if accounts && passkeys && registration {
            RegistrationMode::Open
        } else {
            RegistrationMode::Disabled
        };
        Self {
            version: 1,
            accounts,
            password_login: accounts,
            passkey_login: accounts && passkeys,
            password_registration,
            passkey_registration,
            room_directory: directory,
            room_creation: accounts,
            ad_hoc_rooms: ad_hoc,
        }
    }
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
            // Saturation is a capacity signal: a balancer should route new
            // users elsewhere while existing calls continue.
            if server.room_manager.saturated() {
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
    let mut body = server
        .metrics
        .render_prometheus_snapshot(rooms, participants, live_workers);
    crate::metrics::append_gauge(
        &mut body,
        "simplestchat_connection_permits_in_use",
        "Connection permits held by accepted sockets and by handshake authentication work; MAX_CONNECTIONS is enforced against this, not against connections_active",
        server.connection_count() as u64,
    );
    use std::fmt::Write as _;
    crate::db::append_error_metrics(&mut body);
    let revision = std::env::var("SOURCE_REVISION")
        .ok()
        .filter(|value| value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .unwrap_or_else(|| "unknown".to_owned());
    let _ = writeln!(
        body,
        "# HELP simplestchat_build_info Deployed server source revision\n# TYPE simplestchat_build_info gauge\nsimplestchat_build_info{{revision=\"{revision}\"}} 1"
    );
    if let Some(pool) = &server.db_pool {
        let total = pool.size() as u64;
        let idle = pool.num_idle() as u64;
        let _ = writeln!(
            body,
            "# HELP simplestchat_db_pool_connections Current idle and in-use connections plus configured maximum; component reads are not atomic\n# TYPE simplestchat_db_pool_connections gauge"
        );
        for (state, count) in [
            ("idle", idle),
            ("used", total.saturating_sub(idle)),
            ("max", u64::from(pool.options().get_max_connections())),
        ] {
            let _ = writeln!(
                body,
                "simplestchat_db_pool_connections{{state=\"{state}\"}} {count}"
            );
        }
    }
    let _ = writeln!(
        body,
        "# HELP simplestchat_password_work_in_use Password jobs holding permits including jobs whose request was cancelled\n# TYPE simplestchat_password_work_in_use gauge"
    );
    for (lane, count) in [
        (
            "auth",
            server
                .max_password_work
                .saturating_sub(server.password_work.available_permits()),
        ),
        ("room", server.room_manager.pending_password_work()),
    ] {
        let _ = writeln!(
            body,
            "simplestchat_password_work_in_use{{lane=\"{lane}\"}} {count}"
        );
    }
    let _ = writeln!(
        body,
        "# HELP simplestchat_password_work_capacity Configured maximum concurrent password jobs\n# TYPE simplestchat_password_work_capacity gauge"
    );
    for (lane, capacity) in [
        ("auth", server.max_password_work),
        ("room", server.room_manager.password_work_capacity()),
    ] {
        let _ = writeln!(
            body,
            "simplestchat_password_work_capacity{{lane=\"{lane}\"}} {capacity}"
        );
    }
    (
        StatusCode::OK,
        [("content-type", "text/plain; version=0.0.4; charset=utf-8")],
        body,
    )
        .into_response()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WsParams {}

/// WebSocket upgrade handler
async fn ws_handler(
    Query(_params): Query<WsParams>,
    mut ws: WebSocketUpgrade,
    headers: HeaderMap,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    State(server): State<SignalingServer>,
) -> Response {
    if server.room_manager.drain_signal().is_draining() {
        return (StatusCode::SERVICE_UNAVAILABLE, "Server shutting down").into_response();
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
        if server
            .metrics
            .telemetry()
            .warning_should_log(telemetry::WarningFamily::Handshake)
        {
            warn!("WebSocket handshake rate limit reached; further instances counted in metrics");
        }
        server.metrics.inc_upgrade_rejected();
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
            server.metrics.inc_upgrade_rejected();
            warn!("Connection limit reached, rejecting WebSocket upgrade");
            return (StatusCode::SERVICE_UNAVAILABLE, "Too many connections").into_response();
        }
    };

    let ip_permit = match server.ip_connection_limiter.try_acquire(client_ip) {
        Some(permit) => permit,
        None => {
            server.metrics.inc_upgrade_rejected();
            warn!(%client_ip, "Per-IP connection limit reached");
            return (StatusCode::TOO_MANY_REQUESTS, "Too many connections").into_response();
        }
    };

    let auth_token = match websocket_auth_ticket(&headers) {
        Ok(token) => token,
        Err(message) => return (StatusCode::BAD_REQUEST, message).into_response(),
    };

    // Subscribe before revocation validation so a credential change racing the
    // upgrade is still observed by the established connection handler.
    let auth_revocations = server.auth_revocations.subscribe();
    let authenticated_user = match auth_token.as_deref() {
        None => None,
        Some(ticket) => {
            let Some(pool) = server.db_pool() else {
                return (StatusCode::UNAUTHORIZED, "Authentication unavailable").into_response();
            };
            let Some(_auth_permit) = server.try_acquire_auth_request() else {
                return crate::auth::types::AuthError::ServiceBusy.into_response();
            };
            match server.websocket_tickets().redeem(pool, ticket).await {
                Ok(claims) => Some(claims),
                Err(error) => return error.into_response(),
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
    // tungstenite reserves the read buffer eagerly per socket; signaling
    // messages are a few hundred bytes and are capped at 64 KiB, so the 128 KiB
    // default only inflates per-connection memory.
    ws.max_message_size(65_536)
        .max_frame_size(65_536)
        .read_buffer_size(16 * 1024)
        .on_failed_upgrade(|error| {
            warn!("WebSocket upgrade failed: {}", error);
        })
        .on_upgrade(move |socket| async move {
            let _ip_permit = ip_permit;
            let renewal_authenticator = authenticated_user.as_ref().and_then(|_| {
                server.jwt_secret.clone().map(|secret| {
                    connection::RenewalAuthenticator::new(
                        secret.into(),
                        server.auth_guard.concurrency.clone(),
                    )
                })
            });
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
                renewal_authenticator,
            )
            .await;
        })
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

/// Outermost route layer observes responses from body, deadline, origin, rate
/// and drain guards. Only Axum's matched template enters metrics or logs.
async fn observe_http_request(
    State(metrics): State<ServerMetrics>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let started = Instant::now();
    let route = request.extensions().get::<MatchedPath>().cloned();
    let method = request.method().as_str().to_owned();
    let request_id = uuid::Uuid::new_v4().to_string();
    let span = tracing::info_span!("http_request", request_id = %request_id, route = telemetry::route_label(route.as_ref()));
    let mut response = next.run(request).instrument(span).await;
    metrics.telemetry().record_http(
        route.as_ref(),
        &method,
        response.status().as_u16(),
        started.elapsed(),
    );
    response.headers_mut().insert(
        "x-request-id",
        HeaderValue::from_str(&request_id).expect("UUID is a header value"),
    );
    response
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
        guard.metrics.inc_api_request_rejected();
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
    let response = next.run(request).await;
    if matches!(
        response.status(),
        StatusCode::TOO_MANY_REQUESTS | StatusCode::SERVICE_UNAVAILABLE
    ) {
        guard.metrics.inc_api_request_rejected();
    }
    response
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

fn websocket_auth_ticket(headers: &HeaderMap) -> Result<Option<String>, &'static str> {
    let mut ticket = None;
    for value in headers.get_all(header::SEC_WEBSOCKET_PROTOCOL) {
        let value = value
            .to_str()
            .map_err(|_| "Invalid WebSocket subprotocol")?;
        for protocol in value.split(',').map(str::trim) {
            if protocol == "simplestchat" {
                continue;
            }
            let candidate = protocol
                .strip_prefix(WS_AUTH_PROTOCOL_PREFIX)
                .ok_or("Unsupported WebSocket subprotocol")?;
            if ticket.is_some() || !crate::auth::ws_tickets::valid_shape(candidate) {
                return Err("Invalid WebSocket authentication ticket");
            }
            ticket = Some(candidate.to_string());
        }
    }
    Ok(ticket)
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

/// The application's own content security policy. The reverse proxy carries
/// the same text as a default for responses it answers itself (`Caddyfile`),
/// so a handler's tighter policy is what the browser receives.
pub(crate) const CONTENT_SECURITY_POLICY: &str = "default-src 'self'; base-uri 'none'; object-src 'none'; frame-ancestors 'none'; form-action 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; media-src 'self' blob:; connect-src 'self'; font-src 'self'";

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
    // Download handlers set a tighter sandbox policy for untrusted file bytes.
    // The application policy supplies the default without replacing that boundary.
    headers
        .entry(header::CONTENT_SECURITY_POLICY)
        .or_insert_with(|| HeaderValue::from_static(CONTENT_SECURITY_POLICY));
    headers.insert(
        header::HeaderName::from_static("permissions-policy"),
        HeaderValue::from_static(
            "camera=(self), microphone=(self), display-capture=(self), geolocation=()",
        ),
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
    // Every JSON body, errors included, is account or policy state for one
    // caller; nothing in it may be served from a cache.
    if response
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

/// Sizes the account Argon2 lane.
///
/// Each verification is a 19 MiB, multi-pass argon2id job that runs on the
/// blocking pool and is reachable without an account (unknown emails verify
/// a dummy hash). The lane shares its CPU quota with the in-process media
/// workers, so its default follows available parallelism rather than the HTTP
/// concurrency cap; `MAX_PASSWORD_WORKERS` (1–32) overrides it.
fn password_lane_capacity(
    override_value: Option<usize>,
    auth_concurrency: usize,
    available_parallelism: usize,
) -> usize {
    match override_value {
        Some(value) if (1..=32).contains(&value) => value,
        _ => auth_concurrency.min(available_parallelism / 2).max(1),
    }
}

/// Applies per-socket options to every accepted signaling connection.
///
/// Signaling frames are small and often sent back to back (a join reply
/// followed by producer announcements), so Nagle's algorithm would hold each
/// one until the previous segment is acknowledged. A failed option set is
/// logged and the connection proceeds with kernel defaults.
fn tuned_listener(
    listener: tokio::net::TcpListener,
) -> axum::serve::TapIo<
    tokio::net::TcpListener,
    impl FnMut(&mut tokio::net::TcpStream) + Send + 'static,
> {
    use axum::serve::ListenerExt;
    listener.tap_io(|stream| {
        if let Err(error) = stream.set_nodelay(true) {
            tracing::debug!(%error, "TCP_NODELAY was not applied to a signaling socket");
        }
    })
}
