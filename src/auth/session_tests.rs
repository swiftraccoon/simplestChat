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
    let pool = PgPool::connect(&std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL"))
        .await
        .unwrap();
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
    use crate::auth::jwt::{create_session_token, validate_current_claims, validate_token};
    let database_url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL");
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(4)
        .connect(&database_url)
        .await
        .unwrap();
    let user_id: Uuid = sqlx::query_scalar(
        "INSERT INTO users (email, display_name)
             VALUES ($1, 'logout test') RETURNING id",
    )
    .bind(format!("logout-{}@example.test", Uuid::new_v4()))
    .fetch_one(&pool)
    .await
    .unwrap();
    let secret =
        crate::auth::jwt::JwtSecret::from("logout-revocation-test-secret-with-enough-bytes");
    let mut sessions = Vec::new();
    for _ in 0..2 {
        let refresh = generate_refresh_token().unwrap();
        let mut transaction = pool.begin().await.unwrap();
        let session = create_session_with(&mut transaction, &user_id, &refresh)
            .await
            .unwrap();
        transaction.commit().await.unwrap();
        sessions.push((refresh, session));
    }
    let subject = user_id.to_string();
    let token = |session: Uuid| {
        let issued = create_session_token(&subject, "logout test", &secret, 0, session).unwrap();
        validate_token(&issued, &secret).unwrap()
    };
    let (first, second) = (token(sessions[0].1), token(sessions[1].1));
    assert_eq!(first.sid, sessions[0].1);
    assert!(validate_current_claims(&pool, &first).await.is_ok());

    assert!(
        delete_session_by_token(&pool, &sessions[0].0.raw)
            .await
            .unwrap()
    );
    assert!(
        validate_current_claims(&pool, &first).await.is_err(),
        "logging a session out retires the token issued with it"
    );
    assert!(
        validate_current_claims(&pool, &second).await.is_ok(),
        "another session's token is untouched"
    );
    let missing_session = token(Uuid::new_v4());
    assert!(
        validate_current_claims(&pool, &missing_session)
            .await
            .is_err()
    );
    let other_user: Uuid = sqlx::query_scalar(
        "INSERT INTO users(email,display_name) VALUES($1,'Other account') RETURNING id",
    )
    .bind(format!("session-owner-{}@example.test", Uuid::new_v4()))
    .fetch_one(&pool)
    .await
    .unwrap();
    let mut wrong_account = second.clone();
    wrong_account.sub = other_user.to_string();
    assert!(
        validate_current_claims(&pool, &wrong_account)
            .await
            .is_err()
    );

    // A refresh rotates the token but keeps the session, so the next access
    // token names the same session and stays valid.
    let mut transaction = pool.begin().await.unwrap();
    let rotated = rotate_refresh_token_with(&mut transaction, &sessions[1].0.raw)
        .await
        .unwrap();
    transaction.commit().await.unwrap();
    match rotated {
        RefreshRotation::Rotated { session_id, .. } => assert_eq!(session_id, sessions[1].1),
        _ => panic!("a fresh refresh token rotates"),
    }
    assert!(validate_current_claims(&pool, &second).await.is_ok());

    sqlx::query("UPDATE sessions SET expires_at=clock_timestamp()-interval '1 second' WHERE id=$1")
        .bind(second.sid)
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        validate_current_claims(&pool, &second).await.is_err(),
        "an expired session cannot authorize an unexpired JWT"
    );
    sqlx::query("DELETE FROM users WHERE id = ANY($1)")
        .bind(&[user_id, other_user][..])
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL pointing to a migrated disposable PostgreSQL database"]
async fn database_rotation_replay_grace_and_session_cap() {
    let database_url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL");
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(8)
        .connect(&database_url)
        .await
        .unwrap();

    let cap_user_id: Uuid = sqlx::query_scalar(
        "INSERT INTO users (email, display_name)
             VALUES ($1, 'cap test') RETURNING id",
    )
    .bind(format!("cap-{}@example.test", Uuid::new_v4()))
    .fetch_one(&pool)
    .await
    .unwrap();
    for _ in 0..=MAX_ACTIVE_SESSIONS_PER_USER {
        let token = generate_refresh_token().unwrap();
        create_session(&pool, &cap_user_id, &token).await.unwrap();
    }
    let capped_count: i64 = sqlx::query_scalar("SELECT count(*) FROM sessions WHERE user_id = $1")
        .bind(cap_user_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(capped_count, MAX_ACTIVE_SESSIONS_PER_USER);

    let replay_user_id: Uuid = sqlx::query_scalar(
        "INSERT INTO users (email, display_name)
             VALUES ($1, 'replay test') RETURNING id",
    )
    .bind(format!("replay-{}@example.test", Uuid::new_v4()))
    .fetch_one(&pool)
    .await
    .unwrap();
    let predecessor = generate_refresh_token().unwrap();
    create_session(&pool, &replay_user_id, &predecessor)
        .await
        .unwrap();

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
            RefreshRotation::ReuseDetected => panic!("concurrent request revoked the family"),
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
    assert!(matches!(
        rotate_and_commit(&pool, &predecessor.raw).await.unwrap(),
        RefreshRotation::ReuseDetected
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
    let older_user_id: Uuid = sqlx::query_scalar(
        "INSERT INTO users (email, display_name)
             VALUES ($1, 'older replay test') RETURNING id",
    )
    .bind(format!("older-{}@example.test", Uuid::new_v4()))
    .fetch_one(&pool)
    .await
    .unwrap();
    let oldest = generate_refresh_token().unwrap();
    create_session(&pool, &older_user_id, &oldest)
        .await
        .unwrap();
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
        RefreshRotation::ReuseDetected
    ));

    // A random token cannot identify or revoke an unrelated family.
    let oracle_user_id: Uuid = sqlx::query_scalar(
        "INSERT INTO users (email, display_name)
             VALUES ($1, 'oracle test') RETURNING id",
    )
    .bind(format!("oracle-{}@example.test", Uuid::new_v4()))
    .fetch_one(&pool)
    .await
    .unwrap();
    let current = generate_refresh_token().unwrap();
    create_session(&pool, &oracle_user_id, &current)
        .await
        .unwrap();
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

    sqlx::query("DELETE FROM users WHERE id = ANY($1)")
        .bind(&[cap_user_id, replay_user_id, older_user_id, oracle_user_id][..])
        .execute(&pool)
        .await
        .unwrap();
}
