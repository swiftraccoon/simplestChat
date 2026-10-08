use super::*;
use aws_lc_rs::signature::{ECDSA_P256_SHA256_FIXED, UnparsedPublicKey};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use std::str::FromStr;

#[test]
fn notification_endpoints_require_known_https_services_without_credentials_or_fragments() {
    for endpoint in [
        "https://fcm.googleapis.com/fcm/send/opaque-token",
        "https://updates.push.services.mozilla.com/wpush/v2/opaque-token",
        "https://web.push.apple.com/opaque-token",
        "https://wns2-sg2p.notify.windows.com/w/?token=opaque",
    ] {
        assert!(endpoint_url(endpoint).is_ok(), "{endpoint}");
    }
    for endpoint in [
        "http://fcm.googleapis.com/fcm/send/test",
        "https://127.0.0.1/push",
        "https://[::1]/push",
        "https://localhost/push",
        "https://fcm.googleapis.com.evil.invalid/push",
        "https://notify.windows.com.evil.invalid/push",
        "https://evilnotify.windows.com/push",
        "https://notify.windows.com/push",
        "https://user:secret@fcm.googleapis.com/push",
        "https://fcm.googleapis.com:8443/push",
        "https://fcm.googleapis.com/push#secret",
        "https://fcm.googleapis.com/",
        "https://fcm.googleapis.com/push\n",
        "file:///private/push",
    ] {
        assert!(endpoint_url(endpoint).is_err(), "{endpoint}");
    }
    assert!(endpoint_url(&format!("https://fcm.googleapis.com/{}", "x".repeat(2048))).is_err());
}

#[test]
fn vapid_signatures_bind_the_push_origin_expiry_and_stable_public_key() {
    let pair = EcdsaKeyPair::generate(&ECDSA_P256_SHA256_FIXED_SIGNING).unwrap();
    let document = pair.to_pkcs8v1().unwrap();
    let key = VapidKey::from_private(document.as_ref().to_vec()).unwrap();
    let before = Utc::now().timestamp();
    let endpoint =
        endpoint_url("https://fcm.googleapis.com/fcm/send/private-endpoint-token").unwrap();
    let authorization = key
        .authorization(&endpoint, Some("https://the.research.clinic"))
        .unwrap();
    let after = Utc::now().timestamp();
    let (token, public) = authorization
        .strip_prefix("vapid t=")
        .unwrap()
        .split_once(",k=")
        .unwrap();
    assert_eq!(public, key.public);
    let public = URL_SAFE_NO_PAD.decode(public).unwrap();
    assert_eq!(public.len(), 65);
    assert_eq!(public[0], 4, "VAPID uses an uncompressed P-256 point");
    let pieces: Vec<_> = token.split('.').collect();
    assert_eq!(pieces.len(), 3);
    let header: serde_json::Value =
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(pieces[0]).unwrap()).unwrap();
    let claims: serde_json::Value =
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(pieces[1]).unwrap()).unwrap();
    assert_eq!(header["alg"], "ES256");
    assert_eq!(claims["aud"], "https://fcm.googleapis.com");
    assert_eq!(claims["sub"], "https://the.research.clinic");
    let expiry = claims["exp"].as_i64().unwrap();
    assert!((before + 3600..=after + 3600).contains(&expiry));
    assert!(!claims.to_string().contains("private-endpoint-token"));
    let signature = URL_SAFE_NO_PAD.decode(pieces[2]).unwrap();
    UnparsedPublicKey::new(&ECDSA_P256_SHA256_FIXED, public)
        .verify(
            format!("{}.{}", pieces[0], pieces[1]).as_bytes(),
            &signature,
        )
        .unwrap();
    assert!(VapidKey::from_private(vec![0; 80]).is_err());
    let no_subject = key.authorization(&endpoint, None).unwrap();
    let payload = no_subject
        .strip_prefix("vapid t=")
        .unwrap()
        .split('.')
        .nth(1)
        .unwrap();
    let claims: serde_json::Value =
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload).unwrap()).unwrap();
    assert!(claims.get("sub").is_none());
}

#[test]
fn notification_provider_results_separate_expiry_retry_and_permanent_rejection() {
    for code in [200, 201, 202, 204] {
        assert_eq!(delivery_status(code), Delivery::Accepted);
    }
    for code in [404, 410] {
        assert_eq!(delivery_status(code), Delivery::Expired);
    }
    for code in [408, 429, 500, 503] {
        assert_eq!(delivery_status(code), Delivery::Retry);
    }
    for code in [301, 307, 400, 401, 403] {
        assert_eq!(delivery_status(code), Delivery::Rejected);
    }
}

