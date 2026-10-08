//! Session-list ownership and revocation through the real HTTP router.
use super::*;

async fn session_request(fixture: &Fixture, method: &str, path: &str, token: &str) -> Response {
    fixture
        .request(
            &HttpOperation {
                method: method.to_owned(),
                path: path.to_owned(),
                handler: String::new(),
                policy: "current-session".to_owned(),
                body: None,
                unauthenticated_status: None,
                checks: Vec::new(),
            },
            Some(token),
            218,
        )
        .await
}

#[tokio::test]
#[ignore = "requires DISPOSABLE_TEST_DATABASE=1 and TEST_DATABASE_URL"]
async fn authorization_database_session_controls_scope_reads_and_revocation_to_the_owner() {
    let fixture = Fixture::new(2).await;
    let second = session::create_session(
        &fixture.pool,
        &fixture.users[0],
        &session::generate_refresh_token().unwrap(),
    )
    .await
    .unwrap();
    let second_token = fixture.token(fixture.users[0], second, 0);
    let response = session_request(&fixture, "GET", "/api/auth/sessions", &fixture.tokens[0]).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response.headers()[header::CACHE_CONTROL]
            .to_str()
            .unwrap()
            .contains("no-store")
    );
    let entries: Value = serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), 8192)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(entries.as_array().unwrap().len(), 2);
    assert_eq!(entries[0]["id"], fixture.sessions[0].to_string());
    assert_eq!(entries[0]["current"], true);
    for row in entries.as_array().unwrap() {
        assert_eq!(row.as_object().unwrap().len(), 5);
        assert_ne!(row["id"], fixture.sessions[1].to_string());
    }
    let foreign = format!("/api/auth/sessions/{}", fixture.sessions[1]);
    assert_eq!(
        session_request(&fixture, "DELETE", &foreign, &fixture.tokens[0])
            .await
            .status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        session_request(&fixture, "GET", "/api/auth/profile", &fixture.tokens[1])
            .await
            .status(),
        StatusCode::OK
    );
    let bulk = session_request(
        &fixture,
        "DELETE",
        "/api/auth/sessions/others",
        &fixture.tokens[0],
    )
    .await;
    assert_eq!(bulk.status(), StatusCode::NO_CONTENT);
    assert!(!bulk.headers().contains_key(header::SET_COOKIE));
    assert_eq!(
        session_request(&fixture, "GET", "/api/auth/profile", &second_token)
            .await
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        session_request(&fixture, "GET", "/api/auth/profile", &fixture.tokens[0])
            .await
            .status(),
        StatusCode::OK
    );
    let own = format!("/api/auth/sessions/{}", fixture.sessions[0]);
    let revoke = session_request(&fixture, "DELETE", &own, &fixture.tokens[0]).await;
    assert_eq!(revoke.status(), StatusCode::NO_CONTENT);
    assert!(
        !revoke.headers().contains_key(header::SET_COOKIE),
        "a bearer-only request must not clear an unrelated browser cookie"
    );
    assert_eq!(
        session_request(&fixture, "GET", "/api/auth/sessions", &fixture.tokens[0])
            .await
            .status(),
        StatusCode::UNAUTHORIZED
    );
    fixture.finish().await;
}
