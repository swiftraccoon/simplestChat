//! Optional session-owned Web Push. Empty payloads keep chat contents off push
//! providers; the service worker displays a generic private-message notification.
use crate::{
    auth::{
        account::authenticated_claims,
        routes,
        sessions::lock_current_session,
        types::{AuthError, Claims},
    },
    secret::SecretString,
    signaling::SignalingServer,
};
use aws_lc_rs::signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair, KeyPair};
use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::Utc;
use futures_util::{StreamExt, stream};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use std::{sync::Arc, time::Duration};
use uuid::Uuid;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SubscriptionRequest {
    endpoint: SecretString,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PushStatus {
    public_key: String,
    enabled: bool,
}

// Restrict outbound requests to browser-operated push services. Never follow a
// redirect or use a system proxy with these capability-bearing URLs.
fn endpoint_url(endpoint: &str) -> Result<url::Url, AuthError> {
    let invalid =
        || AuthError::InvalidInput("This browser's notification service is not supported");
    if endpoint.len() > 2048 || endpoint.bytes().any(|byte| byte.is_ascii_whitespace()) {
        return Err(invalid());
    }
    let parsed = url::Url::parse(endpoint).map_err(|_| invalid())?;
    let host = parsed.host_str().ok_or_else(invalid)?;
    let approved = matches!(
        host,
        "fcm.googleapis.com" | "updates.push.services.mozilla.com" | "web.push.apple.com"
    ) || host.ends_with(".notify.windows.com");
    if !approved
        || parsed.scheme() != "https"
        || parsed.port().is_some()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.fragment().is_some()
        || parsed.path() == "/"
    {
        return Err(invalid());
    }
    Ok(parsed)
}

struct VapidKey {
    private: Vec<u8>,
    public: String,
}

impl VapidKey {
    fn from_private(private: Vec<u8>) -> anyhow::Result<Self> {
        let pair = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &private)
            .map_err(|_| anyhow::anyhow!("Invalid stored notification signing key"))?;
        Ok(Self {
            public: URL_SAFE_NO_PAD.encode(pair.public_key().as_ref()),
            private,
        })
    }

    fn authorization(&self, endpoint: &url::Url, subject: Option<&str>) -> anyhow::Result<String> {
        #[derive(Serialize)]
        struct VapidClaims<'a> {
            aud: String,
            exp: i64,
            #[serde(skip_serializing_if = "Option::is_none")]
            sub: Option<&'a str>,
        }
        let token = jsonwebtoken::encode(
            &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::ES256),
            &VapidClaims {
                aud: endpoint.origin().ascii_serialization(),
                exp: Utc::now().timestamp() + 3600,
                sub: subject,
            },
            &jsonwebtoken::EncodingKey::from_ec_der(&self.private),
        )?;
        Ok(format!("vapid t={token},k={}", self.public))
    }
}

async fn load_key(pool: &PgPool) -> anyhow::Result<VapidKey> {
    let mut stored: Option<Vec<u8>> =
        sqlx::query_scalar("SELECT private_key FROM push_keys WHERE singleton")
            .fetch_optional(pool)
            .await?;
    if stored.is_none() {
        let pair = EcdsaKeyPair::generate(&ECDSA_P256_SHA256_FIXED_SIGNING)
            .map_err(|_| anyhow::anyhow!("Notification signing key generation failed"))?;
        let document = pair
            .to_pkcs8v1()
            .map_err(|_| anyhow::anyhow!("Notification signing key serialization failed"))?;
        sqlx::query(
            "INSERT INTO push_keys(singleton,private_key) VALUES(TRUE,$1) ON CONFLICT DO NOTHING",
        )
        .bind(document.as_ref())
        .execute(pool)
        .await?;
        stored = sqlx::query_scalar("SELECT private_key FROM push_keys WHERE singleton")
            .fetch_optional(pool)
            .await?;
    }
    VapidKey::from_private(
        stored.ok_or_else(|| anyhow::anyhow!("Notification signing key unavailable"))?,
    )
}

/// The VAPID signing identity, read once at startup. The status route and the
/// delivery worker share this copy, so no request reads `push_keys`.
#[derive(Clone)]
pub(crate) struct PushSigner(Arc<VapidKey>);

impl PushSigner {
    /// Reads the stored identity, creating it on the first start. It lives in
    /// the database so backup/restore and supported VPS migrations preserve
    /// subscriptions; no third-party account is required. Fails when the key
    /// cannot be read, created or parsed.
    async fn load(pool: &PgPool) -> anyhow::Result<Self> {
        Ok(Self(Arc::new(load_key(pool).await?)))
    }

