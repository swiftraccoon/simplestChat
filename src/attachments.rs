//! Bounded account uploads. Bytes stay in the existing database/backup boundary;
//! access follows the live or durable message, never knowledge of a file UUID.
use crate::{
    auth::{
        account::authenticated_claims,
        routes,
        sessions::lock_current_session,
        types::{AuthError, Claims},
    },
    signaling::{SignalingServer, protocol::ChatAttachment},
};
use axum::{
    Json,
    body::{Body, Bytes, to_bytes},
    extract::{Path, Request, State},
    http::{HeaderMap, HeaderValue, StatusCode},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use rand::{TryRng, rngs::SysRng};
use serde::{Deserialize, Serialize};
use sqlx::{PgConnection, PgPool};
use std::sync::{Arc, LazyLock};
use tokio::sync::{Semaphore, mpsc};
use uuid::Uuid;

pub const MAX_FILE_BYTES: usize = 5 * 1024 * 1024;
pub const MAX_MESSAGE_FILES: usize = 4;
const ACCOUNT_BYTES: i64 = 64 * 1024 * 1024;
const TOTAL_BYTES: i64 = 1024 * 1024 * 1024;
const ACCOUNT_FILES: i64 = 1000;
const TOTAL_FILES: i64 = 20000;
static UPLOADS: Semaphore = Semaphore::const_new(4);
static DOWNLOADS: LazyLock<Arc<Semaphore>> = LazyLock::new(|| Arc::new(Semaphore::new(4)));
static GRANT_KEY: LazyLock<[u8; 32]> = LazyLock::new(|| {
    let mut key = [0; 32];
    SysRng
        .try_fill_bytes(&mut key)
        .expect("Attachment grant randomness unavailable");
    key
});

fn unavailable() -> AuthError {
    AuthError::InvalidInput("Attachment unavailable")
}
fn invalid() -> sqlx::Error {
    sqlx::Error::InvalidArgument("Attachment unavailable".into())
}

pub(crate) fn valid_ids(ids: &[Uuid]) -> bool {
    ids.len() <= MAX_MESSAGE_FILES && ids.iter().enumerate().all(|(i, id)| !ids[..i].contains(id))
}

fn filename(encoded: &str) -> Result<String, AuthError> {
    if encoded.len() > 344 {
        return Err(AuthError::InvalidInput("File name is too long"));
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| AuthError::InvalidInput("Invalid file name"))?;
    if bytes.len() > 255 {
        return Err(AuthError::InvalidInput("File name is too long"));
    }
    let name =
        std::str::from_utf8(&bytes).map_err(|_| AuthError::InvalidInput("Invalid file name"))?;
    let name = name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("")
        .chars()
        .filter(|c| c.is_alphanumeric() || matches!(c, ' ' | '.' | '-' | '_'))
        .take(120)
        .collect::<String>();
    let name = name.trim_matches([' ', '.']);
    Ok(if name.is_empty() {
        "file".into()
    } else {
        name.into()
    })
}

fn content_type(bytes: &[u8]) -> &'static str {
    for mime in ["image/png", "image/jpeg", "image/webp"] {
        if mime == "image/png" && bytes.windows(4).any(|chunk| chunk == b"acTL") {
            continue;
        }
        if crate::auth::account::image_dimensions(mime, bytes).is_some_and(|(width, height)| {
            width > 0
                && height > 0
                && width <= 4096
                && height <= 4096
                && u64::from(width) * u64::from(height) <= 16_000_000
        }) {
            return mime;
        }
    }
    "application/octet-stream"
}

