//! HTTP, WebSocket and proxy-header security regressions against a bound server.

use super::*;

#[test]
fn public_capabilities_distinguish_guest_invited_and_open_enrollment() {
    let guest = capabilities(false, false, false, false, true);
    assert_eq!(guest["version"], 1);
    assert_eq!(guest["passwordRegistration"], "disabled");
    assert_eq!(guest["adHocRooms"], true);
    let invited = capabilities(true, true, false, true, false);
    assert_eq!(invited["passwordRegistration"], "invite");
    assert_eq!(invited["passkeyRegistration"], "disabled");
    assert_eq!(invited["passkeyLogin"], true);
    let open = capabilities(true, true, true, true, false);
    assert_eq!(open["passkeyRegistration"], "open");
    assert_eq!(open["roomCreation"], true);
}

#[test]
fn ip_capacity_evicts_cold_buckets_without_shared_overflow_denial() {
    let guard = AuthGuard::new(
        1,
        1,
        Arc::new(None),
        Arc::new(Vec::new()),
        ServerMetrics::new(),
    );
    let started = Instant::now();
    for index in 0..MAX_TRACKED_AUTH_IPS as u32 {
        assert!(guard.allow_at(IpAddr::V4(std::net::Ipv4Addr::from(index)), started));
    }
    let victim = IpAddr::V4(std::net::Ipv4Addr::from(0_u32));
    assert!(!guard.allow_at(victim, started));
    // These are local keys, not network requests. Capacity is tested without
    // interacting with production services or exhausting live resources.
    assert!(guard.allow_at(IpAddr::from([192, 0, 2, 1]), started));
    for last in 2..=32 {
        assert!(guard.allow_at(IpAddr::from([192, 0, 2, last]), started));
    }
    assert!(!guard.allow_at(victim, started));
    let table = guard.entries.lock().unwrap();
    assert_eq!(table.entries.len(), MAX_TRACKED_AUTH_IPS);
    drop(table);
    assert!(guard.allow_at(victim, started + PRINCIPAL_RATE_WINDOW));
    assert_eq!(
        guard.entries.lock().unwrap().entries.len(),
        MAX_TRACKED_AUTH_IPS
    );
}

#[tokio::test]
async fn profile_reads_nest_beside_the_auth_router_on_their_own_guard() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let metrics = ServerMetrics::new();
    let auth = AuthGuard::new(1, 4, Arc::new(None), Arc::new(Vec::new()), metrics.clone());
    let profiles = AuthGuard::new(10, 4, Arc::new(None), Arc::new(Vec::new()), metrics);
    let router = Router::new()
        .nest(
            "/api/auth/profiles",
            Router::new()
                .route(
                    "/{id}",
                    get(
                        |axum::extract::Path(id): axum::extract::Path<String>| async move {
                            format!("profile {id}")
                        },
                    ),
                )
                .layer(middleware::from_fn_with_state(
                    profiles,
                    guarded_api_request,
                )),
        )
        .nest(
            "/api/auth",
            Router::new()
                .route("/refresh", axum::routing::post(|| async { "refreshed" }))
                .layer(middleware::from_fn_with_state(auth, guarded_api_request)),
        );
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let request = |line: &'static str| async move {
        let mut connection = tokio::net::TcpStream::connect(address).await.unwrap();
        connection
            .write_all(
                format!("{line} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Length: 0\r\n\r\n")
                    .as_bytes(),
            )
            .await
            .unwrap();
        let mut response = String::new();
        connection.read_to_string(&mut response).await.unwrap();
        response
    };
    let result = tokio::time::timeout(Duration::from_secs(5), async {
        // Profile reads keep answering after the auth budget (one request) is spent.
        assert!(
            request("POST /api/auth/refresh")
                .await
                .contains("refreshed")
        );
        assert!(
            request("POST /api/auth/refresh")
                .await
                .starts_with("HTTP/1.1 429 ")
        );
        for _ in 0..3 {
            let response = request("GET /api/auth/profiles/abc").await;
            assert!(response.starts_with("HTTP/1.1 200 "), "{response}");
            assert!(response.contains("profile abc"));
        }
    })
    .await;
    task.abort();
    let _ = task.await;
    result.unwrap();
}

