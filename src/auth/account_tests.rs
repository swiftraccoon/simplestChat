//! Tests of `src/auth/account.rs`.

use super::*;

#[test]
fn recovery_keys_have_full_entropy_shape_and_only_hashes_are_stored() {
    let first = generate_recovery_key().unwrap();
    let second = generate_recovery_key().unwrap();
    assert_ne!(first, second);
    assert_eq!(first.len(), RECOVERY_PREFIX.len() + 43);
    assert_eq!(recovery_hash(&first).unwrap().len(), 64);
    assert!(recovery_hash("guess").is_err());
    assert!(recovery_hash(&format!("{RECOVERY_PREFIX}{}", "A".repeat(1000))).is_err());
}

#[test]
fn preferences_are_validated_without_resetting_unreadable_privacy() {
    let own = Uuid::new_v4();
    let other = Uuid::new_v4();
    let mut preferences = ChatPreferences {
        ignored: vec![IgnoredPerson {
            id: other.to_string(),
            name: "Maya".to_string(),
        }],
        ..ChatPreferences::default()
    };
    assert!(validate_preferences(&preferences, own).is_ok());
    preferences.timestamps = TimestampFormat::Seconds;
    preferences.ignored.push(IgnoredPerson {
        id: own.to_string(),
        name: "Me".to_string(),
    });
    assert!(validate_preferences(&preferences, own).is_err());
    preferences.ignored.pop();
    preferences.ignored.push(IgnoredPerson {
        id: other.to_string(),
        name: "Twice".to_string(),
    });
    assert!(validate_preferences(&preferences, own).is_err());
    preferences.ignored.pop();
    preferences.ignored[0].name = "a\u{7}b".to_string();
    assert!(validate_preferences(&preferences, own).is_err());
    preferences.ignored[0].name = "x".repeat(65);
    assert!(validate_preferences(&preferences, own).is_err());
    preferences.ignored[0].name = "Maya".to_string();
    preferences.ignored = (0..=MAX_IGNORED)
        .map(|_| IgnoredPerson {
            id: Uuid::new_v4().to_string(),
            name: "Crowd".to_string(),
        })
        .collect();
    assert!(validate_preferences(&preferences, own).is_err());

    // Unrelated keys do not change policy, but malformed saved values must
    // not reset ignored accounts or opt-outs to permissive defaults.
    let stored = stored_preferences(
        Some(serde_json::json!({
            "sounds": true,
            "timestamps": "time12",
            "futureSetting": 3,
        })),
        own,
    )
    .unwrap();
    assert!(stored.sounds && stored.allow_private_messages);
    assert_eq!(stored.timestamps, TimestampFormat::Time12);
    assert!(stored_preferences(Some(serde_json::json!({ "sounds": "yes" })), own).is_err());
    assert!(matches!(
        stored_preferences(None, own),
        Err(AuthError::InvalidToken)
    ));
    let serialized = serde_json::to_value(ChatPreferences::default()).unwrap();
    assert_eq!(serialized["allowPrivateMessages"], true);
    assert_eq!(serialized["largeText"], false);
    assert_eq!(serialized["timestamps"], "time");
}

fn preference_patch(value: serde_json::Value) -> PreferencePatch {
    serde_json::from_value(value).unwrap()
}

#[test]
fn preference_patches_reject_null_unknown_empty_and_ambiguous_intents() {
    let own = Uuid::new_v4();
    let peer = Uuid::new_v4();
    for value in [
        serde_json::json!({"unknown": true}),
        serde_json::json!({"allowPrivateMessages": null}),
        serde_json::json!({"sounds": null}),
        serde_json::json!({"largeText": null}),
        serde_json::json!({"timestamps": null}),
        serde_json::json!({"timestamps": "invalid"}),
        serde_json::json!({"ignore": null}),
        serde_json::json!({"unignore": null}),
        serde_json::json!({"ignored": []}),
        serde_json::json!({"ignore": {"id": peer, "name": "Peer", "unknown": true}}),
    ] {
        assert!(serde_json::from_value::<PreferencePatch>(value).is_err());
    }
    for value in [
        serde_json::json!({}),
        serde_json::json!({"ignore": {"id": own, "name": "Me"}}),
        serde_json::json!({"unignore": own}),
        serde_json::json!({"ignore": {"id": peer, "name": "Peer"}, "unignore": peer}),
    ] {
        assert!(
            preference_patch(value)
                .apply(ChatPreferences::default(), own)
                .is_err()
        );
    }
}

