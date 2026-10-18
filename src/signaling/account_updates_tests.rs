//! Hosted-only HTTP contracts: no room join or media capture is required.
use super::*;
use futures_util::StreamExt;

struct Feed(axum::body::BodyDataStream);

impl Feed {
    async fn open(fixture: &Fixture, account: usize, address: u16) -> Self {
        let response = fixture
            .request(
                &operation("GET", "/api/auth/updates".into(), None),
                Some(&fixture.tokens[account]),
                address,
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "text/event-stream"
        );
        assert_eq!(
            response.headers()[header::CACHE_CONTROL],
            "no-store, max-age=0"
        );
        assert_eq!(response.headers()["x-accel-buffering"], "no");
        let mut feed = Self(response.into_body().into_data_stream());
        assert_eq!(feed.event().await, json!({"type":"ready"}));
        feed
    }

    async fn event(&mut self) -> Value {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let bytes = self.0.next().await.expect("stream remains open").unwrap();
                let frame = std::str::from_utf8(&bytes).unwrap();
                if frame.starts_with(':') {
                    continue;
                }
                let data = frame
                    .lines()
                    .find_map(|line| line.strip_prefix("data: "))
                    .expect("SSE data frame");
                let value: Value = serde_json::from_str(data).unwrap();
                assert!(frame.contains(&format!("event: {}\n", value["type"].as_str().unwrap())));
                return value;
            }
        })
        .await
        .expect("bounded account update")
    }

    async fn change(&mut self, scope: &str, peer: Option<Uuid>) {
        let mut expected = json!({"type":"change","scope":scope});
        if let Some(peer) = peer {
            expected["peerId"] = json!(peer);
        }
        assert_eq!(self.event().await, expected);
    }

    async fn quiet(&mut self) {
        assert!(
            tokio::time::timeout(Duration::from_millis(40), self.0.next())
                .await
                .is_err(),
            "unrelated accounts receive no private invalidation"
        );
    }

    async fn closed(&mut self) {
        assert!(
            tokio::time::timeout(Duration::from_secs(11), self.0.next())
                .await
                .expect("authorization/drain closes stream within its bounded interval")
                .is_none()
        );
    }
}

async fn data(fixture: &Fixture, actor: usize, method: &str, path: String, body: Value) -> Value {
    let response = fixture
        .request(
            &operation(method, path, Some(body)),
            Some(&fixture.tokens[actor]),
            42,
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), 256 * 1024)
            .await
            .unwrap(),
    )
    .unwrap()
}