    /// The application server key browsers subscribe with: the uncompressed
    /// P-256 point as unpadded base64url.
    pub(crate) fn public_key(&self) -> &str {
        &self.0.public
    }
}

pub(crate) async fn status(
    State(server): State<SignalingServer>,
    headers: HeaderMap,
) -> Result<(HeaderMap, Json<PushStatus>), AuthError> {
    let _permit = routes::acquire_account_request(&server)?;
    let claims = authenticated_claims(&server, &headers).await?;
    // The key loaded at startup: answering never reads `push_keys`.
    let public_key = server
        .push_signer()
        .ok_or(AuthError::NotConfigured)?
        .public_key()
        .to_owned();
    let pool = server.db_pool().ok_or(AuthError::NotConfigured)?;
    let enabled = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM push_subscriptions WHERE session_id=$1 AND auth_version=$2)",
    )
    .bind(claims.sid)
    .bind(claims.auth_version)
    .fetch_one(pool)
    .await
    .map_err(routes::database_error)?;
    Ok((
        routes::no_store_headers(),
        Json(PushStatus {
            public_key,
            enabled,
        }),
    ))
}

async fn save_subscription(
    pool: &PgPool,
    claims: &Claims,
    endpoint: &str,
) -> Result<(), AuthError> {
    let endpoint = endpoint_url(endpoint)?.to_string();
    let hash = hex::encode(Sha256::digest(endpoint.as_bytes()));
    let mut tx = pool.begin().await.map_err(routes::database_error)?;
    let (user, _) = lock_current_session(&mut tx, claims).await?;
    // An origin-wide browser subscription may follow a new session of the same
    // account. Another account must obtain a fresh browser subscription first.
    let owner: Option<Uuid> = sqlx::query_scalar(
        "SELECT user_id FROM push_subscriptions WHERE endpoint_hash=$1 FOR UPDATE",
    )
    .bind(&hash)
    .fetch_optional(&mut *tx)
    .await
    .map_err(routes::database_error)?;
    if owner.is_some_and(|owner| owner != user) {
        return Err(AuthError::InvalidInput(
            "Reset browser notifications before enabling them for another account",
        ));
    }
    sqlx::query(
        "DELETE FROM push_subscriptions WHERE session_id=$1 OR (endpoint_hash=$2 AND user_id=$3)",
    )
    .bind(claims.sid)
    .bind(&hash)
    .bind(user)
    .execute(&mut *tx)
    .await
    .map_err(routes::database_error)?;
    sqlx::query("INSERT INTO push_subscriptions(id,session_id,user_id,auth_version,endpoint,endpoint_hash) VALUES($1,$2,$3,$4,$5,$6)")
        .bind(Uuid::new_v4()).bind(claims.sid).bind(user).bind(claims.auth_version).bind(endpoint).bind(hash)
        .execute(&mut *tx).await.map_err(routes::database_error)?;
    tx.commit().await.map_err(routes::database_error)
}

pub(crate) async fn subscribe(
    State(server): State<SignalingServer>,
    headers: HeaderMap,
    Json(body): Json<SubscriptionRequest>,
) -> Result<(HeaderMap, StatusCode), AuthError> {
    let _permit = routes::acquire_account_request(&server)?;
    let claims = authenticated_claims(&server, &headers).await?;
    save_subscription(
        server.db_pool().ok_or(AuthError::NotConfigured)?,
        &claims,
        body.endpoint.expose_secret(),
    )
    .await?;
    Ok((routes::no_store_headers(), StatusCode::NO_CONTENT))
}

async fn delete_subscription(pool: &PgPool, claims: &Claims) -> Result<(), AuthError> {
    let mut tx = pool.begin().await.map_err(routes::database_error)?;
    let (user, _) = lock_current_session(&mut tx, claims).await?;
    sqlx::query("DELETE FROM push_subscriptions WHERE session_id=$1 AND user_id=$2")
        .bind(claims.sid)
        .bind(user)
        .execute(&mut *tx)
        .await
        .map_err(routes::database_error)?;
    tx.commit().await.map_err(routes::database_error)
}

pub(crate) async fn unsubscribe(
    State(server): State<SignalingServer>,
    headers: HeaderMap,
) -> Result<(HeaderMap, StatusCode), AuthError> {
    let _permit = routes::acquire_account_request(&server)?;
    let claims = authenticated_claims(&server, &headers).await?;
    delete_subscription(server.db_pool().ok_or(AuthError::NotConfigured)?, &claims).await?;
    Ok((routes::no_store_headers(), StatusCode::NO_CONTENT))
}