async fn store_upload(
    pool: &PgPool,
    claims: &Claims,
    name: String,
    data: Vec<u8>,
) -> Result<ChatAttachment, AuthError> {
    if data.is_empty() || data.len() > MAX_FILE_BYTES {
        return Err(AuthError::InvalidInput(
            "Files must contain 1 byte to 5 MiB",
        ));
    }
    let mut tx = pool.begin().await.map_err(routes::database_error)?;
    let (owner, _) = lock_current_session(&mut tx, claims).await?;
    // A single bounded quota reservation serializes uploads across accounts.
    // Expired bytes count until physically deleted, keeping the disk cap honest.
    sqlx::query("SELECT pg_advisory_xact_lock(7219647820751042)")
        .execute(&mut *tx)
        .await
        .map_err(routes::database_error)?;
    let (total,owned,count,owned_count):(i64,i64,i64,i64)=sqlx::query_as(
        "SELECT COALESCE(sum(size),0)::bigint,COALESCE(sum(size) FILTER(WHERE owner_id=$1),0)::bigint,
         count(*),count(*) FILTER(WHERE owner_id=$1) FROM attachments")
        .bind(owner).fetch_one(&mut *tx).await.map_err(routes::database_error)?;
    let size = data.len() as i64;
    if owned + size > ACCOUNT_BYTES || owned_count >= ACCOUNT_FILES {
        return Err(AuthError::InvalidInput(
            "Your attachment storage is full (64 MiB or 1,000 files)",
        ));
    }
    if total + size > TOTAL_BYTES || count >= TOTAL_FILES {
        return Err(AuthError::InvalidInput(
            "Server attachment storage is full; try again later",
        ));
    }
    let metadata = ChatAttachment {
        id: Uuid::new_v4(),
        name,
        content_type: content_type(&data).into(),
        size,
    };
    sqlx::query("INSERT INTO attachments(id,owner_id,name,content_type,size,data) VALUES($1,$2,$3,$4,$5,$6)")
        .bind(metadata.id).bind(owner).bind(&metadata.name).bind(&metadata.content_type).bind(size).bind(data)
        .execute(&mut *tx).await.map_err(routes::database_error)?;
    tx.commit().await.map_err(routes::database_error)?;
    Ok(metadata)
}

pub async fn upload(
    State(server): State<SignalingServer>,
    request: Request,
) -> Result<(HeaderMap, Json<ChatAttachment>), AuthError> {
    let _upload = UPLOADS.try_acquire().map_err(|_| AuthError::ServiceBusy)?;
    let _permit = routes::acquire_auth_request(&server)?;
    let claims = authenticated_claims(&server, request.headers()).await?;
    let name = filename(
        request
            .headers()
            .get("x-file-name")
            .and_then(|value| value.to_str().ok())
            .ok_or(AuthError::InvalidInput("File name is required"))?,
    )?;
    let data = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        to_bytes(request.into_body(), MAX_FILE_BYTES),
    )
    .await
    .map_err(|_| AuthError::InvalidInput("Upload timed out"))?
    .map_err(|_| AuthError::InvalidInput("File exceeds the 5 MiB limit"))?;
    let metadata = store_upload(
        server.db_pool().ok_or(AuthError::NotConfigured)?,
        &claims,
        name,
        data.to_vec(),
    )
    .await?;
    Ok((routes::no_store_headers(), Json(metadata)))
}