#[test]
fn repeated_ignore_and_unignore_are_sets_and_preserve_other_fields() {
    let own = Uuid::new_v4();
    let peer = Uuid::new_v4();
    let mut saved = ChatPreferences {
        allow_private_messages: false,
        sounds: true,
        ..Default::default()
    };
    for _ in 0..2 {
        saved = preference_patch(
            serde_json::json!({"ignore": {"id": peer.to_string().to_uppercase(), "name": "Peer"}}),
        )
        .apply(saved, own)
        .unwrap();
        assert_eq!(saved.ignored.len(), 1);
        assert_eq!(saved.ignored[0].id, peer.to_string());
        assert!(!saved.allow_private_messages && saved.sounds);
    }
    for _ in 0..2 {
        saved = preference_patch(serde_json::json!({"unignore": peer}))
            .apply(saved, own)
            .unwrap();
        assert!(saved.ignored.is_empty());
        assert!(!saved.allow_private_messages && saved.sounds);
    }
}

#[test]
fn expired_and_unrepresentable_claims_cannot_write_preferences() {
    let mut claims = Claims {
        sub: Uuid::new_v4().to_string(),
        name: "Privacy fixture".into(),
        iss: "simplestChat".into(),
        aud: "simplestChat".into(),
        exp: 0,
        auth_version: 0,
        sid: Uuid::new_v4(),
    };
    assert!(matches!(
        ensure_unexpired_claim(&claims),
        Err(AuthError::TokenExpired)
    ));
    claims.exp = usize::MAX;
    assert!(matches!(
        ensure_unexpired_claim(&claims),
        Err(AuthError::InvalidToken)
    ));
}

async fn preference_pool() -> (PgPool, String) {
    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
    use std::str::FromStr;
    assert_eq!(
        std::env::var("DISPOSABLE_TEST_DATABASE").as_deref(),
        Ok("1")
    );
    let options = PgConnectOptions::from_str(
        &std::env::var("TEST_DATABASE_URL").expect("migrated disposable TEST_DATABASE_URL"),
    )
    .unwrap();
    assert!(matches!(options.get_host(), "127.0.0.1" | "::1"));
    assert!(options.get_socket().is_none());
    assert!(
        options
            .get_database()
            .is_some_and(|name| name.ends_with("_test"))
    );
    let application = format!("preference-patch-{}", Uuid::new_v4());
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .acquire_timeout(std::time::Duration::from_secs(3))
        .connect_with(options.application_name(&application).options([
            ("statement_timeout", "8s"),
            ("lock_timeout", "6s"),
            ("idle_in_transaction_session_timeout", "15s"),
        ]))
        .await
        .unwrap();
    (pool, application)
}

async fn preference_account(pool: &PgPool) -> Claims {
    let account: Uuid = sqlx::query_scalar(
        "INSERT INTO users(email,display_name) VALUES($1,'Privacy patch fixture') RETURNING id",
    )
    .bind(format!("{}@preference.invalid", Uuid::new_v4()))
    .fetch_one(pool)
    .await
    .unwrap();
    let sid = session::create_session(pool, &account, &session::generate_refresh_token().unwrap())
        .await
        .unwrap();
    Claims {
        sub: account.to_string(),
        name: "Privacy fixture".into(),
        iss: "simplestChat".into(),
        aud: "simplestChat".into(),
        exp: usize::try_from(chrono::Utc::now().timestamp() + 3600).unwrap(),
        auth_version: 0,
        sid,
    }
}

async fn wait_for_preference_lock(pool: &PgPool, application: &str) {
    wait_for_preference_query_lock(pool, application, "SELECT auth_version FROM users%").await;
}

async fn wait_for_preference_query_lock(pool: &PgPool, application: &str, query: &str) {
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            let waiting: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database()
                     AND application_name=$1 AND wait_event_type='Lock'
                     AND query LIKE $2)",
            )
            .bind(application)
            .bind(query)
            .fetch_one(pool)
            .await
            .unwrap();
            if waiting {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the preference operation must reach its held authorization row lock");
}