/// The disposable integration server may have its real queue worker running.
/// A unique schema keeps every test endpoint and pending row invisible to it;
/// these tests invoke queue state helpers and never the network delivery path.
struct IsolatedPushDatabase {
    admin: PgPool,
    pool: PgPool,
    schema: String,
}
impl IsolatedPushDatabase {
    async fn new() -> Self {
        let url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL");
        let admin = PgPool::connect(&url).await.unwrap();
        let schema = format!("push_test_{}", Uuid::new_v4().simple());
        let sql = format!("CREATE SCHEMA {schema}");
        sqlx::query(sqlx::AssertSqlSafe(sql))
            .execute(&admin)
            .await
            .unwrap();
        for table in [
            "users",
            "sessions",
            "chat_messages",
            "chat_read_cursors",
            "push_keys",
            "push_subscriptions",
        ] {
            let sql = format!("CREATE TABLE {schema}.{table} (LIKE public.{table} INCLUDING ALL)");
            sqlx::query(sqlx::AssertSqlSafe(sql))
                .execute(&admin)
                .await
                .unwrap();
        }
        let options = PgConnectOptions::from_str(&url)
            .unwrap()
            .options([("search_path", schema.as_str())]);
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::query("ALTER TABLE push_subscriptions ADD FOREIGN KEY(session_id) REFERENCES sessions(id) ON DELETE CASCADE")
            .execute(&pool).await.unwrap();
        Self {
            admin,
            pool,
            schema,
        }
    }
    async fn close(self) {
        self.pool.close().await;
        let sql = format!("DROP SCHEMA {} CASCADE", self.schema);
        sqlx::query(sqlx::AssertSqlSafe(sql))
            .execute(&self.admin)
            .await
            .unwrap();
        self.admin.close().await;
    }
    async fn user(&self) -> Uuid {
        sqlx::query_scalar(
            "INSERT INTO users(email,display_name) VALUES($1,'Push tester') RETURNING id",
        )
        .bind(format!("{}@push.invalid", Uuid::new_v4()))
        .fetch_one(&self.pool)
        .await
        .unwrap()
    }
    async fn session(&self, user: Uuid) -> Claims {
        let refresh = crate::auth::session::generate_refresh_token().unwrap();
        let sid = crate::auth::session::create_session(&self.pool, &user, &refresh)
            .await
            .unwrap();
        Claims {
            sub: user.to_string(),
            name: "Push tester".into(),
            iss: "simplestChat".into(),
            aud: "simplestChat".into(),
            exp: (Utc::now().timestamp() + 3600) as usize,
            auth_version: 0,
            sid,
        }
    }
    async fn incoming(&self, user: Uuid) -> (Uuid, String) {
        let id = Uuid::new_v4();
        let sender = Uuid::new_v4();
        let conversation = format!("pm:{sender}:{user}");
        sqlx::query("INSERT INTO chat_messages(id,conversation,sender_account,recipient_account,sender_session,client_message_id,sent_at,expires_at,body)
            VALUES($1,$2,$3,$4,$3,$1::text,now(),now()+interval '90 days','{}'::jsonb)")
            .bind(id).bind(&conversation).bind(sender).bind(user).execute(&self.pool).await.unwrap();
        (id, conversation)
    }
    async fn due(&self) {
        sqlx::query("UPDATE push_subscriptions SET next_attempt_at=now()-interval '1 second'")
            .execute(&self.pool)
            .await
            .unwrap();
    }
}

#[tokio::test]
#[ignore = "requires a migrated disposable TEST_DATABASE_URL"]
async fn database_notifications_keep_key_identity_and_require_current_owned_sessions() {
    let db = IsolatedPushDatabase::new().await;
    let first_key = load_key(&db.pool).await.unwrap();
    let reloaded_key = load_key(&db.pool).await.unwrap();
    assert_eq!(first_key.public, reloaded_key.public);
    assert_eq!(first_key.private, reloaded_key.private);
    let user = db.user().await;
    let foreign = db.user().await;
    let first = db.session(user).await;
    let second = db.session(user).await;
    let other = db.session(foreign).await;
    let endpoint = "https://fcm.googleapis.com/fcm/send/isolated-subscription";
    save_subscription(&db.pool, &first, endpoint).await.unwrap();
    assert!(save_subscription(&db.pool, &other, endpoint).await.is_err());
    save_subscription(&db.pool, &second, endpoint)
        .await
        .unwrap();
    let active: Uuid = sqlx::query_scalar("SELECT session_id FROM push_subscriptions")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(active, second.sid);
    delete_subscription(&db.pool, &first).await.unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM push_subscriptions")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(
        count, 1,
        "an older session cannot unsubscribe its replacement"
    );
    sqlx::query("DELETE FROM sessions WHERE id=$1")
        .bind(second.sid)
        .execute(&db.pool)
        .await
        .unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM push_subscriptions")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(
        count, 0,
        "session revocation cascades to browser subscriptions"
    );
    assert!(matches!(
        save_subscription(&db.pool, &second, endpoint).await,
        Err(AuthError::InvalidToken)
    ));
    sqlx::query("UPDATE users SET auth_version=auth_version+1 WHERE id=$1")
        .bind(user)
        .execute(&db.pool)
        .await
        .unwrap();
    assert!(matches!(
        save_subscription(&db.pool, &first, endpoint).await,
        Err(AuthError::InvalidToken)
    ));
    db.close().await;
}