pub async fn delete_pending(
    State(server): State<SignalingServer>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<(HeaderMap, StatusCode), AuthError> {
    let _permit = routes::acquire_auth_request(&server)?;
    let claims = authenticated_claims(&server, &headers).await?;
    let mut tx = server
        .db_pool()
        .ok_or(AuthError::NotConfigured)?
        .begin()
        .await
        .map_err(routes::database_error)?;
    let (owner, _) = lock_current_session(&mut tx, &claims).await?;
    sqlx::query("DELETE FROM attachments WHERE id=$1 AND owner_id=$2 AND message_id IS NULL AND ephemeral_message_id IS NULL")
        .bind(id).bind(owner).execute(&mut *tx).await.map_err(routes::database_error)?;
    tx.commit().await.map_err(routes::database_error)?;
    Ok((routes::no_store_headers(), StatusCode::NO_CONTENT))
}

pub(crate) async fn pending_metadata(
    connection: &mut PgConnection,
    owner: Uuid,
    ids: &[Uuid],
) -> Result<Vec<ChatAttachment>, sqlx::Error> {
    if !valid_ids(ids) {
        return Err(invalid());
    }
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let metadata:Vec<ChatAttachment>=sqlx::query_as("SELECT id,name,content_type,size FROM attachments WHERE id=ANY($1) AND owner_id=$2
        AND message_id IS NULL AND ephemeral_message_id IS NULL AND expires_at>clock_timestamp() ORDER BY id FOR UPDATE")
        .bind(ids).bind(owner).fetch_all(&mut *connection).await?;
    if metadata.len() != ids.len() {
        return Err(invalid());
    }
    Ok(ids
        .iter()
        .filter_map(|id| metadata.iter().find(|file| file.id == *id).cloned())
        .collect())
}

pub(crate) async fn bind_durable(
    connection: &mut PgConnection,
    ids: &[Uuid],
    message: Uuid,
    room: Option<&str>,
    expires: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    if ids.is_empty() {
        return Ok(());
    }
    let changed=sqlx::query("UPDATE attachments SET message_id=$2,room_id=$3,expires_at=$4 WHERE id=ANY($1) AND message_id IS NULL AND ephemeral_message_id IS NULL")
        .bind(ids).bind(message).bind(room).bind(expires).execute(&mut *connection).await?.rows_affected();
    if changed != ids.len() as u64 {
        return Err(invalid());
    }
    Ok(())
}

pub(crate) async fn bind_ephemeral(
    pool: &PgPool,
    owner: Uuid,
    ids: &[Uuid],
    message: Uuid,
    room: &str,
) -> Result<Vec<ChatAttachment>, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let metadata = pending_metadata(&mut tx, owner, ids).await?;
    sqlx::query("UPDATE attachments SET ephemeral_message_id=$2,room_id=$3,expires_at=now()+interval '24 hours' WHERE id=ANY($1)")
        .bind(ids).bind(message).bind(room).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(metadata)
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Grant {
    iss: String,
    aud: String,
    exp: usize,
    attachment_id: Uuid,
    room_id: String,
    participant_id: String,
    media_session: Uuid,
    account: Option<Claims>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentAccess {
    pub token: String,
    pub expires_at: DateTime<Utc>,
}

#[derive(sqlx::FromRow)]
pub(crate) struct AttachmentLocation {
    pub(crate) message_id: Option<Uuid>,
    pub(crate) ephemeral_message_id: Option<Uuid>,
    pub(crate) room_id: Option<String>,
}

async fn location(pool: &PgPool, id: Uuid) -> Result<AttachmentLocation, AuthError> {
    sqlx::query_as("SELECT a.message_id,a.ephemeral_message_id,a.room_id FROM attachments a LEFT JOIN chat_messages m ON m.id=a.message_id
        WHERE a.id=$1 AND a.expires_at>clock_timestamp() AND (a.message_id IS NULL OR (m.expires_at>clock_timestamp() AND m.body->>'removedAt' IS NULL))")
        .bind(id).fetch_optional(pool).await.map_err(routes::database_error)?.ok_or_else(unavailable)
}

pub async fn grant_access(
    manager: &crate::room::RoomManager,
    pool: &PgPool,
    room_id: &str,
    participant_id: &str,
    expected_sender: &mpsc::Sender<crate::OutboundJson>,
    claims: Option<&Claims>,
    attachment_id: Uuid,
) -> Result<AttachmentAccess, AuthError> {
    let location = location(pool, attachment_id).await?;
    let (session, authenticated) = manager
        .attachment_grant_context(
            room_id,
            participant_id,
            Some(expected_sender),
            None,
            attachment_id,
            &location,
        )
        .await
        .ok_or_else(unavailable)?;
    if authenticated != claims.is_some()
        || claims.is_some_and(|claims| claims.sub != participant_id)
    {
        return Err(unavailable());
    }
    if let Some(claims) = claims {
        validate_account(pool, claims).await?;
    }
    let expires_at = Utc::now() + chrono::Duration::seconds(30);
    let grant = Grant {
        iss: "simplestChat-attachment".into(),
        aud: "attachment".into(),
        exp: expires_at.timestamp() as usize,
        attachment_id,
        room_id: room_id.into(),
        participant_id: participant_id.into(),
        media_session: session,
        account: claims.cloned(),
    };
    let token = jsonwebtoken::encode(
        &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::HS256),
        &grant,
        &jsonwebtoken::EncodingKey::from_secret(&*GRANT_KEY),
    )
    .map_err(|_| unavailable())?;
    Ok(AttachmentAccess { token, expires_at })
}

async fn validate_account(pool: &PgPool, claims: &Claims) -> Result<(), AuthError> {
    let owner = claims
        .sub
        .parse::<Uuid>()
        .map_err(|_| AuthError::InvalidToken)?;
    let current: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM users u JOIN sessions s ON s.user_id=u.id
        WHERE u.id=$1 AND u.auth_version=$2 AND s.id=$3 AND s.expires_at>clock_timestamp())",
    )
    .bind(owner)
    .bind(claims.auth_version)
    .bind(claims.sid)
    .fetch_one(pool)
    .await
    .map_err(routes::database_error)?;
    if !current || claims.exp as i64 <= Utc::now().timestamp() {
        return Err(AuthError::InvalidToken);
    }
    Ok(())
}

async fn allowed(server: &SignalingServer, headers: &HeaderMap, id: Uuid) -> Result<(), AuthError> {
    let pool = server.db_pool().ok_or(AuthError::NotConfigured)?;
    if let Some(token) = headers
        .get("authorization")
        .and_then(|header| header.to_str().ok())
        .and_then(|header| header.strip_prefix("Attachment "))
    {
        if token.len() > 4096 {
            return Err(unavailable());
        }
        let mut validation = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::HS256);
        validation.set_issuer(&["simplestChat-attachment"]);
        validation.set_audience(&["attachment"]);
        validation.leeway = 0;
        let grant = jsonwebtoken::decode::<Grant>(
            token,
            &jsonwebtoken::DecodingKey::from_secret(&*GRANT_KEY),
            &validation,
        )
        .map_err(|_| unavailable())?
        .claims;
        if grant.attachment_id != id {
            return Err(unavailable());
        }
        if let Some(claims) = &grant.account {
            validate_account(pool, claims).await?;
        }
        let location = location(pool, id).await?;
        server
            .room_manager()
            .attachment_grant_context(
                &grant.room_id,
                &grant.participant_id,
                None,
                Some(grant.media_session),
                id,
                &location,
            )
            .await
            .ok_or_else(unavailable)?;
        return Ok(());
    }
    let claims = authenticated_claims(server, headers).await?;
    let user = claims
        .sub
        .parse::<Uuid>()
        .map_err(|_| AuthError::InvalidToken)?;
    let accessible:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM attachments a LEFT JOIN chat_messages m ON m.id=a.message_id
        WHERE a.id=$1 AND a.expires_at>clock_timestamp() AND ((a.owner_id=$2 AND a.message_id IS NULL AND a.ephemeral_message_id IS NULL)
        OR (m.recipient_account IS NOT NULL AND (m.sender_account=$2 OR m.recipient_account=$2) AND m.expires_at>clock_timestamp() AND m.body->>'removedAt' IS NULL)))")
        .bind(id).bind(user).fetch_one(pool).await.map_err(routes::database_error)?;
    if !accessible {
        return Err(unavailable());
    }
    Ok(())
}