#[tokio::test]
async fn http_metrics_include_outer_timeouts_and_guard_rejections() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let metrics = ServerMetrics::new();
    let guard = AuthGuard::new(1, 1, Arc::new(None), Arc::new(Vec::new()), metrics.clone());
    let router = Router::new()
        .route(
            "/api/auth/login",
            axum::routing::post(|| async {
                tokio::time::sleep(Duration::from_millis(50)).await;
                StatusCode::OK
            }),
        )
        .layer(middleware::from_fn_with_state(guard, guarded_api_request))
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            Duration::from_millis(5),
        ))
        .layer(middleware::from_fn_with_state(
            metrics.clone(),
            observe_http_request,
        ));
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let result = tokio::time::timeout(Duration::from_secs(2), async {
        for status in [408, 429] {
            let mut connection = tokio::net::TcpStream::connect(address).await.unwrap();
            connection.write_all(b"POST /api/auth/login HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Length: 0\r\n\r\n").await.unwrap();
            let mut response = String::new();
            connection.read_to_string(&mut response).await.unwrap();
            assert!(response.starts_with(&format!("HTTP/1.1 {status} ")), "{response}");
            assert!(response.contains("x-request-id:"));
        }
    }).await;
    task.abort();
    let _ = task.await;
    result.unwrap();
    let snapshot = metrics.render_prometheus(0, 0, 0);
    for status in [408, 429] {
        assert!(snapshot.contains(&format!("simplestchat_http_requests_total{{route=\"/api/auth/login\",method=\"POST\",status=\"{status}\"}} 1")), "{snapshot}");
    }
}

#[test]
fn password_lane_is_sized_from_cpu_quota_not_http_concurrency() {
    assert_eq!(password_lane_capacity(None, 16, 2), 1);
    assert_eq!(password_lane_capacity(None, 16, 18), 9);
    assert_eq!(password_lane_capacity(None, 4, 18), 4);
    assert_eq!(password_lane_capacity(Some(6), 16, 2), 6);
    assert_eq!(password_lane_capacity(Some(0), 16, 2), 1);
    assert_eq!(password_lane_capacity(Some(64), 16, 2), 1);
}

#[tokio::test]
async fn accepted_signaling_sockets_disable_nagle() {
    use axum::serve::Listener;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let mut listener = tuned_listener(listener);
    let client = tokio::net::TcpStream::connect(address).await.unwrap();
    let (accepted, _) = Listener::accept(&mut listener).await;
    assert!(accepted.nodelay().unwrap());
    drop(client);
}

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

    let guard = AuthGuard::new(
        10,
        1,
        Arc::new(None),
        Arc::new(Vec::new()),
        ServerMetrics::new(),
    );
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
    assert!(response.contains(&format!(
        "content-security-policy: {CONTENT_SECURITY_POLICY}\r\n"
    )));
}

#[test]
fn the_proxy_defaults_to_the_applications_content_security_policy() {
    // The Caddyfile may only supply the policy where the backend sent none
    // (the `?` prefix); otherwise a handler's sandbox never reaches a browser.
    let caddyfile = include_str!("../../Caddyfile");
    let line = caddyfile
        .lines()
        .map(str::trim)
        .find(|line| line.contains("Content-Security-Policy"))
        .expect("the proxy carries a content security policy");
    let value = line
        .strip_prefix("?Content-Security-Policy \"")
        .and_then(|rest| rest.strip_suffix('"'))
        .expect("the proxy policy is a default, not a replacement");
    assert_eq!(value, CONTENT_SECURITY_POLICY);
    assert_eq!(
        caddyfile.matches("Content-Security-Policy").count(),
        1,
        "one proxy policy line"
    );
}

#[tokio::test]
async fn handler_download_sandbox_survives_global_http_security_headers() {
    use tower::ServiceExt;
    let router = with_static_fallback_and_security(Router::new().route(
        "/owned-download-fixture",
        get(|| async {
            (
                [(
                    header::CONTENT_SECURITY_POLICY,
                    "default-src 'none'; sandbox",
                )],
                "owned fixture",
            )
        }),
    ));
    let response = router
        .oneshot(
            Request::builder()
                .uri("/owned-download-fixture")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[header::CONTENT_SECURITY_POLICY],
        "default-src 'none'; sandbox"
    );
    assert_eq!(
        response.headers()[header::X_CONTENT_TYPE_OPTIONS],
        "nosniff"
    );
    assert_eq!(response.headers()[header::REFERRER_POLICY], "no-referrer");
    let body = axum::body::to_bytes(response.into_body(), 1024)
        .await
        .unwrap();
    assert_eq!(body, "owned fixture");
}