#[tokio::test]
#[ignore = "requires a migrated disposable TEST_DATABASE_URL"]
async fn database_notifications_coalesce_pending_generations_and_suppress_read_revoked_or_expired()
{
    let db = IsolatedPushDatabase::new().await;
    let user = db.user().await;
    let claims = db.session(user).await;
    save_subscription(
        &db.pool,
        &claims,
        "https://updates.push.services.mozilla.com/wpush/v2/isolated-queue",
    )
    .await
    .unwrap();
    let (message, conversation) = db.incoming(user).await;
    enqueue(&db.pool, user).await;
    enqueue(&db.pool, user).await;
    assert!(
        claim_pending(&db.pool).await.unwrap().is_empty(),
        "coalescing delay is respected"
    );
    db.due().await;
    let pending = claim_pending(&db.pool).await.unwrap().pop().unwrap();
    assert_eq!(pending.generation, 2);
    assert_eq!(pending.attempts, 1);
    assert!(
        claim_pending(&db.pool).await.unwrap().is_empty(),
        "claimed entries cannot be claimed twice immediately"
    );
    assert!(still_unread(&db.pool, &pending).await.unwrap());
    enqueue(&db.pool, user).await;
    finish_delivery(&db.pool, &pending, Delivery::Accepted)
        .await
        .unwrap();
    let has_pending: bool =
        sqlx::query_scalar("SELECT pending_since IS NOT NULL FROM push_subscriptions")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert!(
        has_pending,
        "a message arriving during delivery owns a newer generation"
    );
    db.due().await;
    let pending = claim_pending(&db.pool).await.unwrap().pop().unwrap();
    assert_eq!(pending.generation, 3);
    sqlx::query(
        "INSERT INTO chat_read_cursors(user_id,conversation,message_id,sent_at,expires_at)
        SELECT $1,$2,id,sent_at,expires_at FROM chat_messages WHERE id=$3",
    )
    .bind(user)
    .bind(&conversation)
    .bind(message)
    .execute(&db.pool)
    .await
    .unwrap();
    assert!(
        !still_unread(&db.pool, &pending).await.unwrap(),
        "reading before delivery suppresses the notification"
    );
    sqlx::query("UPDATE chat_read_cursors SET expires_at=now()-interval '1 second'")
        .execute(&db.pool)
        .await
        .unwrap();
    assert!(
        still_unread(&db.pool, &pending).await.unwrap(),
        "expired markers cannot suppress new retained unread data"
    );
    sqlx::query("UPDATE chat_messages SET body=jsonb_build_object('removedAt',now()::text)")
        .execute(&db.pool)
        .await
        .unwrap();
    assert!(!still_unread(&db.pool, &pending).await.unwrap());
    sqlx::query("UPDATE chat_messages SET body='{}'::jsonb")
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE push_subscriptions SET pending_since=now()-interval '2 hours'")
        .execute(&db.pool)
        .await
        .unwrap();
    assert!(
        !still_unread(&db.pool, &pending).await.unwrap(),
        "stale notification work expires"
    );
    sqlx::query("UPDATE push_subscriptions SET pending_since=now()")
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE users SET auth_version=auth_version+1 WHERE id=$1")
        .bind(user)
        .execute(&db.pool)
        .await
        .unwrap();
    assert!(
        !still_unread(&db.pool, &pending).await.unwrap(),
        "credential revocation suppresses pending work"
    );
    finish_delivery(&db.pool, &pending, Delivery::Accepted)
        .await
        .unwrap();
    let has_pending: bool =
        sqlx::query_scalar("SELECT pending_since IS NOT NULL FROM push_subscriptions")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert!(!has_pending);
    db.close().await;
}

#[tokio::test]
#[ignore = "requires a migrated disposable TEST_DATABASE_URL"]
async fn database_notifications_bound_retries_and_remove_expired_endpoints() {
    let db = IsolatedPushDatabase::new().await;
    let user = db.user().await;
    let claims = db.session(user).await;
    save_subscription(
        &db.pool,
        &claims,
        "https://web.push.apple.com/isolated-retries",
    )
    .await
    .unwrap();
    enqueue(&db.pool, user).await;
    for attempt in 1..=5 {
        db.due().await;
        let pending = claim_pending(&db.pool).await.unwrap().pop().unwrap();
        assert_eq!(pending.attempts, attempt);
        finish_delivery(&db.pool, &pending, Delivery::Retry)
            .await
            .unwrap();
        let (has_pending, delay): (bool, f64) = sqlx::query_as("SELECT pending_since IS NOT NULL,extract(epoch FROM next_attempt_at-now())::double precision FROM push_subscriptions")
            .fetch_one(&db.pool).await.unwrap();
        assert_eq!(has_pending, attempt < 5);
        assert!(delay > 0.0);
        if attempt < 5 {
            assert!(delay > (30 * (1 << attempt)) as f64 - 5.0);
        }
    }
    enqueue(&db.pool, user).await;
    db.due().await;
    let pending = claim_pending(&db.pool).await.unwrap().pop().unwrap();
    finish_delivery(&db.pool, &pending, Delivery::Expired)
        .await
        .unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM push_subscriptions")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
    db.close().await;
}