/// Coalesce durable new PMs. Delivery never affects message acknowledgement.
pub(crate) async fn enqueue(pool: &PgPool, recipient: Uuid) {
    if sqlx::query(
        "UPDATE push_subscriptions SET generation=generation+1,
         attempts=CASE WHEN pending_since IS NULL THEN 0 ELSE attempts END,
         next_attempt_at=CASE WHEN pending_since IS NULL THEN GREATEST(next_attempt_at,now()+INTERVAL '3 seconds') ELSE next_attempt_at END,
         pending_since=COALESCE(pending_since,now()) WHERE user_id=$1",
    ).bind(recipient).execute(pool).await.is_err() {
        tracing::warn!("Notification queue update failed");
    }
}

#[derive(sqlx::FromRow)]
struct Pending {
    id: Uuid,
    generation: i64,
    attempts: i64,
    endpoint: String,
}

// Claim only as many rows as can begin delivery immediately. The lease covers
// the HTTP deadline plus both bounded database operations, including pool waits.
async fn claim_pending(pool: &PgPool) -> Result<Vec<Pending>, sqlx::Error> {
    sqlx::query_as(
        "WITH due AS (SELECT id FROM push_subscriptions
         WHERE pending_since IS NOT NULL AND next_attempt_at<=now()
         ORDER BY next_attempt_at LIMIT 4 FOR UPDATE SKIP LOCKED)
         UPDATE push_subscriptions p SET next_attempt_at=now()+INTERVAL '60 seconds', attempts=LEAST(5,attempts+1)
         FROM due WHERE p.id=due.id RETURNING p.id,p.generation,p.attempts,p.endpoint",
    ).fetch_all(pool).await
}

async fn still_unread(pool: &PgPool, pending: &Pending) -> Result<bool, sqlx::Error> {
    still_unread_at(pool, pending, Utc::now()).await
}

// Binding one instant keeps quiet-hour, snooze and unread checks consistent,
// including both occurrences of a repeated daylight-saving wall-clock minute.
async fn still_unread_at(
    pool: &PgPool,
    pending: &Pending,
    now: chrono::DateTime<Utc>,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM push_subscriptions p
         JOIN sessions s ON s.id=p.session_id AND s.user_id=p.user_id AND s.expires_at>clock_timestamp()
         JOIN users u ON u.id=p.user_id AND u.auth_version=p.auth_version
         LEFT JOIN account_notification_preferences n ON n.user_id=p.user_id
         CROSS JOIN LATERAL (SELECT (extract(hour FROM ($2::timestamptz AT TIME ZONE COALESCE(n.quiet_timezone,'UTC')))*60
             + extract(minute FROM ($2::timestamptz AT TIME ZONE COALESCE(n.quiet_timezone,'UTC'))))::integer AS minute) local_time
         WHERE p.id=$1 AND p.pending_since>now()-INTERVAL '1 hour'
         AND COALESCE(u.preferences->'allowPrivateMessages','true'::jsonb)<>'false'::jsonb
         AND COALESCE(n.private_messages,TRUE)
         AND (n.quiet_start IS NULL OR NOT CASE WHEN n.quiet_start<n.quiet_end
             THEN local_time.minute>=n.quiet_start AND local_time.minute<n.quiet_end
             ELSE local_time.minute>=n.quiet_start OR local_time.minute<n.quiet_end END)
         AND EXISTS(SELECT 1 FROM chat_messages m LEFT JOIN chat_read_cursors r
           ON r.user_id=p.user_id AND r.conversation=m.conversation
           LEFT JOIN conversation_notification_preferences c ON c.user_id=p.user_id AND c.peer_id=m.sender_account
           WHERE m.recipient_account=p.user_id AND m.expires_at>now()
           AND NOT EXISTS(SELECT 1 FROM jsonb_array_elements(CASE WHEN jsonb_typeof(u.preferences->'ignored')='array'
             THEN u.preferences->'ignored' ELSE '[]'::jsonb END) ignored WHERE ignored->>'id'=m.sender_account::text)
           AND NOT COALESCE(c.muted,FALSE) AND (c.snoozed_until IS NULL OR c.snoozed_until<=$2)
           AND m.body->>'removedAt' IS NULL
           AND (r.message_id IS NULL OR r.expires_at<=now() OR (m.sent_at,m.id)>(r.sent_at,r.message_id))))",
    ).bind(pending.id).bind(now).fetch_one(pool).await
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Delivery {
    Accepted,
    Expired,
    Retry,
    Rejected,
}