#[tokio::test]
#[ignore = "requires DISPOSABLE_TEST_DATABASE=1 and TEST_DATABASE_URL"]
async fn authorization_database_notification_patch_merges_current_fields_and_preserves_quiet_hours()
{
    let fixture = Fixture::new(2).await;
    let mut feed = Feed::open(&fixture, 0, 30).await;
    let mut outsider = Feed::open(&fixture, 1, 30).await;
    let second_session = session::create_session(
        &fixture.pool,
        &fixture.users[0],
        &session::generate_refresh_token().unwrap(),
    )
    .await
    .unwrap();
    let second_token = fixture.token(fixture.users[0], second_session, 0);
    let path = "/api/auth/notification-preferences";
    let quiet = json!({"startMinute":1320,"endMinute":420,"timeZone":"America/New_York"});
    // A remote device disables PM alerts while this device still has an older
    // full snapshot. Its later mention intent must not restore remote fields.
    data(
        &fixture,
        0,
        "PUT",
        path.into(),
        json!({"privateMessages":false,"mentions":true,"quietHours":quiet}),
    )
    .await;
    feed.change("notifications", None).await;
    let mention = operation("PATCH", path.into(), Some(json!({"mentions":false})));
    let response = fixture.request(&mention, Some(&second_token), 30).await;
    assert_eq!(response.status(), StatusCode::OK);
    let merged: Value = serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), 32768)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(merged["privateMessages"], false);
    assert_eq!(merged["mentions"], false);
    assert_eq!(merged["quietHours"], quiet);
    feed.change("notifications", None).await;
    let cleared = data(
        &fixture,
        0,
        "PATCH",
        path.into(),
        json!({"quietHours":null}),
    )
    .await;
    assert_eq!(cleared["quietHours"], Value::Null);
    assert_eq!(cleared["privateMessages"], false);
    assert_eq!(cleared["mentions"], false);
    feed.change("notifications", None).await;
    let kept = data(&fixture, 0, "PATCH", path.into(), json!({"mentions":true})).await;
    assert_eq!(kept["quietHours"], Value::Null);
    assert_eq!(kept["privateMessages"], false);
    feed.change("notifications", None).await;
    data(
        &fixture,
        0,
        "PUT",
        path.into(),
        json!({"privateMessages":true,"mentions":true,"quietHours":quiet}),
    )
    .await;
    feed.change("notifications", None).await;
    let pm = operation("PATCH", path.into(), Some(json!({"privateMessages":false})));
    let (left, right) = tokio::join!(
        fixture.request(&pm, Some(&fixture.tokens[0]), 30),
        fixture.request(&mention, Some(&second_token), 30)
    );
    assert_eq!(left.status(), StatusCode::OK);
    assert_eq!(right.status(), StatusCode::OK);
    feed.change("notifications", None).await;
    feed.change("notifications", None).await;
    let response = fixture
        .request(
            &operation("GET", path.into(), None),
            Some(&second_token),
            30,
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let latest: Value = serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), 32768)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(latest["privateMessages"], false);
    assert_eq!(latest["mentions"], false);
    assert_eq!(latest["quietHours"], quiet);
    for (body, status) in [
        (json!({}), StatusCode::BAD_REQUEST),
        (
            json!({"privateMessages":null}),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            json!({"quietHours":{"startMinute":0,"endMinute":0,"timeZone":"UTC"}}),
            StatusCode::BAD_REQUEST,
        ),
    ] {
        let rejected = operation("PATCH", path.into(), Some(body));
        assert_eq!(
            fixture
                .request(&rejected, Some(&second_token), 30)
                .await
                .status(),
            status
        );
    }
    sqlx::query("DELETE FROM sessions WHERE id=$1")
        .bind(second_session)
        .execute(&fixture.pool)
        .await
        .unwrap();
    assert_eq!(
        fixture
            .request(&mention, Some(&second_token), 30)
            .await
            .status(),
        StatusCode::UNAUTHORIZED
    );
    feed.quiet().await;
    outsider.quiet().await;
    drop((feed, outsider));
    fixture.finish().await;
}

