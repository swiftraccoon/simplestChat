//! Actual Axum requests, owned disposable accounts, and bounded production-style
//! tracing. Intended credential-bearing response bodies/cookies are not leaks.

use super::*;
use crate::security_canary_tests::{Capture, SensitiveValues};
use axum::http::HeaderMap;
use tracing::instrument::WithSubscriber;

struct Exchange {
    status: StatusCode,
    headers: HeaderMap,
    body: String,
}

impl Exchange {
    fn json(&self) -> Value {
        serde_json::from_str(&self.body).expect("JSON response")
    }

    fn credentials(&self, secrets: &mut SensitiveValues) -> (String, String) {
        assert_eq!(self.status, StatusCode::OK);
        let token = self.json()["token"].as_str().unwrap().to_owned();
        let cookie = self.headers[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        secrets.add("issued access token", &token);
        secrets.add("issued refresh token", cookie.split_once('=').unwrap().1);
        (token, cookie)
    }
}

async fn exchange(
    fixture: &Fixture,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Option<Value>,
    urls: &mut String,
) -> Exchange {
    let mut request = Request::builder().method(method).uri(path);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let body = if let Some(body) = body {
        request = request.header(header::CONTENT_TYPE, "application/json");
        Body::from(body.to_string())
    } else {
        Body::empty()
    };
    let mut request = request.body(body).unwrap();
    request
        .extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([192, 0, 2, 213], 12345))));
    urls.push_str(&request.uri().to_string());
    urls.push('\n');
    let response = fixture
        .server
        .clone()
        .router()
        .oneshot(request)
        .await
        .unwrap();
    let (parts, body) = response.into_parts();
    for name in [header::LOCATION, header::CONTENT_LOCATION, header::LINK] {
        for value in parts.headers.get_all(name) {
            urls.push_str(value.to_str().unwrap());
            urls.push('\n');
        }
    }
    Exchange {
        status: parts.status,
        headers: parts.headers,
        body: String::from_utf8(
            axum::body::to_bytes(body, 512 * 1024)
                .await
                .unwrap()
                .to_vec(),
        )
        .unwrap(),
    }
}

