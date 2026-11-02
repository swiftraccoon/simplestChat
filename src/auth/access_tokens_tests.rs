//! Access tokens through the production issuer and validator. The database
//! cases require a migrated disposable `TEST_DATABASE_URL`.

use super::*;
use crate::auth::session;

async fn pool() -> PgPool {
    PgPool::connect(&std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL"))
        .await
        .unwrap()
}

/// A fresh account and one of its sessions, with no access token yet.
async fn signed_in(pool: &PgPool) -> (Uuid, Uuid) {
    let user: Uuid = sqlx::query_scalar(
        "INSERT INTO users(email,display_name) VALUES($1,'Access token fixture') RETURNING id",
    )
    .bind(format!("{}@access-tokens.invalid", Uuid::new_v4()))
    .fetch_one(pool)
    .await
    .unwrap();
    let sid = session::create_session(pool, &user, &session::generate_refresh_token().unwrap())
        .await
        .unwrap();
    (user, sid)
}

async fn remove(pool: &PgPool, user: Uuid) {
    sqlx::query("DELETE FROM users WHERE id=$1")
        .bind(user)
        .execute(pool)
        .await
        .unwrap();
}

async fn stored(pool: &PgPool, sid: Uuid) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM session_access_tokens WHERE session_id=$1")
        .bind(sid)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// Over `unreachable_pool`, an `InvalidToken` was decided before any query and
/// a `DatabaseError` by an attempted one.
#[tokio::test]
async fn only_the_issued_token_shape_reaches_the_database() {
    let pool = crate::signaling::fixtures::unreachable_pool().await;
    let well_formed = URL_SAFE_NO_PAD.encode([7_u8; TOKEN_BYTES]);
    assert!(valid_shape(&well_formed));
    assert!(matches!(
        validate_access_token(&pool, &well_formed).await,
        Err(AuthError::DatabaseError(_))
    ));
    for refused in [
        "a".repeat(129),
        format!("{well_formed}{}", "A".repeat(86)),
        well_formed.replacen('B', "+", 1),
        format!("{}=", &well_formed[..42]),
        well_formed[..42].to_owned(),
        format!("{well_formed}A"),
        "not.a.jwt".to_owned(),
        String::new(),
    ] {
        assert!(!valid_shape(&refused), "{refused:?}");
        assert!(
            matches!(
                validate_access_token(&pool, &refused).await,
                Err(AuthError::InvalidToken)
            ),
            "{refused:?} reached the database"
        );
    }
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL pointing to a migrated disposable PostgreSQL database"]
async fn access_tokens_validate_through_the_session_row() {
    let pool = pool().await;
    let (user, sid) = signed_in(&pool).await;
    let before = Utc::now();
    let mut transaction = pool.begin().await.unwrap();
    let (token, expires_at) = issue_access_token(&mut transaction, sid).await.unwrap();
    transaction.commit().await.unwrap();
    let token = token.expose_secret();
    assert!(
        valid_shape(token),
        "an opaque 43-character base64url handle"
    );
    let lifetime = chrono::TimeDelta::from_std(ACCESS_TOKEN_LIFETIME).unwrap();
    assert!(expires_at > before + lifetime - chrono::TimeDelta::seconds(5));
    assert!(expires_at <= Utc::now() + lifetime);
    let row: (String, i64, DateTime<Utc>) = sqlx::query_as(
        "SELECT token_hash, auth_version, expires_at FROM session_access_tokens WHERE session_id=$1",
    )
    .bind(sid)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        row.0,
        session::hash_token(token),
        "only the digest is stored"
    );
    assert!(!row.0.contains(token));
    assert_eq!((row.1, row.2), (0, expires_at));
    assert_eq!(
        validate_access_token(&pool, token).await.unwrap(),
        Principal {
            user: UserId::from(user),
            sid,
            auth_version: 0,
            expires_at,
        }
    );
    remove(&pool, user).await;
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL pointing to a migrated disposable PostgreSQL database"]
async fn an_expired_access_token_is_refused() {
    let pool = pool().await;
    let (user, sid) = signed_in(&pool).await;
    let token = fixtures::issue(&pool, sid).await;
    validate_access_token(&pool, &token).await.unwrap();
    sqlx::query(
        "UPDATE session_access_tokens SET expires_at=clock_timestamp()-interval '1 second'
         WHERE session_id=$1",
    )
    .bind(sid)
    .execute(&pool)
    .await
    .unwrap();
    assert!(matches!(
        validate_access_token(&pool, &token).await,
        Err(AuthError::InvalidToken)
    ));
    // An unexpired token of an expired session is refused as well.
    let token = fixtures::issue(&pool, sid).await;
    sqlx::query("UPDATE sessions SET expires_at=clock_timestamp()-interval '1 second' WHERE id=$1")
        .bind(sid)
        .execute(&pool)
        .await
        .unwrap();
    assert!(matches!(
        validate_access_token(&pool, &token).await,
        Err(AuthError::InvalidToken)
    ));
    remove(&pool, user).await;
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL pointing to a migrated disposable PostgreSQL database"]
async fn a_revoked_session_invalidates_its_access_token_immediately() {
    let pool = pool().await;
    let (user, _) = signed_in(&pool).await;
    let refresh = session::generate_refresh_token().unwrap();
    let sid = session::create_session(&pool, &user, &refresh)
        .await
        .unwrap();
    let token = fixtures::issue(&pool, sid).await;
    let principal = validate_access_token(&pool, &token).await.unwrap();
    revalidate_principal(&pool, &principal).await.unwrap();
    assert_eq!(
        session::delete_session_by_token(&pool, &refresh.raw)
            .await
            .unwrap(),
        Some(UserId::from(user))
    );
    assert!(matches!(
        validate_access_token(&pool, &token).await,
        Err(AuthError::InvalidToken)
    ));
    assert!(matches!(
        revalidate_principal(&pool, &principal).await,
        Err(AuthError::InvalidToken)
    ));
    assert_eq!(
        stored(&pool, sid).await,
        0,
        "the session's tokens went with it"
    );
    remove(&pool, user).await;
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL pointing to a migrated disposable PostgreSQL database"]
async fn two_tokens_of_one_session_both_validate() {
    let pool = pool().await;
    let (user, sid) = signed_in(&pool).await;
    // Two tabs share the session's refresh cookie and each holds its own token.
    let first = fixtures::issue(&pool, sid).await;
    let second = fixtures::issue(&pool, sid).await;
    assert_ne!(first, second);
    for token in [&first, &second] {
        let principal = validate_access_token(&pool, token).await.unwrap();
        assert_eq!((principal.user, principal.sid), (UserId::from(user), sid));
    }
    remove(&pool, user).await;
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL pointing to a migrated disposable PostgreSQL database"]
async fn the_cap_evicts_only_the_oldest_token() {
    let pool = pool().await;
    let (user, sid) = signed_in(&pool).await;
    let mut tokens = Vec::new();
    for _ in 0..MAX_ACCESS_TOKENS_PER_SESSION {
        tokens.push(fixtures::issue(&pool, sid).await);
    }
    // An expired token goes at the next issue without counting against the cap.
    sqlx::query(
        "INSERT INTO session_access_tokens(token_hash,session_id,auth_version,expires_at,created_at)
         VALUES($1,$2,0,clock_timestamp()-interval '1 second',clock_timestamp()-interval '1 hour')",
    )
    .bind(session::hash_token("expired-fixture-token"))
    .bind(sid)
    .execute(&pool)
    .await
    .unwrap();
    tokens.push(fixtures::issue(&pool, sid).await);
    assert_eq!(stored(&pool, sid).await, MAX_ACCESS_TOKENS_PER_SESSION);
    assert!(matches!(
        validate_access_token(&pool, &tokens[0]).await,
        Err(AuthError::InvalidToken)
    ));
    for token in &tokens[1..] {
        validate_access_token(&pool, token).await.unwrap();
    }
    remove(&pool, user).await;
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL pointing to a migrated disposable PostgreSQL database"]
async fn ending_the_session_refuses_every_token_at_once() {
    let pool = pool().await;
    let (user, sid) = signed_in(&pool).await;
    let (_, other) = signed_in(&pool).await;
    let mut tokens = Vec::new();
    for _ in 0..3 {
        tokens.push(fixtures::issue(&pool, sid).await);
    }
    let unrelated = fixtures::issue(&pool, other).await;
    sqlx::query("DELETE FROM sessions WHERE id=$1")
        .bind(sid)
        .execute(&pool)
        .await
        .unwrap();
    for token in &tokens {
        assert!(matches!(
            validate_access_token(&pool, token).await,
            Err(AuthError::InvalidToken)
        ));
    }
    assert_eq!(stored(&pool, sid).await, 0);
    validate_access_token(&pool, &unrelated).await.unwrap();
    remove(&pool, user).await;
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL pointing to a migrated disposable PostgreSQL database"]
async fn a_changed_auth_version_refuses_a_token_whose_session_row_survived() {
    let pool = pool().await;
    let (user, sid) = signed_in(&pool).await;
    let token = fixtures::issue(&pool, sid).await;
    let principal = validate_access_token(&pool, &token).await.unwrap();
    sqlx::query("UPDATE users SET auth_version=auth_version+1 WHERE id=$1")
        .bind(user)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        stored(&pool, sid).await,
        1,
        "the session and its row survive"
    );
    assert!(matches!(
        validate_access_token(&pool, &token).await,
        Err(AuthError::InvalidToken)
    ));
    assert!(matches!(
        revalidate_principal(&pool, &principal).await,
        Err(AuthError::InvalidToken)
    ));
    // A token issued now records the new version and is accepted.
    let current = fixtures::principal(&pool, sid).await;
    assert_eq!(current.auth_version, 1);
    remove(&pool, user).await;
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL pointing to a migrated disposable PostgreSQL database"]
async fn issuing_for_a_missing_session_writes_nothing() {
    let pool = pool().await;
    let mut transaction = pool.begin().await.unwrap();
    let missing = Uuid::new_v4();
    assert!(matches!(
        issue_access_token(&mut transaction, missing).await,
        Err(AuthError::InvalidToken)
    ));
    transaction.rollback().await.unwrap();
    assert_eq!(stored(&pool, missing).await, 0);
}