#[tokio::test]
#[ignore = "requires DISPOSABLE_TEST_DATABASE=1 and TEST_DATABASE_URL"]
async fn authorization_database_account_updates_publish_committed_scoped_mutations_without_room_join()
 {
    let mut fixture = Fixture::new(3).await;
    let alice = fixture.users[0];
    let bob = fixture.users[1];
    let mut first = Feed::open(&fixture, 0, 1).await;
    let mut second = Feed::open(&fixture, 0, 1).await;
    let mut recipient = Feed::open(&fixture, 1, 1).await;
    let mut stranger = Feed::open(&fixture, 2, 1).await;
    let contact = operation(
        "POST",
        "/api/auth/contacts".into(),
        Some(json!({"accountId":bob})),
    );
    assert_eq!(
        fixture
            .request(&contact, Some(&fixture.tokens[0]), 1)
            .await
            .status(),
        StatusCode::NO_CONTENT
    );
    first.change("contacts", Some(bob)).await;
    second.change("contacts", Some(bob)).await;
    recipient.change("contacts", Some(alice)).await;
    let accept = operation("PUT", format!("/api/auth/contacts/{alice}"), None);
    assert_eq!(
        fixture
            .request(&accept, Some(&fixture.tokens[1]), 1)
            .await
            .status(),
        StatusCode::NO_CONTENT
    );
    first.change("contacts", Some(bob)).await;
    second.change("contacts", Some(bob)).await;
    recipient.change("contacts", Some(alice)).await;
    let sent = data(&fixture, 0, "POST", format!("/api/auth/inbox/{bob}/messages"),
        json!({"clientMessageId":"account-update-send","content":"Retained content never enters the event"})).await;
    for feed in [&mut first, &mut second] {
        feed.change("inbox", Some(bob)).await;
    }
    recipient.change("inbox", Some(alice)).await;
    let id = sent["messageId"].as_str().unwrap();
    data(
        &fixture,
        0,
        "PUT",
        format!("/api/auth/inbox/{bob}/messages/{id}"),
        json!({"content":"Edited durable text","expectedRevision":0}),
    )
    .await;
    for feed in [&mut first, &mut second] {
        feed.change("inbox", Some(bob)).await;
    }
    recipient.change("inbox", Some(alice)).await;
    data(
        &fixture,
        1,
        "PUT",
        format!("/api/auth/inbox/{alice}/messages/{id}/reactions"),
        json!({"emoji":"👍","present":true}),
    )
    .await;
    for feed in [&mut first, &mut second] {
        feed.change("inbox", Some(bob)).await;
    }
    recipient.change("inbox", Some(alice)).await;
    data(
        &fixture,
        1,
        "PUT",
        format!("/api/auth/inbox/{alice}/read"),
        json!({"messageId":id}),
    )
    .await;
    recipient.change("inbox", Some(alice)).await;
    first.quiet().await;
    data(
        &fixture,
        0,
        "PATCH",
        "/api/auth/preferences".into(),
        json!({"allowPrivateMessages":false}),
    )
    .await;
    first.change("preferences", None).await;
    second.change("preferences", None).await;
    data(
        &fixture,
        0,
        "PUT",
        "/api/auth/notification-preferences".into(),
        json!({"privateMessages":false,"mentions":true,"quietHours":null}),
    )
    .await;
    first.change("notifications", None).await;
    second.change("notifications", None).await;
    data(
        &fixture,
        0,
        "PUT",
        format!("/api/auth/notification-preferences/conversations/{bob}"),
        json!({"muted":true,"snoozedUntil":null}),
    )
    .await;
    first.change("notifications", Some(bob)).await;
    second.change("notifications", Some(bob)).await;
    let remove = operation("DELETE", format!("/api/auth/contacts/{bob}"), None);
    assert_eq!(
        fixture
            .request(&remove, Some(&fixture.tokens[0]), 1)
            .await
            .status(),
        StatusCode::NO_CONTENT
    );
    first.change("contacts", Some(bob)).await;
    second.change("contacts", Some(bob)).await;
    recipient.change("contacts", Some(alice)).await;
    let denied = operation(
        "POST",
        format!("/api/auth/inbox/{alice}/messages"),
        Some(json!({"clientMessageId":"denied-account-event","content":"Blocked by privacy"})),
    );
    assert_eq!(
        fixture
            .request(&denied, Some(&fixture.tokens[1]), 1)
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );
    first.quiet().await;
    second.quiet().await;
    recipient.quiet().await;
    stranger.quiet().await;
    drop((first, second, recipient, stranger));
    // Reconnect always starts a new resync; there is no mutation replay.
    let reconnected = Feed::open(&fixture, 0, 1).await;
    drop(reconnected);
    // The JWT deadline, independently of a still-live issuing session, ends a
    // connected stream exactly rather than inheriting a decoder grace period.
    let mut claims =
        jwt::validate_token(&fixture.tokens[2], fixture.server.jwt_secret().unwrap()).unwrap();
    claims.exp = (chrono::Utc::now().timestamp() + 5) as usize;
    fixture.tokens[2] = jsonwebtoken::encode(
        &jsonwebtoken::Header::default(),
        &claims,
        &jsonwebtoken::EncodingKey::from_secret(fixture.server.jwt_secret().unwrap().as_bytes()),
    )
    .unwrap();
    let mut token_expiry = Feed::open(&fixture, 2, 1).await;
    token_expiry.closed().await;
    drop(token_expiry);
    fixture.finish().await;
}

