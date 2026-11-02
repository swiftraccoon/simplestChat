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
fn an_expired_principal_cannot_write_preferences() {
    let mut principal = Principal {
        user: UserId::random(),
        sid: Uuid::new_v4(),
        auth_version: 0,
        expires_at: chrono::Utc::now(),
    };
    assert!(matches!(
        principal.ensure_unexpired(),
        Err(AuthError::TokenExpired)
    ));
    principal.expires_at += chrono::TimeDelta::seconds(60);
    assert!(principal.ensure_unexpired().is_ok());
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

async fn preference_account(pool: &PgPool) -> Principal {
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
    access_tokens::fixtures::principal(pool, sid).await
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
    let principal = preference_account(&pool).await;
    let peer_principal = preference_account(&pool).await;
    let account = principal.user.as_uuid();
    let peer = peer_principal.user.as_uuid();
    let (privacy, appearance, ignored) = tokio::join!(
        save_preferences(
            &pool,
            &principal,
            preference_patch(serde_json::json!({"allowPrivateMessages": false}))
        ),
        save_preferences(
            &pool,
            &principal,
            preference_patch(serde_json::json!({"largeText": true, "sounds": true}))
        ),
        save_preferences(
            &pool,
            &principal,
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
        save_preferences(&pool, &principal, preference_patch(serde_json::json!({"ignore": {"id": peer.to_string().to_uppercase(), "name": "Peer"}}))).await.unwrap();
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
            &principal,
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
    let mut principal = preference_account(&pool).await;
    let account = principal.user.as_uuid();
    let mut blocker = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM users WHERE id=$1 FOR UPDATE")
        .bind(account)
        .execute(&mut *blocker)
        .await
        .unwrap();
    let request_pool = pool.clone();
    let request_principal = principal.clone();
    let queued = tokio::spawn(async move {
        save_preferences(
            &request_pool,
            &request_principal,
            preference_patch(serde_json::json!({"allowPrivateMessages": false})),
        )
        .await
    });
    wait_for_preference_lock(&pool, &application).await;
    sqlx::query("DELETE FROM sessions WHERE id=$1")
        .bind(principal.sid)
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

    principal.sid =
        session::create_session(&pool, &account, &session::generate_refresh_token().unwrap())
            .await
            .unwrap();
    principal.expires_at = chrono::Utc::now() + chrono::TimeDelta::seconds(5);
    let mut blocker = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM users WHERE id=$1 FOR UPDATE")
        .bind(account)
        .execute(&mut *blocker)
        .await
        .unwrap();
    let request_pool = pool.clone();
    let request_principal = principal.clone();
    let queued = tokio::spawn(async move {
        save_preferences(
            &request_pool,
            &request_principal,
            preference_patch(serde_json::json!({"allowPrivateMessages": false})),
        )
        .await
    });
    wait_for_preference_lock(&pool, &application).await;
    while chrono::Utc::now() < principal.expires_at {
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
    let principal = preference_account(&pool).await;
    let account = principal.user.as_uuid();
    let mut blocker = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM users WHERE id=$1 FOR UPDATE")
        .bind(account)
        .execute(&mut *blocker)
        .await
        .unwrap();
    let request_pool = pool.clone();
    let request_principal = principal.clone();
    let queued =
        tokio::spawn(async move { current_preferences(&request_pool, &request_principal).await });
    wait_for_preference_lock(&pool, &application).await;
    sqlx::query("DELETE FROM sessions WHERE id=$1")
        .bind(principal.sid)
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
        let principal = preference_account(&pool).await;
        let account = principal.user.as_uuid();
        sqlx::query(
            "UPDATE sessions SET expires_at=clock_timestamp()+interval '3 seconds' WHERE id=$1",
        )
        .bind(principal.sid)
        .execute(&pool)
        .await
        .unwrap();
        let mut blocker = pool.begin().await.unwrap();
        // Hold the unchanged session row: the waiting query can qualify it
        // before expiry without seeing any subsequent row-version update.
        sqlx::query("SELECT id FROM sessions WHERE id=$1 FOR UPDATE")
            .bind(principal.sid)
            .execute(&mut *blocker)
            .await
            .unwrap();
        let request_pool = pool.clone();
        let request_principal = principal.clone();
        let queued = tokio::spawn(async move {
            if write {
                save_preferences(
                    &request_pool,
                    &request_principal,
                    preference_patch(serde_json::json!({"allowPrivateMessages": false})),
                )
                .await
            } else {
                current_preferences(&request_pool, &request_principal).await
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
                .bind(principal.sid)
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
    let png = STANDARD
        .decode(FIRST_PICTURE.split_once(',').unwrap().1)
        .unwrap();
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
    let original_session =
        session::create_session(&pool, &id, &session::generate_refresh_token().unwrap())
            .await
            .unwrap();
    session::create_session(&pool, &id, &session::generate_refresh_token().unwrap())
        .await
        .unwrap();
    let old_token = access_tokens::fixtures::issue(&pool, original_session).await;
    assert!(
        access_tokens::validate_access_token(&pool, &old_token)
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
        access_tokens::validate_access_token(&pool, &old_token).await,
        Err(AuthError::InvalidToken)
    ));
    let new_session =
        session::create_session(&pool, &id, &session::generate_refresh_token().unwrap())
            .await
            .unwrap();
    let current_token = access_tokens::fixtures::issue(&pool, new_session).await;
    assert_eq!(
        access_tokens::validate_access_token(&pool, &current_token)
            .await
            .unwrap()
            .auth_version,
        1,
        "a token issued after recovery records the new version"
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

#[test]
fn avatar_links_name_the_account_and_a_sixteen_hex_digest() {
    // SHA-256 of the empty string begins e3b0c44298fc1c14.
    assert_eq!(avatar_digest(""), "e3b0c44298fc1c14");
    assert_eq!(
        profile_avatar_url(UserId::from(Uuid::nil()), &avatar_digest("")),
        "/api/auth/profiles/00000000-0000-0000-0000-000000000000/avatar?v=e3b0c44298fc1c14"
    );
    for (version, digest) in [
        (Some("0123456789abcdef"), Some("0123456789abcdef")),
        (Some("0123456789ABCDEF"), None),
        (Some("0123456789abcde"), None),
        (Some("0123456789abcdef0"), None),
        (Some("0123456789abcdeg"), None),
        (None, None),
    ] {
        let query = ImageVersion {
            v: version.map(str::to_owned),
        };
        assert_eq!(query.digest(), digest, "{version:?}");
    }
}

/// Two different 1×1 PNG pictures, as the browser uploads them.
const FIRST_PICTURE: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aCGUAAAAASUVORK5CYII=";
const SECOND_PICTURE: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+ip1sAAAAASUVORK5CYII=";

/// A 1×1 JPEG (start of image, baseline frame header, end of image) as a data URL.
fn jpeg_picture() -> String {
    let frame: [u8; 23] = [
        0xff, 0xd8, 0xff, 0xc0, 0x00, 0x11, 0x08, 0x00, 0x01, 0x00, 0x01, 0x03, 0x01, 0x11, 0x00,
        0x02, 0x11, 0x01, 0x03, 0x11, 0x01, 0xff, 0xd9,
    ];
    format!("data:image/jpeg;base64,{}", STANDARD.encode(frame))
}

/// A 1×1 lossless WebP header (RIFF size, VP8L signature, zero packed size) as a data URL.
fn webp_picture() -> String {
    let mut riff = Vec::with_capacity(30);
    riff.extend_from_slice(b"RIFF");
    riff.extend_from_slice(&22u32.to_le_bytes());
    riff.extend_from_slice(b"WEBPVP8L");
    riff.extend_from_slice(&10u32.to_le_bytes());
    riff.push(0x2f);
    riff.resize(30, 0);
    format!("data:image/webp;base64,{}", STANDARD.encode(riff))
}

/// A signed-in account on `account_server`, driven through the server's own router
/// the way the browser reaches its profile and avatar.
struct ProfileFixture {
    pool: PgPool,
    server: SignalingServer,
    principal: Principal,
}

impl ProfileFixture {
    async fn new() -> Self {
        let (pool, _) = preference_pool().await;
        let principal = preference_account(&pool).await;
        let server = crate::signaling::fixtures::account_server(pool.clone()).await;
        Self {
            pool,
            server,
            principal,
        }
    }

    fn id(&self) -> Uuid {
        self.principal.user.as_uuid()
    }

    /// The avatar route for this account and `version`.
    fn avatar(&self, version: &str) -> String {
        format!("/api/auth/profiles/{}/avatar?v={version}", self.id())
    }

    async fn send(
        &self,
        request: axum::http::request::Builder,
        body: axum::body::Body,
        signed_in: bool,
    ) -> axum::response::Response {
        use tower::ServiceExt;
        let mut request = request.body(body).unwrap();
        if signed_in {
            request
                .headers_mut()
                .extend(crate::signaling::fixtures::bearer(&self.pool, self.principal.sid).await);
        }
        request
            .extensions_mut()
            .insert(axum::extract::ConnectInfo(std::net::SocketAddr::from((
                [192, 0, 2, 41],
                41_000,
            ))));
        self.server.clone().router().oneshot(request).await.unwrap()
    }

    async fn get(&self, uri: &str, signed_in: bool) -> axum::response::Response {
        let request = axum::http::Request::get(uri);
        self.send(request, axum::body::Body::empty(), signed_in)
            .await
    }

    /// Save the profile with `fields` over a fixed name, bio and appearance, and
    /// return the profile the update answers with.
    async fn save(&self, fields: serde_json::Value) -> serde_json::Value {
        let mut body = serde_json::json!({
            "display_name": "Profile fixture", "bio": "",
            "profile_style": {"color": null, "style": "accent"}
        });
        body.as_object_mut()
            .unwrap()
            .extend(fields.as_object().unwrap().clone());
        let request = axum::http::Request::patch("/api/auth/profile")
            .header(axum::http::header::CONTENT_TYPE, "application/json");
        let response = self
            .send(request, axum::body::Body::from(body.to_string()), true)
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        json_body(response).await
    }

    /// The stored picture and its digest.
    async fn stored(&self) -> (Option<String>, Option<String>) {
        sqlx::query_as("SELECT avatar_url, avatar_digest FROM users WHERE id=$1")
            .bind(self.id())
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }

    async fn close(self) {
        sqlx::query("DELETE FROM users WHERE id=$1")
            .bind(self.id())
            .execute(&self.pool)
            .await
            .unwrap();
        self.pool.close().await;
    }
}

async fn json_body(response: axum::response::Response) -> serde_json::Value {
    serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap(),
    )
    .unwrap()
}

/// The digest a profile's `avatar_url` names; panics unless the field is this
/// account's avatar route with a 16-hex digest.
fn avatar_version(profile: &serde_json::Value, account: Uuid) -> String {
    let url = profile["avatar_url"].as_str().unwrap_or_default();
    let version = url
        .strip_prefix(&format!("/api/auth/profiles/{account}/avatar?v="))
        .unwrap_or_else(|| panic!("the profile names its avatar by path, not {url:.40}"));
    assert!(
        version.len() == 16
            && version
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f')),
        "{version}"
    );
    version.to_owned()
}

#[tokio::test]
#[ignore = "requires migrated disposable TEST_DATABASE_URL"]
async fn profiles_never_carry_data_urls() {
    let fixture = ProfileFixture::new().await;
    let saved = fixture
        .save(serde_json::json!({"avatar_url": FIRST_PICTURE}))
        .await;
    let version = avatar_version(&saved, fixture.id());
    let own = json_body(fixture.get("/api/auth/profile", true).await).await;
    let public = format!("/api/auth/profiles/{}", fixture.id());
    let public = json_body(fixture.get(&public, false).await).await;
    for profile in [&saved, &own, &public] {
        assert_eq!(profile["avatar_url"], fixture.avatar(&version));
        assert!(!profile.to_string().contains("data:"), "{profile}");
    }
    assert_eq!(fixture.stored().await.1, Some(version));
    fixture.close().await;
}

#[tokio::test]
#[ignore = "requires migrated disposable TEST_DATABASE_URL"]
async fn avatar_responses_are_immutable_and_sandboxed() {
    let fixture = ProfileFixture::new().await;
    for (picture, content_type) in [
        (FIRST_PICTURE.to_owned(), "image/png"),
        (jpeg_picture(), "image/jpeg"),
        (webp_picture(), "image/webp"),
    ] {
        let saved = fixture
            .save(serde_json::json!({"avatar_url": picture}))
            .await;
        let version = avatar_version(&saved, fixture.id());
        // Anyone who can read the public profile can fetch the picture it names.
        let response = fixture.get(&fixture.avatar(&version), false).await;
        assert_eq!(response.status(), StatusCode::OK);
        let headers = response.headers().clone();
        for (name, value) in [
            (axum::http::header::CONTENT_TYPE, content_type),
            (
                axum::http::header::CACHE_CONTROL,
                "public, max-age=31536000, immutable",
            ),
            (
                axum::http::header::CONTENT_SECURITY_POLICY,
                "default-src 'none'; sandbox",
            ),
            (axum::http::header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ] {
            assert_eq!(headers[&name], value, "{name}");
        }
        let (_, encoded) = picture.split_once(',').unwrap();
        assert_eq!(
            axum::body::to_bytes(response.into_body(), 256 * 1024)
                .await
                .unwrap(),
            STANDARD.decode(encoded).unwrap(),
        );
    }
    fixture.close().await;
}

#[tokio::test]
#[ignore = "requires migrated disposable TEST_DATABASE_URL"]
async fn stale_digests_are_not_found() {
    let fixture = ProfileFixture::new().await;
    let first = avatar_version(
        &fixture
            .save(serde_json::json!({"avatar_url": FIRST_PICTURE}))
            .await,
        fixture.id(),
    );
    let second = avatar_version(
        &fixture
            .save(serde_json::json!({"avatar_url": SECOND_PICTURE}))
            .await,
        fixture.id(),
    );
    assert_ne!(first, second);
    let current = fixture.avatar(&second);
    assert_eq!(fixture.get(&current, false).await.status(), StatusCode::OK);
    for stale in [
        fixture.avatar(&first),
        fixture.avatar(&second.to_uppercase()),
        fixture.avatar(&second[..15]),
        current.replace(&format!("?v={second}"), ""),
        current.replace(&fixture.id().to_string(), &Uuid::new_v4().to_string()),
    ] {
        let status = fixture.get(&stale, false).await.status();
        assert_eq!(status, StatusCode::NOT_FOUND, "{stale}");
    }
    fixture.save(serde_json::json!({"avatar_url": null})).await;
    let status = fixture.get(&current, false).await.status();
    assert_eq!(status, StatusCode::NOT_FOUND, "a removed picture");
    fixture.close().await;
}

/// Omitting `avatar_url` keeps the picture, `null` removes it with its digest,
/// and a new data URL replaces both.
#[tokio::test]
#[ignore = "requires migrated disposable TEST_DATABASE_URL"]
async fn a_profile_update_keeps_removes_or_replaces_the_avatar() {
    let fixture = ProfileFixture::new().await;
    fixture
        .save(serde_json::json!({"avatar_url": FIRST_PICTURE}))
        .await;
    let first = fixture.stored().await;
    assert_eq!(first.0.as_deref(), Some(FIRST_PICTURE));
    assert_eq!(
        first.1.as_deref(),
        Some(avatar_digest(FIRST_PICTURE).as_str())
    );
    let kept = fixture.save(serde_json::json!({})).await;
    assert_eq!(
        fixture.stored().await,
        first,
        "an omitted avatar_url keeps it"
    );
    assert_eq!(
        avatar_version(&kept, fixture.id()),
        avatar_digest(FIRST_PICTURE)
    );
    fixture
        .save(serde_json::json!({"avatar_url": SECOND_PICTURE}))
        .await;
    let replaced = fixture.stored().await;
    assert_eq!(replaced.0.as_deref(), Some(SECOND_PICTURE));
    assert_eq!(
        replaced.1.as_deref(),
        Some(avatar_digest(SECOND_PICTURE).as_str())
    );
    let removed = fixture.save(serde_json::json!({"avatar_url": null})).await;
    assert_eq!(fixture.stored().await, (None, None), "null removes it");
    assert_eq!(removed["avatar_url"], serde_json::Value::Null);
    fixture.close().await;
}

#[tokio::test]
#[ignore = "requires migrated disposable TEST_DATABASE_URL"]
async fn backfill_digest_matches_runtime_digest() {
    let fixture = ProfileFixture::new().await;
    let migration = include_str!("../../migrations/037_users_avatar_digest.sql");
    let backfill = migration
        .lines()
        .find(|line| line.starts_with("UPDATE users SET avatar_digest"))
        .expect("the migration backfills the digest in one statement");
    let jpeg = jpeg_picture();
    let pictures = [
        (FIRST_PICTURE, StatusCode::OK),
        (SECOND_PICTURE, StatusCode::OK),
        (jpeg.as_str(), StatusCode::OK),
        ("data:image/png;base64,ÄÖ€", StatusCode::NOT_FOUND),
    ];
    // The migration's statement runs unmodified, yet touches only fixture rows:
    // a temporary table named users shadows the permanent one on this
    // transaction's connection, because PostgreSQL searches the session's
    // temporary schema first for an unqualified table name. The fixture rows
    // are written and read as pg_temp.users, so only the statement's own bare
    // name relies on that lookup. ON COMMIT DROP and the rollback below leave
    // nothing behind.
    let mut transaction = fixture.pool.begin().await.unwrap();
    sqlx::query(
        "CREATE TEMPORARY TABLE users (avatar_url TEXT, avatar_digest TEXT) ON COMMIT DROP",
    )
    .execute(&mut *transaction)
    .await
    .unwrap();
    for (picture, _) in pictures {
        sqlx::query("INSERT INTO pg_temp.users(avatar_url) VALUES($1)")
            .bind(picture)
            .execute(&mut *transaction)
            .await
            .unwrap();
    }
    sqlx::query(backfill)
        .execute(&mut *transaction)
        .await
        .unwrap();
    let mut backfilled = Vec::with_capacity(pictures.len());
    for (picture, status) in pictures {
        let digest: String =
            sqlx::query_scalar("SELECT avatar_digest FROM pg_temp.users WHERE avatar_url=$1")
                .bind(picture)
                .fetch_one(&mut *transaction)
                .await
                .unwrap();
        assert_eq!(digest, avatar_digest(picture), "{picture:.40}");
        backfilled.push((picture, digest, status));
    }
    transaction.rollback().await.unwrap();
    // The route serves a backfilled picture by that digest, and a stored text
    // that is not a raster data URL is a 404 under its own digest.
    for (picture, digest, status) in backfilled {
        sqlx::query("UPDATE users SET avatar_url=$2, avatar_digest=$3 WHERE id=$1")
            .bind(fixture.id())
            .bind(picture)
            .bind(&digest)
            .execute(&fixture.pool)
            .await
            .unwrap();
        let response = fixture.get(&fixture.avatar(&digest), false).await;
        assert_eq!(response.status(), status, "{picture:.40}");
    }
    fixture.close().await;
}