#[test]
fn auth_rate_guard_enforces_limit_and_recovers_next_window() {
    let guard = AuthGuard::new(
        2,
        1,
        Arc::new(None),
        Arc::new(Vec::new()),
        ServerMetrics::new(),
    );
    let ip = IpAddr::from([192, 0, 2, 1]);
    assert!(guard.allow(ip));
    assert!(guard.allow(ip));
    assert!(!guard.allow(ip));
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL pointing to a migrated disposable PostgreSQL database"]
async fn database_membership_handler_always_returns_a_private_cursor_page() {
    use crate::room::invites::{MembershipParams, list_memberships};
    let pool = PgPool::connect(&std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL"))
        .await
        .unwrap();
    let user = uuid::Uuid::new_v4();
    sqlx::query("INSERT INTO users(id,email,display_name) VALUES($1,$2,'Page test')")
        .bind(user)
        .bind(format!("{user}@membership-handler.invalid"))
        .execute(&pool)
        .await
        .unwrap();
    let mut media = crate::media::config::MediaConfig::default();
    media.worker_config.num_workers = 1;
    media.webrtc_server_port_base = crate::media::worker_manager::reserve_worker_ports(1);
    let metrics = ServerMetrics::new();
    let manager = Arc::new(
        RoomManager::new(
            crate::room::ManagerSettings::default(),
            media,
            metrics.clone(),
            None,
        )
        .await
        .unwrap(),
    );
    let mut server = SignalingServer::new(
        manager,
        metrics,
        Some(pool.clone()),
        &crate::config::Config::for_tests(),
    )
    .unwrap();
    let secret = "disposable-membership-handler-secret-at-least-32-bytes";
    server.jwt_secret = Some(secret.into());
    let session_id = crate::auth::session::create_session(
        &pool,
        &user,
        &crate::auth::session::generate_refresh_token().unwrap(),
    )
    .await
    .unwrap();
    let token = crate::auth::jwt::create_session_token(
        &user.to_string(),
        "Page test",
        secret,
        0,
        session_id,
    )
    .unwrap();
    let mut headers = HeaderMap::new();
    headers.insert(
        header::AUTHORIZATION,
        format!("Bearer {token}").parse().unwrap(),
    );
    for after in [None, Some("room-100".to_string())] {
        let (response_headers, axum::Json(page)) = list_memberships(
            State(server.clone()),
            headers.clone(),
            Query(MembershipParams { after }),
        )
        .await
        .unwrap();
        assert_eq!(response_headers[header::CACHE_CONTROL], "private, no-store");
        assert_eq!(
            serde_json::to_value(page).unwrap(),
            serde_json::json!({"items": [], "next_cursor": null})
        );
    }
    assert!(
        list_memberships(
            State(server),
            HeaderMap::new(),
            Query(MembershipParams::default()),
        )
        .await
        .is_err()
    );
    sqlx::query("DELETE FROM users WHERE id=$1")
        .bind(user)
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL pointing to a migrated disposable PostgreSQL database"]
async fn database_password_and_passkey_signup_share_taken_email_budget() {
    use crate::auth::{
        routes,
        types::{AuthError, RegisterRequest},
    };
    let pool = PgPool::connect(&std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL"))
        .await
        .unwrap();
    let id = uuid::Uuid::new_v4();
    let email = format!("{id}@signup.invalid");
    sqlx::query("INSERT INTO users(id,email,display_name) VALUES($1,$2,'Existing')")
        .bind(id)
        .bind(&email)
        .execute(&pool)
        .await
        .unwrap();
    let mut media = crate::media::config::MediaConfig::default();
    media.worker_config.num_workers = 1;
    media.webrtc_server_port_base = crate::media::worker_manager::reserve_worker_ports(1);
    let metrics = ServerMetrics::new();
    let manager = Arc::new(
        RoomManager::new(
            crate::room::ManagerSettings::default(),
            media,
            metrics.clone(),
            None,
        )
        .await
        .unwrap(),
    );
    let mut server = SignalingServer::new(
        manager,
        metrics,
        Some(pool.clone()),
        &crate::config::Config::for_tests(),
    )
    .unwrap();
    server.registration_enabled = true;
    server.jwt_secret = Some(format!("{}{}", uuid::Uuid::new_v4(), uuid::Uuid::new_v4()));
    server.webauthn = Some(Arc::new(
        webauthn_rs::prelude::WebauthnBuilder::new(
            "localhost",
            &url::Url::parse("https://localhost").unwrap(),
        )
        .unwrap()
        .build()
        .unwrap(),
    ));
    server.challenge_store = Some(Arc::new(ChallengeStore::new()));
    server.registration_limiter = Arc::new(crate::auth::limiter::WindowLimiter::new(
        1,
        Duration::from_secs(3600),
    ));
    let password = |ip| {
        routes::register(
            State(server.clone()),
            axum::Extension(ClientIp(ip)),
            Json(RegisterRequest {
                email: email.clone(),
                password: "unique-river-passphrase".into(),
                display_name: "Existing".into(),
                invite_code: None,
            }),
        )
    };
    let passkey = |ip| {
        routes::passkey_register_start(
            State(server.clone()),
            axum::Extension(ClientIp(ip)),
            Json(
                serde_json::from_value::<routes::PasskeyRegisterStartRequest>(serde_json::json!({
                    "email": email, "display_name": "Existing"
                }))
                .unwrap(),
            ),
        )
    };
    let first = IpAddr::from([192, 0, 2, 1]);
    let second = IpAddr::from([192, 0, 2, 2]);
    assert!(matches!(
        password(first).await,
        Err(AuthError::EmailAlreadyExists)
    ));
    assert!(matches!(passkey(first).await, Err(AuthError::RateLimited)));
    assert!(matches!(
        passkey(second).await,
        Err(AuthError::EmailAlreadyExists)
    ));
    assert!(matches!(
        password(second).await,
        Err(AuthError::RateLimited)
    ));
    sqlx::query("DELETE FROM users WHERE id=$1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL pointing to a migrated disposable PostgreSQL database"]
async fn database_ticket_http_and_upgrade_enforce_single_use_and_logout() {
    use crate::auth::{jwt, session};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let pool = PgPool::connect(&std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL"))
        .await
        .unwrap();
    let user = uuid::Uuid::new_v4();
    sqlx::query("INSERT INTO users(id,email,display_name) VALUES($1,$2,'Upgrade test')")
        .bind(user)
        .bind(format!("{user}@upgrade.invalid"))
        .execute(&pool)
        .await
        .unwrap();
    let refresh = session::generate_refresh_token().unwrap();
    let mut transaction = pool.begin().await.unwrap();
    let sid = session::create_session_with(&mut transaction, &user, &refresh)
        .await
        .unwrap();
    transaction.commit().await.unwrap();
    let secret = "disposable-upgrade-test-secret-at-least-32-bytes";
    let token =
        jwt::create_session_token(&user.to_string(), "Upgrade test", secret, 0, sid).unwrap();
    let mut media = crate::media::config::MediaConfig::default();
    media.worker_config.num_workers = 1;
    media.webrtc_server_port_base = crate::media::worker_manager::reserve_worker_ports(1);
    let metrics = ServerMetrics::new();
    let manager = Arc::new(
        RoomManager::new(
            crate::room::ManagerSettings::default(),
            media,
            metrics.clone(),
            None,
        )
        .await
        .unwrap(),
    );
    let mut server = SignalingServer::new(
        manager,
        metrics,
        Some(pool.clone()),
        &crate::config::Config::for_tests(),
    )
    .unwrap();
    server.jwt_secret = Some(secret.into());
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        axum::serve(
            listener,
            server
                .router()
                .into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });
    let mint = |bearer: Option<String>| async move {
        let mut connection = tokio::net::TcpStream::connect(address).await.unwrap();
        let authorization = bearer.map_or(String::new(), |value| {
            format!("Authorization: Bearer {value}\r\n")
        });
        connection.write_all(format!("POST /api/auth/ws-ticket HTTP/1.1\r\nHost: {address}\r\n{authorization}Content-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}").as_bytes()).await.unwrap();
        let mut response = String::new();
        connection.read_to_string(&mut response).await.unwrap();
        response
    };
    let upgrade = |protocol: String| async move {
        let mut connection = tokio::net::TcpStream::connect(address).await.unwrap();
        connection.write_all(format!("GET /ws HTTP/1.1\r\nHost: {address}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Protocol: {protocol}\r\n\r\n").as_bytes()).await.unwrap();
        let mut response = Vec::new();
        while response.len() < 16_384 && !response.ends_with(b"\r\n\r\n") {
            response.push(connection.read_u8().await.unwrap());
        }
        String::from_utf8(response).unwrap()
    };
    let result = tokio::time::timeout(Duration::from_secs(10), async {
        assert!(mint(None).await.starts_with("HTTP/1.1 401 "));
        let response = mint(Some(token.clone())).await;
        assert!(response.starts_with("HTTP/1.1 200 "));
        assert!(
            response
                .to_ascii_lowercase()
                .contains("cache-control: no-store")
        );
        let data: serde_json::Value =
            serde_json::from_str(response.split_once("\r\n\r\n").unwrap().1).unwrap();
        let ticket = data["ticket"].as_str().unwrap();
        assert!(crate::auth::ws_tickets::valid_shape(ticket));
        let response = upgrade(format!("simplestchat, ticket.{ticket}")).await;
        assert!(response.starts_with("HTTP/1.1 101 "));
        assert!(
            response
                .to_ascii_lowercase()
                .contains("sec-websocket-protocol: simplestchat\r\n")
        );
        assert!(!response.contains(ticket));
        assert!(
            upgrade(format!("simplestchat, ticket.{ticket}"))
                .await
                .starts_with("HTTP/1.1 401 ")
        );
        assert!(
            upgrade(format!("simplestchat, auth.{token}"))
                .await
                .starts_with("HTTP/1.1 400 ")
        );
        let response = mint(Some(token)).await;
        let data: serde_json::Value =
            serde_json::from_str(response.split_once("\r\n\r\n").unwrap().1).unwrap();
        session::delete_session_by_token(&pool, &refresh.raw)
            .await
            .unwrap();
        assert!(
            upgrade(format!(
                "simplestchat, ticket.{}",
                data["ticket"].as_str().unwrap()
            ))
            .await
            .starts_with("HTTP/1.1 401 ")
        );
        assert!(
            upgrade("simplestchat".into())
                .await
                .starts_with("HTTP/1.1 101 "),
            "guest connections remain available"
        );
    })
    .await;
    task.abort();
    let _ = task.await;
    sqlx::query("DELETE FROM users WHERE id=$1")
        .bind(user)
        .execute(&pool)
        .await
        .unwrap();
    result.unwrap();
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
fn websocket_ticket_protocol_rejects_bearers_duplicates_and_malformed_values() {
    let mut headers = HeaderMap::new();
    assert!(websocket_auth_ticket(&headers).unwrap().is_none());
    let ticket = "A".repeat(43);
    headers.insert(
        header::SEC_WEBSOCKET_PROTOCOL,
        HeaderValue::from_str(&format!("simplestchat, ticket.{ticket}")).unwrap(),
    );
    assert_eq!(
        websocket_auth_ticket(&headers).unwrap(),
        Some(ticket.clone())
    );
    for invalid in [
        "auth.header.payload.signature".to_owned(),
        "unexpected".into(),
        "ticket.short".into(),
        format!("ticket.{ticket}, ticket.{ticket}"),
    ] {
        headers.insert(
            header::SEC_WEBSOCKET_PROTOCOL,
            HeaderValue::from_str(&invalid).unwrap(),
        );
        assert!(websocket_auth_ticket(&headers).is_err());
    }
    assert!(Query::<WsParams>::try_from_uri(&"/ws".parse().unwrap()).is_ok());
    assert!(Query::<WsParams>::try_from_uri(&"/ws?token=secret".parse().unwrap()).is_err());
    assert!(Query::<WsParams>::try_from_uri(&"/ws?ticket=secret".parse().unwrap()).is_err());
}