#[tokio::test]
#[ignore = "requires DISPOSABLE_TEST_DATABASE=1 and TEST_DATABASE_URL"]
async fn authorization_database_read_cursors_publish_only_committed_advances() {
    let fixture = Fixture::new(3).await;
    let sender = fixture.users[0];
    let reader = fixture.users[1];
    let contact = operation(
        "POST",
        "/api/auth/contacts".into(),
        Some(json!({"accountId":reader})),
    );
    assert_eq!(
        fixture
            .request(&contact, Some(&fixture.tokens[0]), 31)
            .await
            .status(),
        StatusCode::NO_CONTENT
    );
    let accept = operation("PUT", format!("/api/auth/contacts/{sender}"), None);
    assert_eq!(
        fixture
            .request(&accept, Some(&fixture.tokens[1]), 31)
            .await
            .status(),
        StatusCode::NO_CONTENT
    );
    let first = data(
        &fixture,
        0,
        "POST",
        format!("/api/auth/inbox/{reader}/messages"),
        json!({"clientMessageId":"read-cursor-first","content":"First retained message"}),
    )
    .await;
    let next = data(
        &fixture,
        0,
        "POST",
        format!("/api/auth/inbox/{reader}/messages"),
        json!({"clientMessageId":"read-cursor-next","content":"Next retained message"}),
    )
    .await;
    let first_id = first["messageId"].as_str().unwrap();
    let next_id = next["messageId"].as_str().unwrap();
    let mut first_device = Feed::open(&fixture, 1, 31).await;
    let mut second_device = Feed::open(&fixture, 1, 31).await;
    let mut author = Feed::open(&fixture, 0, 31).await;
    let mut stranger = Feed::open(&fixture, 2, 31).await;
    let path = format!("/api/auth/inbox/{sender}/read");
    let initial = data(
        &fixture,
        1,
        "PUT",
        path.clone(),
        json!({"messageId":first_id}),
    )
    .await;
    assert_eq!(initial, json!({"readMessageId":first_id}));
    first_device.change("inbox", Some(sender)).await;
    second_device.change("inbox", Some(sender)).await;
    // Reopening the same retained page still succeeds without waking every tab.
    for _ in 0..3 {
        assert_eq!(
            data(
                &fixture,
                1,
                "PUT",
                path.clone(),
                json!({"messageId":first_id}),
            )
            .await,
            initial
        );
    }
    first_device.quiet().await;
    second_device.quiet().await;
    // Two devices racing the same next cursor cause exactly one durable advance
    // and one event per reader device, while both receive the canonical result.
    let (left, right) = tokio::join!(
        data(
            &fixture,
            1,
            "PUT",
            path.clone(),
            json!({"messageId":next_id}),
        ),
        data(
            &fixture,
            1,
            "PUT",
            path.clone(),
            json!({"messageId":next_id}),
        )
    );
    assert_eq!(left, json!({"readMessageId":next_id}));
    assert_eq!(right, left);
    first_device.change("inbox", Some(sender)).await;
    second_device.change("inbox", Some(sender)).await;
    assert_eq!(
        data(
            &fixture,
            1,
            "PUT",
            path.clone(),
            json!({"messageId":first_id}),
        )
        .await,
        left,
        "an older device receives the newer cursor without rewinding it"
    );
    // Expiry is stored per retained row. A longer-lived older row may extend
    // cursor retention, but must not wake every tab as a fresh read advance.
    let retained_expiry: chrono::DateTime<chrono::Utc> = sqlx::query_scalar(
        "UPDATE chat_messages SET expires_at=expires_at+interval '1 day' WHERE id=$1 RETURNING expires_at",
    )
    .bind(first_id.parse::<Uuid>().unwrap())
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(
        data(
            &fixture,
            1,
            "PUT",
            path.clone(),
            json!({"messageId":first_id}),
        )
        .await,
        left
    );
    let (stored_id, stored_expiry): (Uuid, chrono::DateTime<chrono::Utc>) = sqlx::query_as(
        "SELECT r.message_id,r.expires_at FROM chat_read_cursors r
         JOIN chat_messages m ON m.conversation=r.conversation WHERE r.user_id=$1 AND m.id=$2",
    )
    .bind(reader)
    .bind(next_id.parse::<Uuid>().unwrap())
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(stored_id.to_string(), next_id);
    assert_eq!(stored_expiry, retained_expiry);
    let unavailable = operation("PUT", path, Some(json!({"messageId":Uuid::new_v4()})));
    assert_eq!(
        fixture
            .request(&unavailable, Some(&fixture.tokens[1]), 31)
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );
    first_device.quiet().await;
    second_device.quiet().await;
    author.quiet().await;
    stranger.quiet().await;
    drop((first_device, second_device, author, stranger));
    fixture.finish().await;
}