#[tokio::test]
#[ignore = "requires migrated disposable TEST_DATABASE_URL"]
async fn database_preference_patches_merge_concurrent_fields_and_repeat_ignore_safely() {
    let (pool, _) = preference_pool().await;
    let claims = preference_account(&pool).await;
    let peer_claims = preference_account(&pool).await;
    let account = Uuid::parse_str(&claims.sub).unwrap();
    let peer = Uuid::parse_str(&peer_claims.sub).unwrap();
    let (privacy, appearance, ignored) = tokio::join!(
        save_preferences(
            &pool,
            &claims,
            preference_patch(serde_json::json!({"allowPrivateMessages": false}))
        ),
        save_preferences(
            &pool,
            &claims,
            preference_patch(serde_json::json!({"largeText": true, "sounds": true}))
        ),
        save_preferences(
            &pool,
            &claims,
            preference_patch(serde_json::json!({"ignore": {"id": peer, "name": "Peer"}}))
        ),
    );
    privacy.unwrap();
    appearance.unwrap();
    ignored.unwrap();
    let saved = load_chat_preferences(&pool, account).await.unwrap();
    assert!(!saved.allow_private_messages && saved.large_text && saved.sounds);
    assert_eq!(saved.ignored.len(), 1);
    for _ in 0..2 {
        save_preferences(&pool, &claims, preference_patch(serde_json::json!({"ignore": {"id": peer.to_string().to_uppercase(), "name": "Peer"}}))).await.unwrap();
    }
    assert_eq!(
        load_chat_preferences(&pool, account)
            .await
            .unwrap()
            .ignored
            .len(),
        1
    );
    for _ in 0..2 {
        save_preferences(
            &pool,
            &claims,
            preference_patch(serde_json::json!({"unignore": peer})),
        )
        .await
        .unwrap();
    }
    let saved = load_chat_preferences(&pool, account).await.unwrap();
    assert!(saved.ignored.is_empty());
    assert!(!saved.allow_private_messages && saved.large_text && saved.sounds);
    sqlx::query("DELETE FROM users WHERE id=ANY($1)")
        .bind(vec![account, peer])
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
}