pub async fn download(
    State(server): State<SignalingServer>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<(HeaderMap, Body), AuthError> {
    let download = DOWNLOADS
        .clone()
        .try_acquire_owned()
        .map_err(|_| AuthError::ServiceBusy)?;
    let _permit = routes::acquire_auth_request(&server)?;
    allowed(&server, &headers, id).await?;
    let pool = server.db_pool().ok_or(AuthError::NotConfigured)?;
    let (name,content_type,data):(String,String,Vec<u8>)=sqlx::query_as("SELECT name,content_type,data FROM attachments WHERE id=$1 AND expires_at>clock_timestamp()")
        .bind(id).fetch_optional(pool).await.map_err(routes::database_error)?.ok_or_else(unavailable)?;
    // Recheck membership and message visibility after the bounded byte read.
    allowed(&server, &headers, id).await?;
    let mut response = routes::no_store_headers();
    response.insert(
        "content-type",
        HeaderValue::from_str(&content_type).map_err(|_| unavailable())?,
    );
    response.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    response.insert(
        "content-security-policy",
        HeaderValue::from_static("default-src 'none'; sandbox"),
    );
    let ascii: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, ' ' | '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let disposition = if content_type.starts_with("image/") {
        "inline"
    } else {
        "attachment"
    };
    response.insert(
        "content-disposition",
        HeaderValue::from_str(&format!("{disposition}; filename=\"{ascii}\""))
            .map_err(|_| unavailable())?,
    );
    response.insert("content-length", HeaderValue::from(data.len()));
    let body = futures_util::stream::unfold(
        (Bytes::from(data), download),
        |(mut remaining, permit)| async move {
            if remaining.is_empty() {
                None
            } else {
                let chunk = remaining.split_to(remaining.len().min(64 * 1024));
                Some((
                    Ok::<_, std::convert::Infallible>(chunk),
                    (remaining, permit),
                ))
            }
        },
    );
    Ok((response, Body::from_stream(body)))
}

pub(crate) async fn cleanup(pool: &PgPool) -> Result<u64, sqlx::Error> {
    Ok(sqlx::query("DELETE FROM attachments WHERE id IN(SELECT id FROM attachments WHERE expires_at<=now() ORDER BY expires_at LIMIT 1000)")
        .execute(pool).await?.rows_affected())
}

#[cfg(test)]
#[path = "attachments_tests.rs"]
mod tests;
