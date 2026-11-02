//! Tests of `src/auth/session.rs`.

#[test]
fn expired_session_sweeps_run_one_at_a_time_and_are_visible_to_drain() {
    let cleanup = super::SessionCleanup::default();
    assert_eq!(cleanup.pending(), 0);
    let first = cleanup.begin().expect("first sweep starts");
    assert!(
        cleanup.begin().is_none(),
        "a concurrent refresh must not start a second table-wide DELETE"
    );
    assert_eq!(cleanup.pending(), 1);
    drop(first);
    assert_eq!(cleanup.pending(), 0);
}

use super::*;

#[test]
fn persisted_token_hashes_remain_lowercase_sha256_hex() {
    // SHA-256 standard known-answer vectors, independent of token generation.
    assert_eq!(
        hash_token(""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(
        hash_token("abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

#[test]
fn fixed_v1_refresh_fixture_preserves_base64url_and_family_hashes() {
    // Fixed current v1n format: 32 0xff generation bytes, then
    // 32 0xfb family bytes. Expected hashes were independently checked
    // with Node's crypto implementation, not this crate's encoder/hasher.
    const GENERATION: &str = "__________________________________________8";
    const FAMILY: &str = "-_v7-_v7-_v7-_v7-_v7-_v7-_v7-_v7-_v7-_v7-_s";
    let raw = format!("v1n{GENERATION}{FAMILY}");
    assert_eq!(URL_SAFE_NO_PAD.encode([0xff; 32]), GENERATION);
    assert_eq!(URL_SAFE_NO_PAD.encode([0xfb; 32]), FAMILY);
    assert_eq!(family_secret(&raw).unwrap(), [0xfb; 32]);
    assert_eq!(
        hash_token(&raw),
        "fb8e7de9c3c058fd86581ba30ca82e14e7ce146e8ba82b2441c1e36e63aa4e7c"
    );
    let successor = build_refresh_token(&[0xfb; 32]).unwrap();
    assert!(successor.raw.ends_with(FAMILY));
    assert_eq!(
        successor.family_hash,
        "456a04986c2572de19b058ef2ef20b0077017bcdb15819af052eb9d5d9b8e504"
    );
    assert!(family_secret(&format!("{raw}=")).is_none());
    assert!(family_secret(&raw.replace('_', "/")).is_none());
}

async fn rotate_and_commit(pool: &PgPool, raw_token: &str) -> Result<RefreshRotation, AuthError> {
    let mut transaction = pool
        .begin()
        .await
        .map_err(crate::auth::routes::database_error)?;
    let outcome = rotate_refresh_token_with(&mut transaction, raw_token).await?;
    transaction
        .commit()
        .await
        .map_err(crate::auth::routes::database_error)?;
    Ok(outcome)
}

/// The migrated disposable database the ignored tests require.
async fn pool() -> PgPool {
    sqlx::postgres::PgPoolOptions::new()
        .max_connections(4)
        .connect(&std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL"))
        .await
        .unwrap()
}

/// A new account with a unique address; `delete_accounts` removes it.
async fn account(pool: &PgPool, display_name: &str) -> Uuid {
    sqlx::query_scalar("INSERT INTO users (email, display_name) VALUES ($1, $2) RETURNING id")
        .bind(format!("session-{}@example.test", Uuid::new_v4()))
        .bind(display_name)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// A committed sign-in: the session's first refresh token and its id.
async fn new_session(pool: &PgPool, user_id: Uuid) -> (RefreshToken, Uuid) {
    let refresh = generate_refresh_token().unwrap();
    let session = create_session(pool, &user_id, &refresh).await.unwrap();
    (refresh, session)
}

/// Remove test accounts; their sessions cascade.
async fn delete_accounts(pool: &PgPool, user_ids: &[Uuid]) {
    sqlx::query("DELETE FROM users WHERE id = ANY($1)")
        .bind(user_ids)
        .execute(pool)
        .await
        .unwrap();
}

#[test]
fn generated_tokens_are_high_entropy_opaque_and_hash_only() {
    let first = generate_refresh_token().unwrap();
    let second = generate_refresh_token().unwrap();

    assert_ne!(first.raw, second.raw);
    assert!(first.raw.starts_with(REFRESH_TOKEN_PREFIX));
    assert_eq!(
        first.raw.len(),
        REFRESH_TOKEN_PREFIX.len() + ENCODED_REFRESH_SECRET_LEN * 2
    );
    assert!(
        first
            .raw
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    );
    assert_eq!(first.token_hash.len(), 64);
    assert_eq!(first.family_hash.len(), 64);
    assert!(!first.token_hash.contains(&first.raw));
    assert!(!first.family_hash.contains(&first.raw));
    assert_eq!(
        family_secret(&first.raw).unwrap().len(),
        REFRESH_SECRET_BYTES
    );
}

#[test]
fn successors_keep_only_the_family_capability() {
    let first = generate_refresh_token().unwrap();
    let family = family_secret(&first.raw).unwrap();
    let successor = build_refresh_token(&family).unwrap();

    assert_ne!(first.raw, successor.raw);
    assert_ne!(first.token_hash, successor.token_hash);
    assert_eq!(first.family_hash, successor.family_hash);
}

#[test]
fn unsupported_or_malformed_tokens_have_no_family_capability() {
    let uuid = Uuid::new_v4().to_string();
    let malformed = [
        String::new(),
        "not-a-uuid".to_owned(),
        uuid.clone(),
        uuid.to_ascii_uppercase(),
        "v1.invalid.invalid".to_owned(),
        format!("v1n{}", "A".repeat(ENCODED_REFRESH_SECRET_LEN * 2 - 1)),
        format!("v1n{}", "A".repeat(ENCODED_REFRESH_SECRET_LEN * 2 + 1)),
        format!("v1l{}", "A".repeat(ENCODED_REFRESH_SECRET_LEN + 48)),
        format!("v1x{}", "A".repeat(ENCODED_REFRESH_SECRET_LEN * 2)),
        format!("v2n{}", "A".repeat(ENCODED_REFRESH_SECRET_LEN * 2)),
    ];
    for raw in malformed {
        assert!(family_secret(&raw).is_none(), "{raw}");
    }
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL pointing to a migrated disposable PostgreSQL database"]
async fn database_current_session_migration_preserves_accounts_and_requires_families() {
    let pool = pool().await;
    let mut transaction = pool.begin().await.unwrap();
    // Shadow the current schema only within this rolled-back transaction.
    // The nullable fixture recreates the pre-022 invariant without changing
    // any persistent database table or already-applied migration ledger.
    sqlx::raw_sql("SET LOCAL search_path TO pg_temp, pg_catalog;
            CREATE TEMP TABLE users (LIKE public.users INCLUDING DEFAULTS INCLUDING CONSTRAINTS);
            CREATE TEMP TABLE webauthn_credentials (LIKE public.webauthn_credentials INCLUDING DEFAULTS INCLUDING CONSTRAINTS);
            CREATE TEMP TABLE room_roles (LIKE public.room_roles INCLUDING DEFAULTS INCLUDING CONSTRAINTS);
            CREATE TEMP TABLE sessions (LIKE public.sessions INCLUDING DEFAULTS INCLUDING CONSTRAINTS);
            ALTER TABLE sessions ALTER COLUMN refresh_token_family_hash DROP NOT NULL;
            INSERT INTO users(id,email,display_name,password_hash,recovery_key_hash)
                VALUES('11111111-1111-4111-8111-111111111111','migration@example.test','Migration test','password-sentinel',repeat('a',64));
            INSERT INTO webauthn_credentials(user_id,credential_json,credential_id)
                VALUES('11111111-1111-4111-8111-111111111111','{\"passkey\":\"sentinel\"}','passkey-sentinel');
            INSERT INTO room_roles(room_id,user_id,role)
                VALUES('migration-room','11111111-1111-4111-8111-111111111111',2);
            INSERT INTO sessions(user_id,refresh_token_hash,refresh_token_family_hash,expires_at)
                VALUES('11111111-1111-4111-8111-111111111111',repeat('b',64),NULL,now()+interval '1 day'),
                      ('11111111-1111-4111-8111-111111111111',repeat('c',64),repeat('d',64),now()+interval '1 day');")
            .execute(&mut *transaction).await.unwrap();
    let before: serde_json::Value = sqlx::query_scalar(
        "SELECT jsonb_build_object(
            'users',(SELECT jsonb_agg(to_jsonb(u)) FROM users u),
            'passkeys',(SELECT jsonb_agg(to_jsonb(c)) FROM webauthn_credentials c),
            'memberships',(SELECT jsonb_agg(to_jsonb(r)) FROM room_roles r))",
    )
    .fetch_one(&mut *transaction)
    .await
    .unwrap();
    sqlx::raw_sql(include_str!(
        "../../migrations/022_require_current_sessions.sql"
    ))
    .execute(&mut *transaction)
    .await
    .unwrap();
    let after: serde_json::Value = sqlx::query_scalar(
        "SELECT jsonb_build_object(
            'users',(SELECT jsonb_agg(to_jsonb(u)) FROM users u),
            'passkeys',(SELECT jsonb_agg(to_jsonb(c)) FROM webauthn_credentials c),
            'memberships',(SELECT jsonb_agg(to_jsonb(r)) FROM room_roles r))",
    )
    .fetch_one(&mut *transaction)
    .await
    .unwrap();
    assert_eq!(
        before, after,
        "credentials, profiles and memberships are unchanged"
    );
    let remaining: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sessions")
        .fetch_one(&mut *transaction)
        .await
        .unwrap();
    assert_eq!(remaining, 0, "every pre-migration sign-in is invalidated");
    let required: bool = sqlx::query_scalar("SELECT attnotnull FROM pg_attribute WHERE attrelid='pg_temp.sessions'::regclass AND attname='refresh_token_family_hash'")
            .fetch_one(&mut *transaction).await.unwrap();
    assert!(required);
    let refresh = generate_refresh_token().unwrap();
    insert_session_with(
        &mut *transaction,
        &Uuid::parse_str("11111111-1111-4111-8111-111111111111").unwrap(),
        &refresh,
    )
    .await
    .unwrap();
    sqlx::query("SAVEPOINT missing_family")
        .execute(&mut *transaction)
        .await
        .unwrap();
    let missing_family = sqlx::query("INSERT INTO sessions(user_id,refresh_token_hash,expires_at) VALUES('11111111-1111-4111-8111-111111111111',repeat('e',64),now()+interval '1 day')")
            .execute(&mut *transaction).await.unwrap_err();
    assert_eq!(
        missing_family
            .as_database_error()
            .unwrap()
            .code()
            .as_deref(),
        Some("23502")
    );
    sqlx::query("ROLLBACK TO SAVEPOINT missing_family")
        .execute(&mut *transaction)
        .await
        .unwrap();
    transaction.rollback().await.unwrap();
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL pointing to a migrated disposable PostgreSQL database"]
async fn database_logout_retires_the_access_token_issued_with_its_session() {
    use crate::auth::access_tokens::{fixtures, revalidate_principal, validate_access_token};
    let pool = pool().await;
    let user_id = account(&pool, "logout test").await;
    let mut sessions = Vec::new();
    for _ in 0..2 {
        sessions.push(new_session(&pool, user_id).await);
    }
    let first_token = fixtures::issue(&pool, sessions[0].1).await;
    let second_token = fixtures::issue(&pool, sessions[1].1).await;
    let first = validate_access_token(&pool, &first_token).await.unwrap();
    let second = validate_access_token(&pool, &second_token).await.unwrap();
    assert_eq!(first.sid, sessions[0].1);

    assert_eq!(
        delete_session_by_token(&pool, &sessions[0].0.raw)
            .await
            .unwrap(),
        Some(UserId::from(user_id)),
        "logout names the account whose session ended"
    );
    assert!(
        validate_access_token(&pool, &first_token).await.is_err(),
        "logging a session out retires the token issued with it"
    );
    assert!(revalidate_principal(&pool, &first).await.is_err());
    assert!(
        validate_access_token(&pool, &second_token).await.is_ok(),
        "another session's token is untouched"
    );
    let mut missing_session = second.clone();
    missing_session.sid = Uuid::new_v4();
    assert!(revalidate_principal(&pool, &missing_session).await.is_err());
    let other_user = account(&pool, "Other account").await;
    let mut wrong_account = second.clone();
    wrong_account.user = crate::ids::UserId::from(other_user);
    assert!(revalidate_principal(&pool, &wrong_account).await.is_err());

    // A refresh rotates the refresh token but keeps the session, so the token
    // issued before it stays valid for the tab still holding it.
    let mut transaction = pool.begin().await.unwrap();
    let rotated = rotate_refresh_token_with(&mut transaction, &sessions[1].0.raw)
        .await
        .unwrap();
    transaction.commit().await.unwrap();
    match rotated {
        RefreshRotation::Rotated { session_id, .. } => assert_eq!(session_id, sessions[1].1),
        _ => panic!("a fresh refresh token rotates"),
    }
    assert!(validate_access_token(&pool, &second_token).await.is_ok());

    sqlx::query("UPDATE sessions SET expires_at=clock_timestamp()-interval '1 second' WHERE id=$1")
        .bind(second.sid)
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        validate_access_token(&pool, &second_token).await.is_err(),
        "an expired session cannot authorize an unexpired access token"
    );
    assert!(revalidate_principal(&pool, &second).await.is_err());
    delete_accounts(&pool, &[user_id, other_user]).await;
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL pointing to a migrated disposable PostgreSQL database"]
async fn refresh_mints_an_opaque_token_for_a_session_without_one() {
    use tower::ServiceExt;
    let pool = pool().await;
    let user_id = account(&pool, "refresh test").await;
    // A session created before access tokens were stored has no token rows.
    let (refresh, sid) = new_session(&pool, user_id).await;
    let rows = || async {
        sqlx::query_as::<_, (String, chrono::DateTime<chrono::Utc>)>(
            "SELECT token_hash, expires_at FROM session_access_tokens WHERE session_id=$1",
        )
        .bind(sid)
        .fetch_all(&pool)
        .await
        .unwrap()
    };
    assert!(rows().await.is_empty());
    let server = crate::signaling::fixtures::account_server(pool.clone()).await;
    let mut request = axum::http::Request::post("/api/auth/refresh")
        .header(
            axum::http::header::COOKIE,
            format!("__Host-refresh_token={}", refresh.raw),
        )
        .body(axum::body::Body::empty())
        .unwrap();
    request
        .extensions_mut()
        .insert(axum::extract::ConnectInfo(std::net::SocketAddr::from((
            [192, 0, 2, 52],
            52_000,
        ))));
    let response = server.router().oneshot(request).await.unwrap();
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    assert!(
        response
            .headers()
            .get_all(axum::http::header::SET_COOKIE)
            .iter()
            .any(|cookie| cookie
                .to_str()
                .unwrap()
                .starts_with("__Host-refresh_token=")),
        "the refresh cookie rotates"
    );
    let body: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap(),
    )
    .unwrap();
    let mut keys = body.as_object().unwrap().keys().collect::<Vec<_>>();
    keys.sort();
    assert_eq!(keys, ["expires_at", "token", "user"]);
    let token = body["token"].as_str().unwrap();
    assert!(crate::auth::access_tokens::valid_shape(token));
    let stored = rows().await;
    assert_eq!(stored.len(), 1);
    assert_eq!(
        stored[0].0,
        hash_token(token),
        "only the token's digest is kept"
    );
    let expires_at: chrono::DateTime<chrono::Utc> =
        serde_json::from_value(body["expires_at"].clone()).unwrap();
    assert_eq!(
        expires_at, stored[0].1,
        "the response states the stored expiry"
    );
    let principal = crate::auth::access_tokens::validate_access_token(&pool, token)
        .await
        .unwrap();
    assert_eq!(principal.sid, sid);
    assert_eq!(principal.expires_at, expires_at);
    delete_accounts(&pool, &[user_id]).await;
}

/// `request` from `192.0.2.octet` through `server`'s real router.
async fn route(
    server: &crate::signaling::SignalingServer,
    mut request: axum::http::Request<axum::body::Body>,
    octet: u8,
) -> axum::response::Response {
    use tower::ServiceExt;
    request
        .extensions_mut()
        .insert(axum::extract::ConnectInfo(std::net::SocketAddr::from((
            [192, 0, 2, octet],
            52_000,
        ))));
    server.clone().router().oneshot(request).await.unwrap()
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL pointing to a migrated disposable PostgreSQL database"]
async fn a_token_issued_near_its_sessions_end_expires_with_the_session() {
    let pool = pool().await;
    let user_id = account(&pool, "session end test").await;
    let (refresh, sid) = new_session(&pool, user_id).await;
    // Signed in five minutes short of the absolute lifetime: a refresh keeps
    // the session only until that end, five minutes from now.
    sqlx::query(
        "UPDATE sessions
         SET created_at = clock_timestamp() - make_interval(days => $2::integer)
             + interval '5 minutes'
         WHERE id = $1",
    )
    .bind(sid)
    .bind(SESSION_ABSOLUTE_LIFETIME_DAYS)
    .execute(&pool)
    .await
    .unwrap();
    let server = crate::signaling::fixtures::account_server(pool.clone()).await;
    let request = axum::http::Request::post("/api/auth/refresh")
        .header(
            axum::http::header::COOKIE,
            format!("__Host-refresh_token={}", refresh.raw),
        )
        .body(axum::body::Body::empty())
        .unwrap();
    let response = route(&server, request, 53).await;
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let body: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap(),
    )
    .unwrap();
    let stated: chrono::DateTime<chrono::Utc> =
        serde_json::from_value(body["expires_at"].clone()).unwrap();
    let session_end: chrono::DateTime<chrono::Utc> =
        sqlx::query_scalar("SELECT expires_at FROM sessions WHERE id = $1")
            .bind(sid)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(stated, session_end, "the token expires with its session");
    assert!(session_end - chrono::Utc::now() <= chrono::TimeDelta::minutes(5));
    delete_accounts(&pool, &[user_id]).await;
}

/// One committed `create_session_with` for `user_id`.
async fn created_session(pool: &PgPool, user_id: Uuid) -> CreatedSession {
    let mut transaction = pool.begin().await.unwrap();
    let created = create_session_with(
        &mut transaction,
        &user_id,
        &generate_refresh_token().unwrap(),
    )
    .await
    .unwrap();
    transaction.commit().await.unwrap();
    created
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL pointing to a migrated disposable PostgreSQL database"]
async fn session_creation_reports_only_the_eviction_of_a_live_session() {
    let pool = pool().await;
    let user_id = account(&pool, "eviction test").await;
    let mut sessions = Vec::new();
    for _ in 0..MAX_ACTIVE_SESSIONS_PER_USER {
        let created = created_session(&pool, user_id).await;
        assert!(!created.evicted_live_session, "below the cap nothing ends");
        sessions.push(created.id);
    }
    let exists = |id: Uuid| {
        let pool = pool.clone();
        async move {
            sqlx::query_scalar::<_, bool>("SELECT EXISTS (SELECT 1 FROM sessions WHERE id = $1)")
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap()
        }
    };
    // The oldest session expires; the next creation removes it, which ends no
    // live session.
    sqlx::query("UPDATE sessions SET expires_at = now() - interval '1 second' WHERE id = $1")
        .bind(sessions[0])
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        !created_session(&pool, user_id).await.evicted_live_session,
        "removing an expired session is no eviction"
    );
    assert!(!exists(sessions[0]).await);
    // Now at the cap, the next creation ends the oldest live session and says so.
    assert!(created_session(&pool, user_id).await.evicted_live_session);
    assert!(!exists(sessions[1]).await, "the oldest live session ended");
    let live: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM sessions WHERE user_id = $1 AND expires_at > now()",
    )
    .bind(user_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(live, MAX_ACTIVE_SESSIONS_PER_USER);
    delete_accounts(&pool, &[user_id]).await;
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL pointing to a migrated disposable PostgreSQL database"]
async fn a_sign_in_that_evicts_a_live_session_notifies_the_accounts_sockets() {
    let pool = pool().await;
    let user_id = account(&pool, "eviction sign-in test").await;
    let password = format!("eviction-sign-in-{}", Uuid::new_v4());
    let email: String =
        sqlx::query_scalar("UPDATE users SET password_hash = $2 WHERE id = $1 RETURNING email")
            .bind(user_id)
            .bind(crate::auth::password::hash_password(&password).unwrap())
            .fetch_one(&pool)
            .await
            .unwrap();
    let server = crate::signaling::fixtures::account_server(pool.clone()).await;
    let mut socket = server
        .room_manager()
        .account_updates()
        .subscribe_revalidation(UserId::from(user_id));
    let login = || {
        axum::http::Request::post("/api/auth/login")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(axum::body::Body::from(
                serde_json::json!({"email": email, "password": password}).to_string(),
            ))
            .unwrap()
    };
    for _ in 1..MAX_ACTIVE_SESSIONS_PER_USER {
        new_session(&pool, user_id).await;
    }
    // With one session to spare, a sign-in ends none and tells no one: the
    // route would have sent its notice before it answered.
    assert_eq!(
        route(&server, login(), 61).await.status(),
        axum::http::StatusCode::OK
    );
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(200), socket.notice())
            .await
            .is_err(),
        "a sign-in that ends no session sends no notice"
    );
    // At the cap, the next sign-in ends the oldest live session.
    assert_eq!(
        route(&server, login(), 62).await.status(),
        axum::http::StatusCode::OK
    );
    tokio::time::timeout(std::time::Duration::from_secs(2), socket.notice())
        .await
        .expect("the evicting sign-in tells the account's sockets to re-check");
    drop(socket);
    delete_accounts(&pool, &[user_id]).await;
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL pointing to a migrated disposable PostgreSQL database"]
async fn database_rotation_replay_grace_and_session_cap() {
    let pool = pool().await;

    let cap_user_id = account(&pool, "cap test").await;
    for _ in 0..=MAX_ACTIVE_SESSIONS_PER_USER {
        new_session(&pool, cap_user_id).await;
    }
    let capped_count: i64 = sqlx::query_scalar("SELECT count(*) FROM sessions WHERE user_id = $1")
        .bind(cap_user_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(capped_count, MAX_ACTIVE_SESSIONS_PER_USER);

    let replay_user_id = account(&pool, "replay test").await;
    let (predecessor, _) = new_session(&pool, replay_user_id).await;

    // Two requests racing with the same bearer serialize: one rotates and
    // one is rejected under grace without authenticating or revoking.
    let (first, second) = tokio::join!(
        rotate_and_commit(&pool, &predecessor.raw),
        rotate_and_commit(&pool, &predecessor.raw)
    );
    let mut successor_raw = None;
    let mut concurrent_requests = 0;
    for outcome in [first.unwrap(), second.unwrap()] {
        match outcome {
            RefreshRotation::Rotated { refresh_token, .. } => {
                successor_raw = Some(refresh_token.raw)
            }
            RefreshRotation::ConcurrentRequest => concurrent_requests += 1,
            RefreshRotation::ReuseDetected { .. } => {
                panic!("concurrent request revoked the family")
            }
        }
    }
    assert_eq!(concurrent_requests, 1);
    let successor_raw = successor_raw.expect("one request rotated");
    let replay_count: i64 = sqlx::query_scalar("SELECT count(*) FROM sessions WHERE user_id = $1")
        .bind(replay_user_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(replay_count, 1);

    // The grace boundary is strict: at two seconds the predecessor is
    // reuse, not a concurrent request, and revokes the live successor.
    sqlx::query(
        "UPDATE sessions
             SET refresh_token_rotated_at =
                 clock_timestamp() - ($2::bigint * interval '1 second')
             WHERE user_id = $1",
    )
    .bind(replay_user_id)
    .bind(CONCURRENT_REFRESH_GRACE_SECONDS)
    .execute(&pool)
    .await
    .unwrap();
    // The revocation names its account, whose connections must hear of it.
    assert!(matches!(
        rotate_and_commit(&pool, &predecessor.raw).await.unwrap(),
        RefreshRotation::ReuseDetected { user_id } if user_id == UserId::from(replay_user_id)
    ));
    let replay_count: i64 = sqlx::query_scalar("SELECT count(*) FROM sessions WHERE user_id = $1")
        .bind(replay_user_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(replay_count, 0);
    assert!(matches!(
        rotate_and_commit(&pool, &successor_raw).await,
        Err(AuthError::InvalidToken)
    ));

    // Grace applies only to the immediate predecessor. An older consumed
    // token revokes the family even when the latest rotation was recent.
    let older_user_id = account(&pool, "older replay test").await;
    let (oldest, _) = new_session(&pool, older_user_id).await;
    let middle = match rotate_and_commit(&pool, &oldest.raw).await.unwrap() {
        RefreshRotation::Rotated { refresh_token, .. } => refresh_token,
        _ => panic!("oldest token did not rotate"),
    };
    assert!(matches!(
        rotate_and_commit(&pool, &middle.raw).await.unwrap(),
        RefreshRotation::Rotated { .. }
    ));
    assert!(matches!(
        rotate_and_commit(&pool, &oldest.raw).await.unwrap(),
        RefreshRotation::ReuseDetected { user_id } if user_id == UserId::from(older_user_id)
    ));

    // A random token cannot identify or revoke an unrelated family.
    let oracle_user_id = account(&pool, "oracle test").await;
    new_session(&pool, oracle_user_id).await;
    let random = generate_refresh_token().unwrap();
    assert!(matches!(
        rotate_and_commit(&pool, &random.raw).await,
        Err(AuthError::InvalidToken)
    ));
    let oracle_count: i64 = sqlx::query_scalar("SELECT count(*) FROM sessions WHERE user_id = $1")
        .bind(oracle_user_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(oracle_count, 1);

    delete_accounts(
        &pool,
        &[cap_user_id, replay_user_id, older_user_id, oracle_user_id],
    )
    .await;
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL pointing to a migrated disposable PostgreSQL database"]
async fn database_refresh_is_refused_after_the_absolute_lifetime() {
    let pool = pool().await;
    let user_id = account(&pool, "lifetime test").await;
    let (refresh, session_id) = new_session(&pool, user_id).await;
    // Signed in 31 days ago yet unexpired, as the uncapped sliding window allowed.
    sqlx::query(
        "UPDATE sessions
         SET created_at = clock_timestamp() - interval '31 days',
             expires_at = clock_timestamp() + interval '6 days'
         WHERE id = $1",
    )
    .bind(session_id)
    .execute(&pool)
    .await
    .unwrap();
    assert!(
        matches!(
            rotate_and_commit(&pool, &refresh.raw).await,
            Err(AuthError::InvalidToken)
        ),
        "a session signed in more than 30 days ago is refused like an expired one"
    );
    delete_accounts(&pool, &[user_id]).await;
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL pointing to a migrated disposable PostgreSQL database"]
async fn database_refresh_expiry_is_capped_at_the_absolute_lifetime() {
    let pool = pool().await;
    let user_id = account(&pool, "lifetime cap test").await;
    let (refresh, session_id) = new_session(&pool, user_id).await;
    sqlx::query(
        "UPDATE sessions SET created_at = clock_timestamp() - interval '29 days' WHERE id = $1",
    )
    .bind(session_id)
    .execute(&pool)
    .await
    .unwrap();
    assert!(
        matches!(
            rotate_and_commit(&pool, &refresh.raw).await.unwrap(),
            RefreshRotation::Rotated { .. }
        ),
        "a session signed in 29 days ago still rotates"
    );
    let (expires_at, cap): (chrono::DateTime<chrono::Utc>, chrono::DateTime<chrono::Utc>) =
        sqlx::query_as(
            "SELECT expires_at, created_at + interval '30 days' FROM sessions WHERE id = $1",
        )
        .bind(session_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        expires_at.timestamp(),
        cap.timestamp(),
        "a refresh never extends a session past 30 days after sign-in"
    );
    delete_accounts(&pool, &[user_id]).await;
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL pointing to a migrated disposable PostgreSQL database"]
async fn database_refresh_keeps_a_young_session_seven_more_days() {
    let pool = pool().await;
    let user_id = account(&pool, "lifetime young test").await;
    let (refresh, session_id) = new_session(&pool, user_id).await;
    assert!(
        matches!(
            rotate_and_commit(&pool, &refresh.raw).await.unwrap(),
            RefreshRotation::Rotated { .. }
        ),
        "a fresh session rotates"
    );
    let (extension, seven_days, before_cap): (String, bool, bool) = sqlx::query_as(
        "SELECT (expires_at - refresh_token_rotated_at)::text,
                expires_at - refresh_token_rotated_at
                    BETWEEN interval '7 days' - interval '1 second'
                        AND interval '7 days' + interval '1 second',
                expires_at < created_at + interval '30 days'
         FROM sessions WHERE id = $1",
    )
    .bind(session_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(
        seven_days,
        "a refresh keeps a young session seven more days, not {extension}"
    );
    assert!(
        before_cap,
        "a young session's refreshed expiry stays before its 30-day cap"
    );
    delete_accounts(&pool, &[user_id]).await;
}