fn delivery_status(code: u16) -> Delivery {
    match code {
        200..=299 => Delivery::Accepted,
        404 | 410 => Delivery::Expired,
        408 | 429 | 500..=599 => Delivery::Retry,
        _ => Delivery::Rejected,
    }
}

async fn finish_delivery(
    pool: &PgPool,
    pending: &Pending,
    outcome: Delivery,
) -> Result<(), sqlx::Error> {
    if outcome == Delivery::Expired {
        sqlx::query("DELETE FROM push_subscriptions WHERE id=$1")
            .bind(pending.id)
            .execute(pool)
            .await?;
    } else if outcome == Delivery::Retry && pending.attempts < 5 {
        let delay = 30_i64 * (1_i64 << pending.attempts.min(5));
        sqlx::query("UPDATE push_subscriptions SET next_attempt_at=now()+make_interval(secs=>$2) WHERE id=$1")
            .bind(pending.id).bind(delay as f64).execute(pool).await?;
    } else {
        // A message arriving during the request owns the newer generation.
        sqlx::query("UPDATE push_subscriptions SET pending_since=CASE WHEN generation=$2 THEN NULL ELSE pending_since END,
                     attempts=0,next_attempt_at=now()+INTERVAL '15 seconds' WHERE id=$1")
            .bind(pending.id).bind(pending.generation).execute(pool).await?;
    }
    Ok(())
}

async fn deliver(
    pool: &PgPool,
    client: &reqwest::Client,
    key: &VapidKey,
    subject: Option<&str>,
    pending: Pending,
) {
    let outcome = match still_unread(pool, &pending).await {
        Ok(false) => Delivery::Accepted,
        Err(_) => Delivery::Retry,
        Ok(true) => match endpoint_url(&pending.endpoint) {
            Err(_) => Delivery::Expired,
            Ok(endpoint) => match key.authorization(&endpoint, subject) {
                Err(_) => Delivery::Rejected,
                Ok(authorization) => match client
                    .post(endpoint)
                    .header("Authorization", authorization)
                    .header("TTL", "300")
                    .header("Urgency", "normal")
                    .header("Topic", "inbox")
                    .body(Vec::<u8>::new())
                    .send()
                    .await
                {
                    Ok(response) => delivery_status(response.status().as_u16()),
                    Err(_) => Delivery::Retry,
                },
            },
        },
    };
    if outcome == Delivery::Rejected {
        tracing::warn!("Notification provider rejected delivery");
    }
    if finish_delivery(pool, &pending, outcome).await.is_err() {
        tracing::warn!("Notification queue completion failed");
    }
}

/// Starts the delivery worker, which signs every request with `signer`, the
/// identity the status route hands to browsers. Fails when the worker's HTTP
/// client cannot be built. It is synchronous, so it must be called inside a
/// Tokio runtime: `tokio::spawn` panics outside one.
fn spawn(
    pool: PgPool,
    signer: PushSigner,
    origin: Option<String>,
) -> anyhow::Result<tokio::task::JoinHandle<()>> {
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(10))
        .build()?;
    Ok(tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(3));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tick.tick().await;
            match claim_pending(&pool).await {
                Ok(pending) => {
                    stream::iter(pending)
                        .for_each_concurrent(4, |pending| {
                            deliver(&pool, &client, &signer.0, origin.as_deref(), pending)
                        })
                        .await
                }
                Err(_) => tracing::warn!("Notification queue unavailable"),
            }
        }
    }))
}

/// Starts Web Push for a server with a database, as `run_server` does whenever
/// one is configured: reads the signing key once, creating it on the first
/// start, starts the delivery worker with it and returns the server with the
/// key attached, so the status route and the worker use one key. Fails when
/// the key cannot be read, created or parsed or the worker's HTTP client
/// cannot be built; like `spawn`, it must run inside a Tokio runtime.
pub(crate) async fn start(
    pool: &PgPool,
    server: SignalingServer,
) -> anyhow::Result<(SignalingServer, tokio::task::JoinHandle<()>)> {
    let signer = PushSigner::load(pool).await?;
    let worker = spawn(pool.clone(), signer.clone(), server.notification_origin())?;
    Ok((server.with_push_signer(signer), worker))
}

#[cfg(test)]
#[path = "push_tests.rs"]
mod tests;