#[tokio::test]
#[ignore = "requires DISPOSABLE_TEST_DATABASE=1 and TEST_DATABASE_URL"]
async fn authorization_database_account_updates_bound_leases_and_close_on_revocation_expiry_lag_and_drain()
 {
    let mut fixture = Fixture::new(3).await;
    let mut feeds = Vec::new();
    for _ in 0..8 {
        feeds.push(Feed::open(&fixture, 0, 2).await);
    }
    let open = operation("GET", "/api/auth/updates".into(), None);
    assert_eq!(
        fixture
            .request(&open, Some(&fixture.tokens[0]), 2)
            .await
            .status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    drop(feeds.pop());
    feeds.push(Feed::open(&fixture, 0, 2).await);
    let hub = fixture.server.room_manager().account_updates();
    for _ in 0..65 {
        hub.changed(fixture.users[0], crate::auth::updates::Scope::Inbox, None);
    }
    for feed in &mut feeds {
        feed.closed().await;
    }
    drop(feeds);
    let mut revoked = Feed::open(&fixture, 0, 2).await;
    // Queue an old notice first: the synchronous revocation generation still
    // forces validation before that notice can be emitted.
    hub.changed(
        fixture.users[0],
        crate::auth::updates::Scope::Contacts,
        None,
    );
    let revoke = operation(
        "DELETE",
        format!("/api/auth/sessions/{}", fixture.sessions[0]),
        None,
    );
    assert_eq!(
        fixture
            .request(&revoke, Some(&fixture.tokens[0]), 2)
            .await
            .status(),
        StatusCode::NO_CONTENT
    );
    revoked.closed().await;
    assert_eq!(
        fixture
            .request(&open, Some(&fixture.tokens[0]), 2)
            .await
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let mut expired = Feed::open(&fixture, 1, 2).await;
    sqlx::query("UPDATE sessions SET expires_at=clock_timestamp()-interval '1 second' WHERE id=$1")
        .bind(fixture.sessions[1])
        .execute(&fixture.pool)
        .await
        .unwrap();
    expired.closed().await;
    let mut old_version = Feed::open(&fixture, 2, 2).await;
    sqlx::query("UPDATE users SET auth_version=1 WHERE id=$1")
        .bind(fixture.users[2])
        .execute(&fixture.pool)
        .await
        .unwrap();
    fixture
        .server
        .revoke_account_sessions(fixture.users[2].to_string(), 1);
    old_version.closed().await;
    assert_eq!(
        fixture
            .request(&open, Some(&fixture.tokens[2]), 2)
            .await
            .status(),
        StatusCode::UNAUTHORIZED
    );
    fixture.tokens[2] = fixture.token(fixture.users[2], fixture.sessions[2], 1);
    let mut drain = Feed::open(&fixture, 2, 2).await;
    fixture.server.begin_draining();
    drain.closed().await;
    drop((revoked, expired, old_version, drain));
    fixture.finish().await;
}

#[tokio::test]
#[ignore = "requires DISPOSABLE_TEST_DATABASE=1 and TEST_DATABASE_URL"]
async fn authorization_database_account_data_and_credentials_have_independent_budgets_and_stream_permits()
 {
    let mut fixture = Fixture::new(1).await;
    fixture.server.auth_guard.max_requests_per_minute = 2;
    fixture.server.account_guard.max_requests_per_minute = 4;
    let app = operation("GET", "/api/auth/inbox".into(), None);
    let ticket = operation("POST", "/api/auth/ws-ticket".into(), Some(json!({})));
    let auth_capacity = fixture.server.auth_guard.concurrency.available_permits();
    let app_capacity = fixture.server.account_guard.concurrency.available_permits();
    let mut feed = Feed::open(&fixture, 0, 10).await;
    assert_eq!(
        fixture.server.auth_guard.concurrency.available_permits(),
        auth_capacity
    );
    assert_eq!(
        fixture.server.account_guard.concurrency.available_permits(),
        app_capacity
    );
    assert_eq!(
        fixture
            .request(&ticket, Some(&fixture.tokens[0]), 10)
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        fixture
            .request(&ticket, Some(&fixture.tokens[0]), 10)
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        fixture
            .request(&ticket, Some(&fixture.tokens[0]), 10)
            .await
            .status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    for _ in 0..3 {
        assert_eq!(
            fixture
                .request(&app, Some(&fixture.tokens[0]), 10)
                .await
                .status(),
            StatusCode::OK
        );
    }
    assert_eq!(
        fixture
            .request(&app, Some(&fixture.tokens[0]), 10)
            .await
            .status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    // Reverse exhaustion: app requests do not consume the credential budget.
    for _ in 0..4 {
        assert_eq!(
            fixture
                .request(&app, Some(&fixture.tokens[0]), 11)
                .await
                .status(),
            StatusCode::OK
        );
    }
    assert_eq!(
        fixture
            .request(&app, Some(&fixture.tokens[0]), 11)
            .await
            .status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(
        fixture
            .request(&ticket, Some(&fixture.tokens[0]), 11)
            .await
            .status(),
        StatusCode::OK
    );
    // An open stream occupies neither ordinary semaphore for its lifetime.
    let auth_permits: Vec<_> = (0..fixture.server.auth_guard.concurrency.available_permits())
        .map(|_| fixture.server.try_acquire_auth_request().unwrap())
        .collect();
    assert_eq!(
        fixture
            .request(&ticket, Some(&fixture.tokens[0]), 12)
            .await
            .status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(
        fixture
            .request(&app, Some(&fixture.tokens[0]), 12)
            .await
            .status(),
        StatusCode::OK
    );
    drop(auth_permits);
    let app_permits: Vec<_> = (0..fixture.server.account_guard.concurrency.available_permits())
        .map(|_| fixture.server.try_acquire_account_request().unwrap())
        .collect();
    assert_eq!(
        fixture
            .request(&app, Some(&fixture.tokens[0]), 13)
            .await
            .status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(
        fixture
            .request(&ticket, Some(&fixture.tokens[0]), 13)
            .await
            .status(),
        StatusCode::OK
    );
    fixture.server.room_manager().account_updates().changed(
        fixture.users[0],
        crate::auth::updates::Scope::Preferences,
        None,
    );
    feed.change("preferences", None).await;
    drop(app_permits);
    drop(feed);
    fixture.finish().await;
}

#[tokio::test]
#[ignore = "requires DISPOSABLE_TEST_DATABASE=1 and TEST_DATABASE_URL"]
async fn authorization_database_account_updates_commit_completion_survives_abandoned_http_response()
{
    let fixture = Fixture::new(1).await;
    let mut feed = Feed::open(&fixture, 0, 20).await;
    let capacity = fixture.server.account_guard.concurrency.available_permits();
    let mut blocker = fixture.pool.begin().await.unwrap();
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *blocker)
        .await
        .unwrap();
    sqlx::query("SELECT id FROM users WHERE id=$1 FOR UPDATE")
        .bind(fixture.users[0])
        .execute(&mut *blocker)
        .await
        .unwrap();
    let mut request = Request::builder()
        .method("PATCH")
        .uri("/api/auth/preferences")
        .header(
            header::AUTHORIZATION,
            format!("Bearer {}", fixture.tokens[0]),
        )
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({"largeText":true}).to_string()))
        .unwrap();
    request
        .extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([192, 0, 2, 20], 12345))));
    let router = fixture.server.clone().router();
    let response = tokio::spawn(async move { router.oneshot(request).await.unwrap() });
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let waiting: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT FROM pg_stat_activity WHERE $1=ANY(pg_blocking_pids(pid)))",
            )
            .bind(pid)
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
            if waiting {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("owned mutation reaches this fixture's row lock");
    response.abort();
    assert!(response.await.unwrap_err().is_cancelled());
    assert_eq!(
        fixture.server.account_guard.concurrency.available_permits(),
        capacity - 1,
        "abandoning the response must retain the owned mutation's resource lease"
    );
    blocker.commit().await.unwrap();
    feed.change("preferences", None).await;
    let saved: Value = sqlx::query_scalar("SELECT preferences FROM users WHERE id=$1")
        .bind(fixture.users[0])
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
    assert_eq!(saved["largeText"], true);
    // Headers complete immediately, and the body remains usable after the
    // ordinary 15-second HTTP request deadline.
    tokio::time::sleep(Duration::from_secs(16)).await;
    fixture.server.room_manager().account_updates().changed(
        fixture.users[0],
        crate::auth::updates::Scope::Notifications,
        None,
    );
    feed.change("notifications", None).await;
    drop(feed);
    fixture.finish().await;
}