#[tokio::test]
#[ignore = "requires migrated disposable TEST_DATABASE_URL"]
async fn database_preference_write_rechecks_revocation_and_expiry_after_waiting_for_locks() {
    let (pool, application) = preference_pool().await;
    let mut claims = preference_account(&pool).await;
    let account = Uuid::parse_str(&claims.sub).unwrap();
    let mut blocker = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM users WHERE id=$1 FOR UPDATE")
        .bind(account)
        .execute(&mut *blocker)
        .await
        .unwrap();
    let request_pool = pool.clone();
    let request_claims = claims.clone();
    let queued = tokio::spawn(async move {
        save_preferences(
            &request_pool,
            &request_claims,
            preference_patch(serde_json::json!({"allowPrivateMessages": false})),
        )
        .await
    });
    wait_for_preference_lock(&pool, &application).await;
    sqlx::query("DELETE FROM sessions WHERE id=$1")
        .bind(claims.sid)
        .execute(&mut *blocker)
        .await
        .unwrap();
    blocker.commit().await.unwrap();
    assert!(matches!(
        queued.await.unwrap(),
        Err(AuthError::InvalidToken)
    ));
    assert!(
        load_chat_preferences(&pool, account)
            .await
            .unwrap()
            .allow_private_messages
    );

    claims.sid =
        session::create_session(&pool, &account, &session::generate_refresh_token().unwrap())
            .await
            .unwrap();
    claims.exp = usize::try_from(chrono::Utc::now().timestamp() + 5).unwrap();
    let mut blocker = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM users WHERE id=$1 FOR UPDATE")
        .bind(account)
        .execute(&mut *blocker)
        .await
        .unwrap();
    let request_pool = pool.clone();
    let request_claims = claims.clone();
    let queued = tokio::spawn(async move {
        save_preferences(
            &request_pool,
            &request_claims,
            preference_patch(serde_json::json!({"allowPrivateMessages": false})),
        )
        .await
    });
    wait_for_preference_lock(&pool, &application).await;
    while usize::try_from(chrono::Utc::now().timestamp()).unwrap() < claims.exp {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    blocker.commit().await.unwrap();
    assert!(matches!(
        queued.await.unwrap(),
        Err(AuthError::TokenExpired)
    ));
    assert!(
        load_chat_preferences(&pool, account)
            .await
            .unwrap()
            .allow_private_messages
    );
    sqlx::query("DELETE FROM users WHERE id=$1")
        .bind(account)
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
}

#[tokio::test]
#[ignore = "requires migrated disposable TEST_DATABASE_URL"]
async fn database_preference_read_rechecks_revocation_after_waiting_for_locks() {
    let (pool, application) = preference_pool().await;
    let claims = preference_account(&pool).await;
    let account = Uuid::parse_str(&claims.sub).unwrap();
    let mut blocker = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM users WHERE id=$1 FOR UPDATE")
        .bind(account)
        .execute(&mut *blocker)
        .await
        .unwrap();
    let request_pool = pool.clone();
    let request_claims = claims.clone();
    let queued =
        tokio::spawn(async move { current_preferences(&request_pool, &request_claims).await });
    wait_for_preference_lock(&pool, &application).await;
    sqlx::query("DELETE FROM sessions WHERE id=$1")
        .bind(claims.sid)
        .execute(&mut *blocker)
        .await
        .unwrap();
    blocker.commit().await.unwrap();
    assert!(matches!(
        queued.await.unwrap(),
        Err(AuthError::InvalidToken)
    ));
    sqlx::query("DELETE FROM users WHERE id=$1")
        .bind(account)
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
}

#[tokio::test]
#[ignore = "requires migrated disposable TEST_DATABASE_URL"]
async fn database_preference_read_and_write_recheck_natural_session_expiry_after_session_lock_wait()
{
    let (pool, application) = preference_pool().await;
    for write in [false, true] {
        let claims = preference_account(&pool).await;
        let account = Uuid::parse_str(&claims.sub).unwrap();
        sqlx::query(
            "UPDATE sessions SET expires_at=clock_timestamp()+interval '3 seconds' WHERE id=$1",
        )
        .bind(claims.sid)
        .execute(&pool)
        .await
        .unwrap();
        let mut blocker = pool.begin().await.unwrap();
        // Hold the unchanged session row: the waiting query can qualify it
        // before expiry without seeing any subsequent row-version update.
        sqlx::query("SELECT id FROM sessions WHERE id=$1 FOR UPDATE")
            .bind(claims.sid)
            .execute(&mut *blocker)
            .await
            .unwrap();
        let request_pool = pool.clone();
        let request_claims = claims.clone();
        let queued = tokio::spawn(async move {
            if write {
                save_preferences(
                    &request_pool,
                    &request_claims,
                    preference_patch(serde_json::json!({"allowPrivateMessages": false})),
                )
                .await
            } else {
                current_preferences(&request_pool, &request_claims).await
            }
        });
        wait_for_preference_query_lock(
            &pool,
            &application,
            "SELECT refresh_token_hash FROM sessions%",
        )
        .await;
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let expired: bool = sqlx::query_scalar(
                    "SELECT expires_at<=clock_timestamp() FROM sessions WHERE id=$1",
                )
                .bind(claims.sid)
                .fetch_one(&pool)
                .await
                .unwrap();
                if expired {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the locked issuing session must reach its natural expiry");
        blocker.commit().await.unwrap();
        assert!(
            matches!(queued.await.unwrap(), Err(AuthError::InvalidToken)),
            "{} must reject a session that expired during its row-lock wait",
            if write { "PATCH" } else { "GET" },
        );
        assert!(
            load_chat_preferences(&pool, account)
                .await
                .unwrap()
                .allow_private_messages
        );
        sqlx::query("DELETE FROM users WHERE id=$1")
            .bind(account)
            .execute(&pool)
            .await
            .unwrap();
    }
    pool.close().await;
}

#[test]
fn profile_appearance_requires_known_color_and_treatment() {
    let mut request = serde_json::json!({
        "display_name": "Account name", "avatar_url": null, "bio": "",
        "profile_style": {"color": "teal", "style": "bubble"}
    });
    assert!(serde_json::from_value::<UpdateProfileRequest>(request.clone()).is_ok());
    for appearance in [
        serde_json::json!({"color": "invented", "style": "accent"}),
        serde_json::json!({"color": null, "style": "invented"}),
        serde_json::json!({"color": null, "style": "text", "css": "extra"}),
        serde_json::Value::Null,
    ] {
        request["profile_style"] = appearance;
        assert!(serde_json::from_value::<UpdateProfileRequest>(request.clone()).is_err());
    }
    request.as_object_mut().unwrap().remove("profile_style");
    assert!(serde_json::from_value::<UpdateProfileRequest>(request).is_err());
}

#[test]
fn profile_content_rejects_active_formats_and_unbounded_images() {
    assert!(validate_image_data_url(None).is_ok());
    for value in [
        "https://example.test/avatar.png",
        "data:image/svg+xml;base64,PHN2Zz4=",
        "data:text/html;base64,PGh0bWw+",
        "data:image/png;base64,aGVsbG8=",
    ] {
        assert!(validate_image_data_url(Some(value)).is_err());
    }
    let png = STANDARD.decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aCGUAAAAASUVORK5CYII=").unwrap();
    assert!(
        validate_image_data_url(Some(&format!(
            "data:image/png;base64,{}",
            STANDARD.encode(&png)
        )))
        .is_ok()
    );
    let mut huge_dimensions = png.clone();
    huge_dimensions[16..20].copy_from_slice(&4000u32.to_be_bytes());
    assert!(
        validate_image_data_url(Some(&format!(
            "data:image/png;base64,{}",
            STANDARD.encode(huge_dimensions)
        )))
        .is_err()
    );
    assert!(
        validate_image_data_url(Some(&format!(
            "data:image/png;base64,{}",
            "A".repeat(180_000)
        )))
        .is_err()
    );
    assert!(validate_text("A short bio\nsecond line", 1024, true));
    assert!(!validate_text("bad\0value", 1024, true));
    assert!(!validate_text(&"a".repeat(1025), 1024, true));
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL pointing to a migrated disposable PostgreSQL database"]
async fn recovery_is_atomic_one_time_and_revokes_all_sessions() {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(4)
        .connect(&std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL"))
        .await
        .unwrap();
    let email = format!("recovery-{}@example.test", Uuid::new_v4());
    let key = generate_recovery_key().unwrap();
    let key_hash = recovery_hash(&key).unwrap();
    let id: Uuid = sqlx::query_scalar("INSERT INTO users (email, display_name, password_hash, recovery_key_hash) VALUES ($1, 'Recovery test', 'old-hash', $2) RETURNING id")
            .bind(&email).bind(&key_hash).fetch_one(&pool).await.unwrap();
    let secret = jwt::JwtSecret::from("account-recovery-test-secret-at-least-32-bytes");
    let original_session =
        session::create_session(&pool, &id, &session::generate_refresh_token().unwrap())
            .await
            .unwrap();
    session::create_session(&pool, &id, &session::generate_refresh_token().unwrap())
        .await
        .unwrap();
    let old_token = jwt::create_session_token(
        &id.to_string(),
        "Recovery test",
        &secret,
        0,
        original_session,
    )
    .unwrap();
    let old_claims = jwt::validate_token(&old_token, &secret).unwrap();
    assert!(
        jwt::validate_current_claims(&pool, &old_claims)
            .await
            .is_ok()
    );
    assert!(
        consume_recovery(&pool, &email, &session::hash_token("wrong"), "bad-hash")
            .await
            .is_err()
    );
    let still_live: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sessions WHERE user_id = $1")
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(still_live, 2);
    let (first, second) = tokio::join!(
        consume_recovery(&pool, &email, &key_hash, "new-one"),
        consume_recovery(&pool, &email, &key_hash, "new-two")
    );
    assert_ne!(first.is_ok(), second.is_ok());
    let row: (Option<String>, i64, Option<String>) = sqlx::query_as(
        "SELECT recovery_key_hash, auth_version, password_hash FROM users WHERE id = $1",
    )
    .bind(id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(row.0.is_none());
    assert_eq!(row.1, 1);
    assert!(matches!(row.2.as_deref(), Some("new-one" | "new-two")));
    let remaining: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sessions WHERE user_id = $1")
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(remaining, 0);
    assert!(matches!(
        jwt::validate_current_claims(&pool, &old_claims).await,
        Err(AuthError::InvalidToken)
    ));
    let new_session =
        session::create_session(&pool, &id, &session::generate_refresh_token().unwrap())
            .await
            .unwrap();
    let current_token =
        jwt::create_session_token(&id.to_string(), "Recovery test", &secret, 1, new_session)
            .unwrap();
    let current_claims = jwt::validate_token(&current_token, &secret).unwrap();
    assert!(
        jwt::validate_current_claims(&pool, &current_claims)
            .await
            .is_ok()
    );
    assert!(
        consume_recovery(&pool, &email, &key_hash, "replay")
            .await
            .is_err()
    );
    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
}