#[tokio::test]
#[ignore = "requires DISPOSABLE_TEST_DATABASE=1 and TEST_DATABASE_URL"]
async fn runtime_canary_auth_requests_keep_secrets_out_of_logs_metrics_diagnostics_and_urls() {
    let mut fixture = Fixture::new(0).await;
    fixture.server.registration_enabled = true;
    fixture.server.media_diagnostics = media_diagnostics::MediaDiagnostics::configure(
        true,
        fixture.server.metrics_token.as_deref(),
    )
    .unwrap();
    let capture = Capture::default();
    let mut secrets = SensitiveValues::default();
    let mut urls = String::new();
    let nonce = Uuid::new_v4().simple().to_string();
    let email = format!("canary-{nonce}@example.invalid");
    let password = format!("PasswordCanary-{nonce}");
    let wrong_password = format!("DeniedPasswordCanary-{nonce}");
    let name = format!("NameCanary-{nonce}");
    let malformed = format!("MalformedCredential-{nonce}");
    let absent_invite = crate::invite_codes::generate().unwrap();
    for (label, value) in [
        ("email", email.as_str()),
        ("password", password.as_str()),
        ("denied password", wrong_password.as_str()),
        ("display name", name.as_str()),
        ("malformed credential", malformed.as_str()),
        ("absent invite", absent_invite.as_str()),
        (
            "JWT signing key",
            fixture.server.jwt_secret.as_deref().unwrap(),
        ),
        (
            "metrics credential",
            fixture.server.metrics_token.as_deref().unwrap(),
        ),
    ] {
        secrets.add(label, value);
    }
    let (metrics, diagnostics) = async {
        let registered = exchange(
            &fixture,
            "POST",
            "/api/auth/register",
            &[],
            Some(json!({"email":email,"password":password,"display_name":name})),
            &mut urls,
        )
        .await;
        let (_, _) = registered.credentials(&mut secrets);
        let user_id = Uuid::parse_str(registered.json()["user"]["id"].as_str().unwrap()).unwrap();
        fixture.users.push(user_id);

        let denied = exchange(
            &fixture,
            "POST",
            "/api/auth/login",
            &[],
            Some(json!({"email":email,"password":wrong_password})),
            &mut urls,
        )
        .await;
        assert_eq!(denied.status, StatusCode::UNAUTHORIZED);
        secrets.assert_absent("denied login response", &denied.body);
        let login = exchange(
            &fixture,
            "POST",
            "/api/auth/login",
            &[],
            Some(json!({"email":email,"password":password})),
            &mut urls,
        )
        .await;
        let (token, cookie) = login.credentials(&mut secrets);
        let bearer = format!("Bearer {token}");
        let profile = exchange(
            &fixture,
            "GET",
            "/api/auth/profile",
            &[("authorization", &bearer)],
            None,
            &mut urls,
        )
        .await;
        assert_eq!(profile.status, StatusCode::OK);
        assert!(
            profile.body.contains(&name),
            "real profile must contain its owner identity"
        );
        let ticket = exchange(
            &fixture,
            "POST",
            "/api/auth/ws-ticket",
            &[("authorization", &bearer)],
            Some(json!({})),
            &mut urls,
        )
        .await;
        assert_eq!(ticket.status, StatusCode::OK);
        secrets.add(
            "issued WebSocket ticket",
            ticket.json()["ticket"].as_str().unwrap(),
        );

        let refreshed = exchange(
            &fixture,
            "POST",
            "/api/auth/refresh",
            &[("cookie", &cookie)],
            None,
            &mut urls,
        )
        .await;
        let (fresh_token, fresh_cookie) = refreshed.credentials(&mut secrets);
        assert!(
            cookie != fresh_cookie,
            "refresh must issue a new credential"
        );
        let fresh_bearer = format!("Bearer {fresh_token}");
        let invite = exchange(
            &fixture,
            "POST",
            "/api/rooms/invites/redeem",
            &[("authorization", &fresh_bearer)],
            Some(json!({"code":absent_invite})),
            &mut urls,
        )
        .await;
        assert_eq!(invite.status, StatusCode::NOT_FOUND);
        secrets.assert_absent("denied invitation response", &invite.body);

        for (path, name, value) in [
            (
                "/api/auth/profile",
                "authorization",
                format!("Bearer {malformed}"),
            ),
            (
                "/api/auth/refresh",
                "cookie",
                format!("__Host-refresh_token={malformed}"),
            ),
        ] {
            let denied = exchange(
                &fixture,
                if name == "cookie" { "POST" } else { "GET" },
                path,
                &[(name, &value)],
                None,
                &mut urls,
            )
            .await;
            assert_eq!(denied.status, StatusCode::UNAUTHORIZED);
            secrets.assert_absent("denied credential response", &denied.body);
        }
        let logout = exchange(
            &fixture,
            "POST",
            "/api/auth/logout",
            &[("cookie", &fresh_cookie)],
            None,
            &mut urls,
        )
        .await;
        assert_eq!(logout.status, StatusCode::NO_CONTENT);
        let revoked = exchange(
            &fixture,
            "GET",
            "/api/auth/profile",
            &[("authorization", &fresh_bearer)],
            None,
            &mut urls,
        )
        .await;
        assert_eq!(revoked.status, StatusCode::UNAUTHORIZED);
        secrets.assert_absent("revoked-session response", &revoked.body);

        let metrics_bearer = format!(
            "Bearer {}",
            fixture.server.metrics_token.as_deref().unwrap()
        );
        let metrics = exchange(
            &fixture,
            "GET",
            "/metrics",
            &[("authorization", &metrics_bearer)],
            None,
            &mut urls,
        )
        .await;
        assert_eq!(metrics.status, StatusCode::OK);
        assert!(metrics.body.contains("simplestchat_db_pool_connections"));
        assert!(metrics.body.contains("/api/auth/login"));
        let diagnostics = exchange(
            &fixture,
            "GET",
            "/diagnostics/media",
            &[("authorization", &metrics_bearer)],
            None,
            &mut urls,
        )
        .await;
        assert_eq!(diagnostics.status, StatusCode::OK);
        assert_eq!(diagnostics.json()["schemaVersion"], 2);
        assert_eq!(diagnostics.json()["sampleId"], 1);
        assert_eq!(diagnostics.json()["coverage"]["complete"], true);
        fixture.finish().await;
        (metrics.body, diagnostics.body)
    }
    .with_subscriber(capture.subscriber())
    .await;
    let records = capture.records();
    for expected in ["User registered", "User logged in", "Failed login attempt"] {
        assert!(
            records
                .iter()
                .any(|record| record["fields"]["message"] == expected),
            "actual request event missing: {expected}"
        );
    }
    secrets.assert_absent("application tracing", &capture.text());
    secrets.assert_absent("HTTP metrics", &metrics);
    secrets.assert_absent("media diagnostic response", &diagnostics);
    secrets.assert_absent("request URIs and URL response headers", &urls);
}
