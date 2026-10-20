//! Tests of `src/auth/passkeys.rs`.

use super::*;

#[test]
fn management_requests_accept_only_the_fixed_operation_fields() {
    assert!(
        serde_json::from_value::<StartRequest>(serde_json::json!({"operation":{"action":"add"}}))
            .is_ok()
    );
    assert!(
        serde_json::from_value::<StartRequest>(serde_json::json!({
            "operation":{"action":"replace","id":Uuid::new_v4()}
        }))
        .is_ok()
    );
    for value in [
        serde_json::json!({"operation":{"action":"add","user_id":Uuid::new_v4()}}),
        serde_json::json!({"operation":{"action":"remove"}}),
        serde_json::json!({"operation":{"action":"remove","id":"credential"}}),
        serde_json::json!({"operation":{"action":"replace"}}),
        serde_json::json!({"operation":{"action":"replace","id":Uuid::new_v4(),"recovery_hash":"untrusted"}}),
        serde_json::json!({"operation":{"action":"recover"}}),
        serde_json::json!({"operation":{"action":"recovery_key"},"authorized":true}),
    ] {
        assert!(serde_json::from_value::<StartRequest>(value).is_err());
    }
}

async fn replacement_fixture(
    pool: &sqlx::PgPool,
    count: usize,
) -> (Uuid, Vec<Uuid>, ReplacementRegistration) {
    let owner = Uuid::new_v4();
    let recovery_hash = "a".repeat(64);
    sqlx::query("INSERT INTO users (id, email, display_name, recovery_key_hash) VALUES ($1, $2, 'Replacement test', $3)")
            .bind(owner).bind(format!("replace-{owner}@example.test")).bind(&recovery_hash)
            .execute(pool).await.unwrap();
    let mut keys = Vec::new();
    for _ in 0..count {
        let key = Uuid::new_v4();
        sqlx::query("INSERT INTO webauthn_credentials (id, user_id, credential_id, credential_json) VALUES ($1, $2, $3, '{}')")
                .bind(key).bind(owner).bind(key.to_string()).execute(pool).await.unwrap();
        keys.push(key);
    }
    session::create_session(pool, &owner, &session::generate_refresh_token().unwrap())
        .await
        .unwrap();
    let replacement = ReplacementRegistration {
        target: keys[0],
        recovery_hash,
    };
    (owner, keys, replacement)
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL pointing to a migrated disposable PostgreSQL database"]
async fn replacement_at_limit_preserves_backup_and_atomically_revokes_sessions() {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect(&std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL"))
        .await
        .unwrap();
    let (owner, keys, replacement) = replacement_fixture(&pool, MAX_PASSKEYS as usize).await;
    let new_id = Uuid::new_v4().to_string();
    let mut transaction = pool.begin().await.unwrap();
    assert_eq!(
        replace_credential(
            &mut transaction,
            owner,
            0,
            &replacement,
            &new_id,
            serde_json::json!({})
        )
        .await
        .unwrap(),
        1
    );
    transaction.commit().await.unwrap();
    let remaining: Vec<Uuid> =
        sqlx::query_scalar("SELECT id FROM webauthn_credentials WHERE user_id = $1")
            .bind(owner)
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(remaining.len(), MAX_PASSKEYS as usize);
    assert!(!remaining.contains(&keys[0]));
    assert!(keys[1..].iter().all(|key| remaining.contains(key)));
    let state: (i64, Option<String>, Option<String>) = sqlx::query_as(
        "SELECT auth_version, password_hash, recovery_key_hash FROM users WHERE id = $1",
    )
    .bind(owner)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(state, (1, None, Some(replacement.recovery_hash)));
    let sessions: i64 = sqlx::query_scalar("SELECT count(*) FROM sessions WHERE user_id = $1")
        .bind(owner)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(sessions, 0);
    let mut transaction = pool.begin().await.unwrap();
    assert!(matches!(
        locked_account(&mut transaction, owner, 0).await,
        Err(AuthError::InvalidToken)
    ));
    transaction.rollback().await.unwrap();
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL pointing to a migrated disposable PostgreSQL database"]
async fn replacement_rejects_foreign_stale_and_duplicate_credentials_without_changes() {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect(&std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL"))
        .await
        .unwrap();
    let (owner, keys, original) = replacement_fixture(&pool, 1).await;
    let (_, foreign, _) = replacement_fixture(&pool, 1).await;
    for (target, hash, version, new_id) in [
        (
            Uuid::new_v4(),
            original.recovery_hash.clone(),
            0,
            Uuid::new_v4().to_string(),
        ),
        (
            foreign[0],
            original.recovery_hash.clone(),
            0,
            Uuid::new_v4().to_string(),
        ),
        (keys[0], "b".repeat(64), 0, Uuid::new_v4().to_string()),
        (
            keys[0],
            original.recovery_hash.clone(),
            1,
            Uuid::new_v4().to_string(),
        ),
        (
            keys[0],
            original.recovery_hash.clone(),
            0,
            keys[0].to_string(),
        ),
        (
            keys[0],
            original.recovery_hash.clone(),
            0,
            foreign[0].to_string(),
        ),
    ] {
        let mut transaction = pool.begin().await.unwrap();
        let replacement = ReplacementRegistration {
            target,
            recovery_hash: hash,
        };
        assert!(
            replace_credential(
                &mut transaction,
                owner,
                version,
                &replacement,
                &new_id,
                serde_json::json!({})
            )
            .await
            .is_err()
        );
        transaction.rollback().await.unwrap();
        let remaining: Vec<Uuid> =
            sqlx::query_scalar("SELECT id FROM webauthn_credentials WHERE user_id = $1")
                .bind(owner)
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(remaining, keys);
        let state: (i64, Option<String>, Option<String>) = sqlx::query_as(
            "SELECT auth_version, password_hash, recovery_key_hash FROM users WHERE id = $1",
        )
        .bind(owner)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(state, (0, None, Some(original.recovery_hash.clone())));
        let sessions: i64 = sqlx::query_scalar("SELECT count(*) FROM sessions WHERE user_id = $1")
            .bind(owner)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(sessions, 1);
    }
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL pointing to a migrated disposable PostgreSQL database"]
async fn full_challenge_store_rolls_back_replacement_backup_preparation() {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect(&std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL"))
        .await
        .unwrap();
    let (owner, keys, _) = replacement_fixture(&pool, 1).await;
    let webauthn = webauthn_rs::prelude::WebauthnBuilder::new(
        "localhost",
        &url::Url::parse("https://localhost").unwrap(),
    )
    .unwrap()
    .build()
    .unwrap();
    let store = ChallengeStore::new();
    let source = IpAddr::from([192, 0, 2, 1]);
    for _ in 0..3 {
        let (_, state) = webauthn.start_discoverable_authentication().unwrap();
        assert!(
            store
                .store_account(
                    AccountChallenge::Authentication(AccountAuthenticationData {
                        state,
                        user_id: owner,
                        auth_version: 0,
                        action: PasskeyAction::RecoveryKey {},
                    }),
                    source
                )
                .is_some()
        );
    }
    let mut transaction = pool.begin().await.unwrap();
    let _owner = locked_account(&mut transaction, owner, 0).await.unwrap();
    sqlx::query("UPDATE users SET recovery_key_hash = repeat('b', 64) WHERE id = $1")
        .bind(owner)
        .execute(&mut *transaction)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE webauthn_credentials SET credential_json = '{\"fixture_counter\":1}' WHERE id = $1",
    )
    .bind(keys[0])
    .execute(&mut *transaction)
    .await
    .unwrap();
    let (_, state) = webauthn
        .start_passkey_registration(owner, "owner@example.test", "Owner", None)
        .unwrap();
    let result = commit_registration(
        transaction,
        &store,
        AccountRegistrationData {
            state,
            user_id: owner,
            auth_version: 0,
            replacement: Some(ReplacementRegistration {
                target: keys[0],
                recovery_hash: "b".repeat(64),
            }),
        },
        source,
    )
    .await;
    assert!(matches!(result, Err(AuthError::RateLimited)));
    let backup: Option<String> =
        sqlx::query_scalar("SELECT recovery_key_hash FROM users WHERE id = $1")
            .bind(owner)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(backup, Some("a".repeat(64)));
    let credential: Value =
        sqlx::query_scalar("SELECT credential_json FROM webauthn_credentials WHERE id = $1")
            .bind(keys[0])
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(credential, serde_json::json!({}));
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL pointing to a migrated disposable PostgreSQL database"]
async fn removal_keeps_a_sign_in_method_and_atomically_revokes_sessions() {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(4)
        .connect(&std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL"))
        .await
        .unwrap();
    let owner = Uuid::new_v4();
    let email = format!("passkey-management-{owner}@example.test");
    sqlx::query("INSERT INTO users (id, email, display_name, recovery_key_hash) VALUES ($1, $2, 'Management test', repeat('a', 64))")
            .bind(owner).bind(email).execute(&pool).await.unwrap();
    let first = Uuid::new_v4();
    sqlx::query("INSERT INTO webauthn_credentials (id, user_id, credential_id, credential_json) VALUES ($1, $2, $3, '{}')")
            .bind(first).bind(owner).bind(first.to_string()).execute(&pool).await.unwrap();
    let mut transaction = pool.begin().await.unwrap();
    let account = locked_account(&mut transaction, owner, 0).await.unwrap();
    assert!(account.recovery_enabled);
    assert!(matches!(
        remove_credential(&mut transaction, owner, 0, first, false).await,
        Err(AuthError::InvalidInput(_))
    ));
    transaction.rollback().await.unwrap();
    let second = Uuid::new_v4();
    sqlx::query("INSERT INTO webauthn_credentials (id, user_id, credential_id, credential_json) VALUES ($1, $2, $3, '{}')")
            .bind(second).bind(owner).bind(second.to_string()).execute(&pool).await.unwrap();
    session::create_session(&pool, &owner, &session::generate_refresh_token().unwrap())
        .await
        .unwrap();
    let mut transaction = pool.begin().await.unwrap();
    let _owner = locked_account(&mut transaction, owner, 0).await.unwrap();
    assert_eq!(
        remove_credential(&mut transaction, owner, 0, first, false)
            .await
            .unwrap(),
        1
    );
    transaction.commit().await.unwrap();
    let remaining: i64 =
        sqlx::query_scalar("SELECT count(*) FROM webauthn_credentials WHERE user_id = $1")
            .bind(owner)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(remaining, 1);
    let sessions: i64 = sqlx::query_scalar("SELECT count(*) FROM sessions WHERE user_id = $1")
        .bind(owner)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(sessions, 0);
    let mut transaction = pool.begin().await.unwrap();
    assert!(matches!(
        locked_account(&mut transaction, owner, 0).await,
        Err(AuthError::InvalidToken)
    ));
    transaction.rollback().await.unwrap();
    // A current password independently preserves sign-in after the final key is removed.
    sqlx::query("UPDATE users SET password_hash = 'test-hash' WHERE id = $1")
        .bind(owner)
        .execute(&pool)
        .await
        .unwrap();
    let mut transaction = pool.begin().await.unwrap();
    let account = locked_account(&mut transaction, owner, 1).await.unwrap();
    assert_eq!(
        remove_credential(
            &mut transaction,
            owner,
            1,
            second,
            account.password_hash.is_some()
        )
        .await
        .unwrap(),
        2
    );
    transaction.commit().await.unwrap();
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL pointing to a migrated disposable PostgreSQL database"]
async fn concurrent_removals_cannot_remove_both_remaining_sign_in_methods() {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(4)
        .connect(&std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL"))
        .await
        .unwrap();
    let owner = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO users (id, email, display_name) VALUES ($1, $2, 'Concurrent management')",
    )
    .bind(owner)
    .bind(format!("concurrent-keys-{owner}@example.test"))
    .execute(&pool)
    .await
    .unwrap();
    let keys = [Uuid::new_v4(), Uuid::new_v4()];
    for key in keys {
        sqlx::query("INSERT INTO webauthn_credentials (id, user_id, credential_id, credential_json) VALUES ($1, $2, $3, '{}')")
                .bind(key).bind(owner).bind(key.to_string()).execute(&pool).await.unwrap();
    }
    let refresh = session::generate_refresh_token().unwrap();
    session::create_session(&pool, &owner, &refresh)
        .await
        .unwrap();
    let barrier = tokio::sync::Barrier::new(2);
    let pool_ref = &pool;
    let barrier_ref = &barrier;
    let remove = |key| async move {
        let mut transaction = pool_ref.begin().await.map_err(routes::database_error)?;
        barrier_ref.wait().await;
        let state = locked_account(&mut transaction, owner, 0).await?;
        let version = remove_credential(
            &mut transaction,
            owner,
            0,
            key,
            state.password_hash.is_some(),
        )
        .await?;
        transaction.commit().await.map_err(routes::database_error)?;
        Ok::<_, AuthError>(version)
    };
    let (first, second) = tokio::join!(remove(keys[0]), remove(keys[1]));
    assert!(matches!(
        (&first, &second),
        (Ok(1), Err(AuthError::InvalidToken)) | (Err(AuthError::InvalidToken), Ok(1))
    ));
    let remaining: i64 =
        sqlx::query_scalar("SELECT count(*) FROM webauthn_credentials WHERE user_id = $1")
            .bind(owner)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(remaining, 1);
    let sessions: i64 = sqlx::query_scalar("SELECT count(*) FROM sessions WHERE user_id = $1")
        .bind(owner)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(sessions, 0);
}
