//! Bounded, opt-in room history and account-only private conversations.
//!
//! Visibility is checked before querying: public history requires live room
//! membership; a PM conversation is always derived from the authenticated UUID.
//! Expiry is enforced by every read, independently of background deletion.
use crate::auth::{account, routes, types::AuthError};
use crate::signaling::{SignalingServer, protocol::ChatEntry};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::HeaderMap,
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{PgConnection, PgPool, types::Json as SqlJson};
use uuid::Uuid;

pub const PM_RETENTION_DAYS: i32 = 90;
const PAGE_LIMIT: usize = 50;
/// Search examines the newest 10,000 retained messages, then paginates matches.
const SEARCH_WINDOW: i64 = 10_000;
const CLEANUP_BATCH: i64 = 1_000;

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HistoryQuery {
    pub before: Option<String>,
    pub after: Option<String>,
    pub around: Option<String>,
    pub resume: Option<bool>,
    pub q: Option<String>,
    pub limit: Option<u32>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryPage {
    pub messages: Vec<ChatEntry>,
    pub next_cursor: Option<String>,
    pub newer_cursor: Option<String>,
    pub first_unread_message_id: Option<String>,
    pub read_message_id: Option<String>,
    pub retention_days: i32,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Conversation {
    pub peer_id: Uuid,
    pub peer_name: String,
    pub last_message: ChatEntry,
    /// At most 1,000: the UI should display the cap as 1,000+.
    pub unread_count: i64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InboxPage {
    pub conversations: Vec<Conversation>,
    pub next_cursor: Option<String>,
    pub retention_days: i32,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReadRequest {
    pub message_id: Uuid,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadResult {
    pub read_message_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SendRequest {
    pub client_message_id: String,
    pub content: String,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EditRequest {
    pub content: String,
    pub expected_revision: u32,
}

/// Editing never changes identity, order, delivery receipts or conversation.
pub(crate) fn apply_edit(
    message: &mut ChatEntry,
    request: &EditRequest,
) -> Result<(), &'static str> {
    if request.content.trim().is_empty()
        || request.content.len() > 4096
        || request
            .content
            .chars()
            .any(|c| c.is_control() && c != '\n' && c != '\t')
    {
        return Err("Message must contain 1–4096 bytes of text");
    }
    if message.removed_at.is_some() {
        return Err("That message has been removed");
    }
    if message.revision != request.expected_revision {
        if request.expected_revision.checked_add(1) == Some(message.revision)
            && message.content == request.content
        {
            return Ok(()); // The previous attempt committed but its response was lost.
        }
        return Err("This message changed. Reload it before editing again");
    }
    if message.content == request.content {
        return Ok(());
    }
    message.revision = message
        .revision
        .checked_add(1)
        .ok_or("This message cannot be edited again")?;
    message.content.clone_from(&request.content);
    message.edited_at = Some(Utc::now().to_rfc3339());
    Ok(())
}

pub(crate) fn quote_excerpt(content: &str) -> String {
    let flat = content.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = flat.chars();
    let head: String = chars.by_ref().take(140).collect();
    if chars.next().is_some() {
        format!("{}…", head.trim_end())
    } else {
        head
    }
}

#[derive(Serialize, Deserialize)]
struct Cursor {
    at: DateTime<Utc>,
    id: Uuid,
}

fn encode_cursor(at: DateTime<Utc>, id: Uuid) -> String {
    URL_SAFE_NO_PAD.encode(format!("{}|{}", at.to_rfc3339(), id))
}

fn decode_cursor(value: Option<&str>) -> Result<Option<Cursor>, &'static str> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.len() > 160 {
        return Err("Invalid history cursor");
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| "Invalid history cursor")?;
    let decoded = std::str::from_utf8(&bytes).map_err(|_| "Invalid history cursor")?;
    let (at, id) = decoded.split_once('|').ok_or("Invalid history cursor")?;
    Ok(Some(Cursor {
        at: DateTime::parse_from_rfc3339(at)
            .map_err(|_| "Invalid history cursor")?
            .with_timezone(&Utc),
        id: id.parse().map_err(|_| "Invalid history cursor")?,
    }))
}

impl HistoryQuery {
    pub(crate) fn validate(&self) -> Result<(), &'static str> {
        decode_cursor(self.before.as_deref())?;
        decode_cursor(self.after.as_deref())?;
        let modes = usize::from(self.before.is_some())
            + usize::from(self.after.is_some())
            + usize::from(self.around.is_some())
            + usize::from(self.resume == Some(true));
        if modes > 1
            || (self.q.is_some()
                && (self.after.is_some() || self.around.is_some() || self.resume == Some(true)))
        {
            return Err("Choose one history position");
        }
        if self
            .around
            .as_ref()
            .is_some_and(|id| id.parse::<Uuid>().is_err())
        {
            return Err("Invalid message ID");
        }
        if self.limit.is_some_and(|limit| !(1..=50).contains(&limit)) {
            return Err("History pages contain 1–50 messages");
        }
        if let Some(query) = &self.q {
            let query = query.trim();
            if !(3..=128).contains(&query.chars().count()) || query.chars().any(char::is_control) {
                return Err("Search must contain 3–128 plain characters");
            }
        }
        Ok(())
    }
}

pub(crate) fn public_conversation(room: &str) -> String {
    format!("room:{room}")
}
fn private_conversation(a: Uuid, b: Uuid) -> String {
    let (a, b) = if a < b { (a, b) } else { (b, a) };
    format!("pm:{a}:{b}")
}

fn invalid(message: &'static str) -> sqlx::Error {
    sqlx::Error::InvalidArgument(message.into())
}

#[derive(Clone, sqlx::FromRow)]
struct ChatAccount {
    id: Uuid,
    display_name: String,
    preferences: serde_json::Value,
    chat_color: Option<String>,
    chat_style: String,
    auth_version: i64,
}

impl ChatAccount {
    fn allows_private_messages_from(&self, peer: Uuid) -> bool {
        let peer = peer.to_string();
        self.preferences
            .get("allowPrivateMessages")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true)
            && !self
                .preferences
                .get("ignored")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|people| {
                    people.iter().any(|person| {
                        person.get("id").and_then(serde_json::Value::as_str) == Some(peer.as_str())
                    })
                })
    }
}

/// Shared by room delivery and offline inbox sends. FOR SHARE keeps account
/// preference updates behind the message's commit; both accounts are always
/// locked in UUID order, before any session or message row is touched.
async fn lock_private_accounts(
    connection: &mut PgConnection,
    sender: Uuid,
    recipient: Uuid,
) -> Result<(ChatAccount, ChatAccount), sqlx::Error> {
    if sender == recipient {
        return Err(invalid("Private message unavailable"));
    }
    let people: Vec<ChatAccount> = sqlx::query_as(
        "SELECT id,display_name,preferences,chat_color,chat_style,auth_version FROM users WHERE id=ANY($1) ORDER BY id FOR SHARE")
        .bind(vec![sender, recipient]).fetch_all(connection).await?;
    let sender = people
        .iter()
        .find(|person| person.id == sender)
        .cloned()
        .ok_or_else(|| invalid("Private message unavailable"))?;
    let recipient = people
        .iter()
        .find(|person| person.id == recipient)
        .cloned()
        .ok_or_else(|| invalid("Private message unavailable"))?;
    Ok((sender, recipient))
}

fn private_accounts_allow(sender: &ChatAccount, recipient: &ChatAccount) -> bool {
    sender.allows_private_messages_from(recipient.id)
        && recipient.allows_private_messages_from(sender.id)
}

/// A successful return means the durable write committed before live delivery.
/// The caller holds room control, never its state lock, across this operation.
pub(crate) async fn persist_message(
    pool: &PgPool,
    room: Option<&str>,
    sender_session: Uuid,
    sender_authenticated: bool,
    retention_days: i32,
    message: &ChatEntry,
) -> Result<ChatEntry, sqlx::Error> {
    let mut tx = pool.begin().await?;
    if let Some(recipient) = message.recipient_id.as_deref() {
        let sender = message
            .participant_id
            .parse()
            .map_err(|_| invalid("Invalid sender"))?;
        let recipient = recipient
            .parse()
            .map_err(|_| invalid("Invalid recipient"))?;
        let (sender, recipient) = lock_private_accounts(&mut tx, sender, recipient).await?;
        if !sender_authenticated || !private_accounts_allow(&sender, &recipient) {
            return Err(invalid("Private message unavailable"));
        }
    }
    let saved = insert_message(
        &mut tx,
        room,
        sender_session,
        sender_authenticated,
        retention_days,
        message,
    )
    .await?;
    tx.commit().await?;
    if saved.message_id == message.message_id
        && let Some(recipient) = &saved.recipient_id
    {
        let recipient = recipient
            .parse()
            .map_err(|_| invalid("Invalid recipient"))?;
        crate::push::enqueue(pool, recipient).await;
    }
    Ok(saved)
}

async fn insert_message(
    connection: &mut PgConnection,
    room: Option<&str>,
    sender_session: Uuid,
    sender_authenticated: bool,
    retention_days: i32,
    message: &ChatEntry,
) -> Result<ChatEntry, sqlx::Error> {
    let sender: Uuid = message
        .participant_id
        .parse()
        .map_err(|_| invalid("Invalid sender"))?;
    let recipient = message
        .recipient_id
        .as_deref()
        .map(str::parse::<Uuid>)
        .transpose()
        .map_err(|_| invalid("Invalid recipient"))?;
    let conversation = match (room, recipient) {
        (Some(room), None) => public_conversation(room),
        (None, Some(recipient)) if sender_authenticated => private_conversation(sender, recipient),
        _ => return Err(invalid("Invalid saved conversation")),
    };
    // Serialize edits and quoted sends in one conversation so a reply cannot
    // commit a pre-edit excerpt after an edit has already updated older quotes.
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,29))")
        .bind(&conversation)
        .execute(&mut *connection)
        .await?;
    let mut message = message.clone();
    if let Some(reply) = &mut message.reply_to {
        let original: Option<SqlJson<ChatEntry>> = sqlx::query_scalar(
            "SELECT body FROM chat_messages WHERE conversation=$1 AND id=$2 AND expires_at>now()",
        )
        .bind(&conversation)
        .bind(
            reply
                .message_id
                .parse::<Uuid>()
                .map_err(|_| invalid("Invalid reply"))?,
        )
        .fetch_optional(&mut *connection)
        .await?;
        if let Some(original) = original {
            reply.excerpt = if original.0.removed_at.is_some() {
                "Message removed".into()
            } else {
                quote_excerpt(&original.0.content)
            };
        }
    }
    let id: Uuid = message
        .message_id
        .parse()
        .map_err(|_| invalid("Invalid message ID"))?;
    let at = DateTime::parse_from_rfc3339(&message.sent_at)
        .map_err(|_| invalid("Invalid message date"))?
        .with_timezone(&Utc);
    let expires = at + chrono::Duration::days(i64::from(retention_days));
    // The unique attempt key reconciles retries after an uncertain commit. Never
    // replace the saved body: moderation may already have removed that content.
    let inserted: Option<SqlJson<ChatEntry>> = sqlx::query_scalar(
        "INSERT INTO chat_messages (id,conversation,room_id,sender_account,recipient_account,sender_session,client_message_id,sent_at,expires_at,body)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)
         ON CONFLICT (conversation,sender_session,client_message_id) DO NOTHING RETURNING body")
        .bind(id).bind(&conversation).bind(room).bind(sender_authenticated.then_some(sender))
        .bind(recipient).bind(sender_session).bind(&message.client_message_id).bind(at).bind(expires)
        .bind(SqlJson(&message)).fetch_optional(&mut *connection).await?;
    let saved = if let Some(saved) = inserted {
        saved.0
    } else {
        let saved: SqlJson<ChatEntry> = sqlx::query_scalar(
            "SELECT body FROM chat_messages WHERE conversation=$1 AND sender_session=$2 AND client_message_id=$3")
            .bind(&conversation).bind(sender_session).bind(&message.client_message_id)
            .fetch_one(&mut *connection).await?;
        if saved.0.removed_at.is_none()
            && saved.0.edited_at.is_none()
            && (saved.0.content != message.content
                || saved.0.reply_to.as_ref().map(|r| &r.message_id)
                    != message.reply_to.as_ref().map(|r| &r.message_id))
        {
            return Err(invalid("Message ID was already used"));
        }
        return Ok(saved.0);
    };
    if let Some(recipient) = recipient {
        let pairs = if sender < recipient {
            [(sender, recipient), (recipient, sender)]
        } else {
            [(recipient, sender), (sender, recipient)]
        };
        for (user, peer) in pairs {
            sqlx::query("INSERT INTO chat_inbox (user_id,peer_id,conversation,last_message_id,last_sent_at,expires_at)
                VALUES ($1,$2,$3,$4,$5,$6) ON CONFLICT (user_id,peer_id) DO UPDATE SET
                last_message_id=EXCLUDED.last_message_id,last_sent_at=EXCLUDED.last_sent_at,expires_at=EXCLUDED.expires_at
                WHERE (chat_inbox.last_sent_at,chat_inbox.last_message_id)<(EXCLUDED.last_sent_at,EXCLUDED.last_message_id)")
                .bind(user).bind(peer).bind(&conversation).bind(id).bind(at).bind(expires)
                .execute(&mut *connection).await?;
        }
    }
    Ok(saved)
}

type MessageRow = (Uuid, DateTime<Utc>, SqlJson<ChatEntry>);

pub(crate) async fn page(
    pool: &PgPool,
    conversation: &str,
    account: Option<Uuid>,
    retention_days: i32,
    query: &HistoryQuery,
) -> Result<HistoryPage, sqlx::Error> {
    query.validate().map_err(invalid)?;
    let cursor =
        decode_cursor(query.before.as_deref().or(query.after.as_deref())).map_err(invalid)?;
    let limit = query.limit.unwrap_or(PAGE_LIMIT as u32) as usize;
    let read: Option<(Uuid, DateTime<Utc>)> = if let Some(account) = account {
        sqlx::query_as("SELECT message_id,sent_at FROM chat_read_cursors WHERE user_id=$1 AND conversation=$2 AND expires_at>now()")
            .bind(account).bind(conversation).fetch_optional(pool).await?
    } else {
        None
    };
    let first_unread: Option<Uuid> = if let Some(account) = account {
        sqlx::query_scalar(
            "SELECT id FROM chat_messages WHERE conversation=$1 AND expires_at>now()
            AND sender_account IS DISTINCT FROM $2 AND body->>'removedAt' IS NULL
            AND ($3::timestamptz IS NULL OR (sent_at,id)>($3,$4)) ORDER BY sent_at,id LIMIT 1",
        )
        .bind(conversation)
        .bind(account)
        .bind(read.map(|r| r.1))
        .bind(read.map(|r| r.0))
        .fetch_optional(pool)
        .await?
    } else {
        None
    };
    let anchor = query
        .around
        .as_deref()
        .map(str::parse::<Uuid>)
        .transpose()
        .map_err(|_| invalid("Invalid message ID"))?
        .or_else(|| {
            (query.resume == Some(true))
                .then_some(first_unread)
                .flatten()
        });
    let mut rows: Vec<MessageRow> = if let Some(anchor) = anchor {
        let at:DateTime<Utc> = sqlx::query_scalar("SELECT sent_at FROM chat_messages WHERE conversation=$1 AND id=$2 AND expires_at>now()")
            .bind(conversation).bind(anchor).fetch_one(pool).await?;
        // Two bounded index walks provide context on either side without an OFFSET scan.
        sqlx::query_as("(SELECT id,sent_at,body FROM chat_messages WHERE conversation=$1 AND expires_at>now()
                AND (sent_at,id)<($2,$3) ORDER BY sent_at DESC,id DESC LIMIT $4)
            UNION ALL (SELECT id,sent_at,body FROM chat_messages WHERE conversation=$1 AND expires_at>now()
                AND (sent_at,id)>=($2,$3) ORDER BY sent_at,id LIMIT $5)")
            .bind(conversation).bind(at).bind(anchor).bind((limit/2) as i64).bind((limit-limit/2) as i64)
            .fetch_all(pool).await?
    } else if let Some(search) = &query.q {
        // Materialize only a bounded recent window; '%' and '_' are literal characters.
        sqlx::query_as("WITH recent AS MATERIALIZED (
            SELECT id,sent_at,body FROM chat_messages WHERE conversation=$1 AND expires_at>now()
            ORDER BY sent_at DESC,id DESC LIMIT $6)
            SELECT id,sent_at,body FROM recent WHERE ($2::timestamptz IS NULL OR (sent_at,id)<($2,$3))
              AND body->>'removedAt' IS NULL AND position(lower($4) in lower(body->>'content'))>0
            ORDER BY sent_at DESC,id DESC LIMIT $5")
            .bind(conversation).bind(cursor.as_ref().map(|c|c.at)).bind(cursor.as_ref().map(|c|c.id))
            .bind(search.trim()).bind((limit+1) as i64).bind(SEARCH_WINDOW).fetch_all(pool).await?
    } else if query.after.is_some() {
        sqlx::query_as(
            "SELECT id,sent_at,body FROM chat_messages WHERE conversation=$1 AND expires_at>now()
            AND (sent_at,id)>($2,$3) ORDER BY sent_at,id LIMIT $4",
        )
        .bind(conversation)
        .bind(cursor.as_ref().map(|c| c.at))
        .bind(cursor.as_ref().map(|c| c.id))
        .bind((limit + 1) as i64)
        .fetch_all(pool)
        .await?
    } else {
        sqlx::query_as("SELECT id,sent_at,body FROM chat_messages WHERE conversation=$1 AND expires_at>now()
            AND ($2::timestamptz IS NULL OR (sent_at,id)<($2,$3)) ORDER BY sent_at DESC,id DESC LIMIT $4")
            .bind(conversation).bind(cursor.as_ref().map(|c|c.at)).bind(cursor.as_ref().map(|c|c.id))
            .bind((limit+1) as i64).fetch_all(pool).await?
    };
    let more = rows.len() > limit;
    rows.truncate(limit);
    rows.sort_unstable_by_key(|(id, at, _)| (*at, *id));
    let (next_cursor, newer_cursor) = if query.q.is_some() {
        (
            more.then(|| rows.first().map(|(id, at, _)| encode_cursor(*at, *id)))
                .flatten(),
            None,
        )
    } else if let (Some((first, first_at, _)), Some((last, last_at, _))) =
        (rows.first(), rows.last())
    {
        let (older,newer):(bool,bool)=sqlx::query_as("SELECT
            EXISTS(SELECT 1 FROM chat_messages WHERE conversation=$1 AND expires_at>now() AND (sent_at,id)<($2,$3)),
            EXISTS(SELECT 1 FROM chat_messages WHERE conversation=$1 AND expires_at>now() AND (sent_at,id)>($4,$5))")
            .bind(conversation).bind(first_at).bind(first).bind(last_at).bind(last).fetch_one(pool).await?;
        (
            older.then(|| encode_cursor(*first_at, *first)),
            newer.then(|| encode_cursor(*last_at, *last)),
        )
    } else {
        (None, None)
    };
    Ok(HistoryPage {
        messages: rows.into_iter().map(|(_, _, entry)| entry.0).collect(),
        next_cursor,
        newer_cursor,
        first_unread_message_id: first_unread.map(|id| id.to_string()),
        read_message_id: read.map(|(id, _)| id.to_string()),
        retention_days,
    })
}

pub(crate) async fn mark_read(
    pool: &PgPool,
    conversation: &str,
    account: Uuid,
    message: Uuid,
) -> Result<ReadResult, sqlx::Error> {
    let result: Option<Uuid> = sqlx::query_scalar(
        "INSERT INTO chat_read_cursors (user_id,conversation,message_id,sent_at,expires_at)
         SELECT $1,conversation,id,sent_at,expires_at FROM chat_messages
         WHERE conversation=$2 AND id=$3 AND expires_at>now()
         ON CONFLICT (user_id,conversation) DO UPDATE SET
           message_id=CASE WHEN (chat_read_cursors.sent_at,chat_read_cursors.message_id)<(EXCLUDED.sent_at,EXCLUDED.message_id) THEN EXCLUDED.message_id ELSE chat_read_cursors.message_id END,
           sent_at=GREATEST(chat_read_cursors.sent_at,EXCLUDED.sent_at),expires_at=GREATEST(chat_read_cursors.expires_at,EXCLUDED.expires_at)
         RETURNING message_id")
        .bind(account).bind(conversation).bind(message).fetch_optional(pool).await?;
    Ok(ReadResult {
        read_message_id: result.ok_or(sqlx::Error::RowNotFound)?.to_string(),
    })
}

pub(crate) async fn save_reactions(
    pool: &PgPool,
    id: &str,
    reactions: &[crate::signaling::protocol::ChatReaction],
) -> Result<(), sqlx::Error> {
    let id: Uuid = id.parse().map_err(|_| invalid("Invalid message ID"))?;
    sqlx::query("UPDATE chat_messages SET body=jsonb_set(body,'{reactions}',$2) WHERE id=$1 AND expires_at>now() AND body->>'removedAt' IS NULL")
        .bind(id).bind(SqlJson(reactions)).execute(pool).await?;
    Ok(())
}

pub(crate) async fn lookup_public(
    pool: &PgPool,
    room: &str,
    id: &str,
) -> Result<Option<(ChatEntry, bool)>, sqlx::Error> {
    let id: Uuid = id.parse().map_err(|_| invalid("Invalid message ID"))?;
    let row: Option<(SqlJson<ChatEntry>,bool)> = sqlx::query_as(
        "SELECT body,sender_account IS NOT NULL FROM chat_messages WHERE room_id=$1 AND id=$2 AND expires_at>now()")
        .bind(room).bind(id).fetch_optional(pool).await?;
    Ok(row.map(|(message, authenticated)| (message.0, authenticated)))
}

/// Call with the same transaction that records the moderation decision.
pub(crate) async fn remove_public(
    connection: &mut PgConnection,
    room: &str,
    id: &str,
    removed_at: &str,
) -> Result<Option<(ChatEntry, bool)>, sqlx::Error> {
    let id: Uuid = id.parse().map_err(|_| invalid("Invalid message ID"))?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,29))")
        .bind(public_conversation(room))
        .execute(&mut *connection)
        .await?;
    let row: Option<(SqlJson<ChatEntry>,bool)> = sqlx::query_as(
        "UPDATE chat_messages SET body=(body-'replyTo') || jsonb_build_object('content','','reactions','[]'::jsonb,'removedAt',COALESCE(body->>'removedAt',$3))
         WHERE room_id=$1 AND id=$2 AND expires_at>now() RETURNING body,sender_account IS NOT NULL")
        .bind(room).bind(id).bind(removed_at).fetch_optional(&mut *connection).await?;
    {
        sqlx::query("UPDATE chat_messages SET body=jsonb_set(body,'{replyTo,excerpt}','\"Message removed\"'::jsonb)
            WHERE room_id=$1 AND body->'replyTo'->>'messageId'=$2")
            .bind(room).bind(id.to_string()).execute(&mut *connection).await?;
    }
    sqlx::query("DELETE FROM chat_pins WHERE room_id=$1 AND message_id=$2")
        .bind(room)
        .bind(id)
        .execute(&mut *connection)
        .await?;
    Ok(row.map(|(entry, authenticated)| (entry.0, authenticated)))
}

/// Caller has already locked account consent and the live HTTP session, if any.
async fn edit_in_transaction(
    connection: &mut PgConnection,
    conversation: &str,
    actor: Uuid,
    guest_session: Option<Uuid>,
    message: Uuid,
    request: &EditRequest,
) -> Result<ChatEntry, sqlx::Error> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,29))")
        .bind(conversation)
        .execute(&mut *connection)
        .await?;
    let saved: SqlJson<ChatEntry> = sqlx::query_scalar(
        "SELECT body FROM chat_messages
        WHERE conversation=$1 AND id=$2 AND expires_at>now()
          AND (sender_account=$3 OR (sender_account IS NULL AND sender_session=$4)) FOR UPDATE",
    )
    .bind(conversation)
    .bind(message)
    .bind(actor)
    .bind(guest_session)
    .fetch_one(&mut *connection)
    .await?;
    let mut edited = saved.0;
    let old_revision = edited.revision;
    apply_edit(&mut edited, request).map_err(invalid)?;
    if old_revision != edited.revision {
        sqlx::query("UPDATE chat_messages SET body=$2 WHERE id=$1")
            .bind(message)
            .bind(SqlJson(&edited))
            .execute(&mut *connection)
            .await?;
        sqlx::query(
            "UPDATE chat_messages SET body=jsonb_set(body,'{replyTo,excerpt}',$3)
            WHERE conversation=$1 AND body->'replyTo'->>'messageId'=$2 AND expires_at>now()",
        )
        .bind(conversation)
        .bind(message.to_string())
        .bind(SqlJson(quote_excerpt(&edited.content)))
        .execute(&mut *connection)
        .await?;
    }
    Ok(edited)
}

pub(crate) async fn edit_saved(
    pool: &PgPool,
    room: Option<&str>,
    actor: Uuid,
    guest_session: Option<Uuid>,
    original: &ChatEntry,
    request: &EditRequest,
) -> Result<ChatEntry, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let conversation = if let Some(peer) = &original.recipient_id {
        let peer = peer.parse().map_err(|_| invalid("Invalid recipient"))?;
        let (sender, recipient) = lock_private_accounts(&mut tx, actor, peer).await?;
        if guest_session.is_some() || !private_accounts_allow(&sender, &recipient) {
            return Err(invalid("Private message unavailable"));
        }
        private_conversation(actor, peer)
    } else {
        public_conversation(room.ok_or_else(|| invalid("Room unavailable"))?)
    };
    let message = original
        .message_id
        .parse()
        .map_err(|_| invalid("Invalid message ID"))?;
    let edited = edit_in_transaction(
        &mut tx,
        &conversation,
        actor,
        guest_session,
        message,
        request,
    )
    .await?;
    tx.commit().await?;
    Ok(edited)
}

pub async fn edit_message(
    State(server): State<SignalingServer>,
    headers: HeaderMap,
    Path((peer, message)): Path<(Uuid, Uuid)>,
    Json(request): Json<EditRequest>,
) -> Result<(HeaderMap, Json<ChatEntry>), AuthError> {
    let _permit = routes::acquire_auth_request(&server)?;
    let claims = account::authenticated_claims(&server, &headers).await?;
    let own: Uuid = claims.sub.parse().map_err(|_| AuthError::InvalidToken)?;
    let pool = server.db_pool().ok_or(AuthError::NotConfigured)?;
    let mut tx = pool.begin().await.map_err(routes::database_error)?;
    let (sender, recipient) = lock_private_accounts(&mut tx, own, peer)
        .await
        .map_err(edit_error)?;
    if sender.auth_version != claims.auth_version {
        return Err(AuthError::InvalidToken);
    }
    let current:bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM sessions WHERE id=$1 AND user_id=$2 AND expires_at>clock_timestamp() FOR SHARE)")
        .bind(claims.sid).bind(own).fetch_one(&mut *tx).await.map_err(routes::database_error)?;
    if !current {
        return Err(AuthError::InvalidToken);
    }
    if !private_accounts_allow(&sender, &recipient) {
        return Err(AuthError::InvalidInput("Private message unavailable"));
    }
    let edited = edit_in_transaction(
        &mut tx,
        &private_conversation(own, peer),
        own,
        None,
        message,
        &request,
    )
    .await
    .map_err(edit_error)?;
    tx.commit().await.map_err(routes::database_error)?;
    server.room_manager().deliver_inbox_edit(&edited).await;
    Ok((routes::no_store_headers(), Json(edited)))
}

fn edit_error(error: sqlx::Error) -> AuthError {
    match error {
        sqlx::Error::RowNotFound => AuthError::InvalidInput("Message unavailable"),
        sqlx::Error::InvalidArgument(_) => {
            AuthError::InvalidInput("Message unavailable or changed; refresh before editing")
        }
        other => routes::database_error(other),
    }
}

pub(crate) async fn pinned_messages(
    pool: &PgPool,
    room: &str,
) -> Result<Vec<ChatEntry>, sqlx::Error> {
    let rows: Vec<SqlJson<ChatEntry>> = sqlx::query_scalar(
        "SELECT m.body FROM chat_pins p JOIN chat_messages m ON m.id=p.message_id
        WHERE p.room_id=$1 AND m.room_id=$1 AND m.expires_at>now() AND m.body->>'removedAt' IS NULL
        ORDER BY p.pinned_at DESC,p.message_id DESC LIMIT 3",
    )
    .bind(room)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(|r| r.0).collect())
}

pub(crate) async fn set_pin(
    pool: &PgPool,
    room: &str,
    message: &str,
    pinned: bool,
) -> Result<(), sqlx::Error> {
    let message: Uuid = message.parse().map_err(|_| invalid("Invalid message ID"))?;
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT id FROM rooms WHERE id=$1 FOR UPDATE")
        .bind(room)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM chat_pins WHERE room_id=$1 AND message_id IN (SELECT id FROM chat_messages WHERE expires_at<=now() OR body->>'removedAt' IS NOT NULL)")
        .bind(room).execute(&mut *tx).await?;
    if pinned {
        let available:bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM chat_messages WHERE room_id=$1 AND id=$2 AND expires_at>now() AND body->>'removedAt' IS NULL)")
            .bind(room).bind(message).fetch_one(&mut *tx).await?;
        if !available {
            return Err(sqlx::Error::RowNotFound);
        }
        let existing: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM chat_pins WHERE room_id=$1 AND message_id=$2)",
        )
        .bind(room)
        .bind(message)
        .fetch_one(&mut *tx)
        .await?;
        if !existing {
            let slot:Option<i64> = sqlx::query_scalar("SELECT s FROM generate_series(1::bigint,3::bigint) s WHERE NOT EXISTS(SELECT 1 FROM chat_pins WHERE room_id=$1 AND slot=s) ORDER BY s LIMIT 1")
                .bind(room).fetch_optional(&mut *tx).await?;
            let slot = slot.ok_or_else(|| {
                invalid("Unpin a message before pinning another; rooms keep up to three")
            })?;
            sqlx::query("INSERT INTO chat_pins(room_id,message_id,slot) VALUES($1,$2,$3)")
                .bind(room)
                .bind(message)
                .bind(slot)
                .execute(&mut *tx)
                .await?;
        }
    } else {
        sqlx::query("DELETE FROM chat_pins WHERE room_id=$1 AND message_id=$2")
            .bind(room)
            .bind(message)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await
}

/// Public history stops being readable and is erased in the settings transaction.
pub(crate) async fn set_retention(pool: &PgPool, room: &str, days: i32) -> Result<(), sqlx::Error> {
    if ![0, 1, 7, 30, 90].contains(&days) {
        return Err(invalid("Invalid history retention"));
    }
    let mut tx = pool.begin().await?;
    let updated =
        sqlx::query("UPDATE rooms SET history_retention_days=$2,updated_at=now() WHERE id=$1")
            .bind(room)
            .bind(i64::from(days))
            .execute(&mut *tx)
            .await?;
    if updated.rows_affected() != 1 {
        return Err(sqlx::Error::RowNotFound);
    }
    sqlx::query("DELETE FROM chat_messages WHERE room_id=$1 AND ($2=0 OR sent_at<=now()-make_interval(days=>$2))")
        .bind(room).bind(days).execute(&mut *tx).await?;
    // Extending retention affects future messages only. Never extend a promise
    // already made when a message was saved under a shorter retention period.
    sqlx::query("UPDATE chat_messages SET expires_at=LEAST(expires_at,sent_at+make_interval(days=>$2)) WHERE room_id=$1")
        .bind(room).bind(days).execute(&mut *tx).await?;
    if days == 0 {
        sqlx::query("DELETE FROM chat_read_cursors WHERE conversation=$1")
            .bind(public_conversation(room))
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await
}

pub async fn inbox(
    State(server): State<SignalingServer>,
    headers: HeaderMap,
    Query(query): Query<HistoryQuery>,
) -> Result<(HeaderMap, Json<InboxPage>), AuthError> {
    let _permit = routes::acquire_auth_request(&server)?;
    let claims = account::authenticated_claims(&server, &headers).await?;
    let own: Uuid = claims.sub.parse().map_err(|_| AuthError::InvalidToken)?;
    query.validate().map_err(AuthError::InvalidInput)?;
    let cursor = decode_cursor(query.before.as_deref()).map_err(AuthError::InvalidInput)?;
    let pool = server.db_pool().ok_or(AuthError::NotConfigured)?;
    type Row = (Uuid, String, Uuid, DateTime<Utc>, SqlJson<ChatEntry>, i64);
    let mut rows:Vec<Row>=sqlx::query_as("SELECT i.peer_id,u.display_name,i.last_message_id,i.last_sent_at,m.body,
        (SELECT count(*) FROM (SELECT 1 FROM chat_messages incoming
          LEFT JOIN chat_read_cursors r ON r.user_id=$1 AND r.conversation=i.conversation
          WHERE incoming.conversation=i.conversation AND incoming.recipient_account=$1 AND incoming.expires_at>now()
            AND (r.message_id IS NULL OR r.expires_at<=now() OR (incoming.sent_at,incoming.id)>(r.sent_at,r.message_id)) LIMIT 1000) unread)
        FROM chat_inbox i JOIN users u ON u.id=i.peer_id JOIN chat_messages m ON m.id=i.last_message_id
        WHERE i.user_id=$1 AND i.expires_at>now() AND m.expires_at>now()
          AND ($2::timestamptz IS NULL OR (i.last_sent_at,i.last_message_id)<($2,$3))
        ORDER BY i.last_sent_at DESC,i.last_message_id DESC LIMIT 51")
        .bind(own).bind(cursor.as_ref().map(|c|c.at)).bind(cursor.as_ref().map(|c|c.id))
        .fetch_all(pool).await.map_err(routes::database_error)?;
    let more = rows.len() > PAGE_LIMIT;
    rows.truncate(PAGE_LIMIT);
    let next_cursor = if more {
        rows.last()
            .map(|(_, _, id, at, _, _)| encode_cursor(*at, *id))
    } else {
        None
    };
    Ok((
        routes::no_store_headers(),
        Json(InboxPage {
            conversations: rows
                .into_iter()
                .map(
                    |(peer_id, peer_name, _, _, message, unread_count)| Conversation {
                        peer_id,
                        peer_name,
                        last_message: message.0,
                        unread_count,
                    },
                )
                .collect(),
            next_cursor,
            retention_days: PM_RETENTION_DAYS,
        }),
    ))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UnreadSummary {
    pub unread_count: i64,
}

pub async fn unread_summary(
    State(server): State<SignalingServer>,
    headers: HeaderMap,
) -> Result<(HeaderMap, Json<UnreadSummary>), AuthError> {
    let _permit = routes::acquire_auth_request(&server)?;
    let claims = account::authenticated_claims(&server, &headers).await?;
    let own: Uuid = claims.sub.parse().map_err(|_| AuthError::InvalidToken)?;
    let pool = server.db_pool().ok_or(AuthError::NotConfigured)?;
    let unread_count:i64 = sqlx::query_scalar("SELECT count(*) FROM (
        SELECT 1 FROM chat_messages m LEFT JOIN chat_read_cursors r ON r.user_id=$1 AND r.conversation=m.conversation
        WHERE m.recipient_account=$1 AND m.expires_at>now() AND m.body->>'removedAt' IS NULL
          AND (r.message_id IS NULL OR r.expires_at<=now() OR (m.sent_at,m.id)>(r.sent_at,r.message_id)) LIMIT 1000) unread")
        .bind(own).fetch_one(pool).await.map_err(routes::database_error)?;
    Ok((
        routes::no_store_headers(),
        Json(UnreadSummary { unread_count }),
    ))
}

pub async fn messages(
    State(server): State<SignalingServer>,
    headers: HeaderMap,
    Path(peer): Path<Uuid>,
    Query(query): Query<HistoryQuery>,
) -> Result<(HeaderMap, Json<HistoryPage>), AuthError> {
    let _permit = routes::acquire_auth_request(&server)?;
    let claims = account::authenticated_claims(&server, &headers).await?;
    let own: Uuid = claims.sub.parse().map_err(|_| AuthError::InvalidToken)?;
    query.validate().map_err(AuthError::InvalidInput)?;
    let pool = server.db_pool().ok_or(AuthError::NotConfigured)?;
    let result = page(
        pool,
        &private_conversation(own, peer),
        Some(own),
        PM_RETENTION_DAYS,
        &query,
    )
    .await
    .map_err(routes::database_error)?;
    Ok((routes::no_store_headers(), Json(result)))
}

pub async fn read(
    State(server): State<SignalingServer>,
    headers: HeaderMap,
    Path(peer): Path<Uuid>,
    Json(request): Json<ReadRequest>,
) -> Result<(HeaderMap, Json<ReadResult>), AuthError> {
    let _permit = routes::acquire_auth_request(&server)?;
    let claims = account::authenticated_claims(&server, &headers).await?;
    let own: Uuid = claims.sub.parse().map_err(|_| AuthError::InvalidToken)?;
    let pool = server.db_pool().ok_or(AuthError::NotConfigured)?;
    let result = mark_read(
        pool,
        &private_conversation(own, peer),
        own,
        request.message_id,
    )
    .await
    .map_err(|error| match error {
        sqlx::Error::RowNotFound => AuthError::InvalidInput("Message unavailable"),
        other => routes::database_error(other),
    })?;
    Ok((routes::no_store_headers(), Json(result)))
}

pub async fn send_message(
    State(server): State<SignalingServer>,
    headers: HeaderMap,
    Path(peer): Path<Uuid>,
    Json(request): Json<SendRequest>,
) -> Result<(HeaderMap, Json<ChatEntry>), AuthError> {
    let _permit = routes::acquire_auth_request(&server)?;
    let claims = account::authenticated_claims(&server, &headers).await?;
    let own: Uuid = claims.sub.parse().map_err(|_| AuthError::InvalidToken)?;
    if own == peer
        || !crate::signaling::protocol::valid_correlation_id(&request.client_message_id)
        || request.content.trim().is_empty()
        || request.content.len() > 4096
        || request
            .content
            .chars()
            .any(|c| c.is_control() && c != '\n' && c != '\t')
    {
        return Err(AuthError::InvalidInput("Invalid private message"));
    }
    let pool = server.db_pool().ok_or(AuthError::NotConfigured)?;
    let conversation = private_conversation(own, peer);
    let mut tx = pool.begin().await.map_err(routes::database_error)?;
    let (sender, recipient) = lock_private_accounts(&mut tx, own, peer)
        .await
        .map_err(|error| match error {
            sqlx::Error::InvalidArgument(_) => {
                AuthError::InvalidInput("Private message unavailable")
            }
            error => routes::database_error(error),
        })?;
    if sender.auth_version != claims.auth_version {
        return Err(AuthError::InvalidToken);
    }
    let session:Option<Uuid>=sqlx::query_scalar("SELECT id FROM sessions WHERE id=$1 AND user_id=$2 AND expires_at>clock_timestamp() FOR SHARE")
        .bind(claims.sid).bind(own).fetch_optional(&mut *tx).await.map_err(routes::database_error)?;
    if session.is_none() {
        return Err(AuthError::InvalidToken);
    }
    let contact:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM chat_inbox WHERE user_id=$1 AND peer_id=$2 AND expires_at>now())")
        .bind(own).bind(peer).fetch_one(&mut *tx).await.map_err(routes::database_error)?;
    let contact = contact
        || crate::auth::discovery::accepted_contact(&mut tx, own, peer)
            .await
            .map_err(routes::database_error)?;
    if !contact {
        return Err(AuthError::InvalidInput(
            "Add this person as a contact or start the conversation in a room",
        ));
    }
    // One account's inbox writes serialize with each other, bounding a burst
    // across multiple sessions. Retries do not consume a second send slot.
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1::text,17))")
        .bind(own.to_string())
        .execute(&mut *tx)
        .await
        .map_err(routes::database_error)?;
    let existing:Option<SqlJson<ChatEntry>>=sqlx::query_scalar("SELECT body FROM chat_messages WHERE conversation=$1 AND sender_session=$2 AND client_message_id=$3 AND expires_at>now()")
        .bind(&conversation).bind(own).bind(&request.client_message_id).fetch_optional(&mut *tx).await.map_err(routes::database_error)?;
    if let Some(existing) = existing {
        if existing.0.edited_at.is_none() && existing.0.content != request.content {
            return Err(AuthError::InvalidInput("Message ID was already used"));
        }
        return Ok((routes::no_store_headers(), Json(existing.0)));
    }
    if !private_accounts_allow(&sender, &recipient) {
        return Err(AuthError::InvalidInput("Private message unavailable"));
    }
    let recent:i64=sqlx::query_scalar("SELECT count(*) FROM (SELECT 1 FROM chat_messages WHERE sender_account=$1 AND sender_session=$1 AND recipient_account IS NOT NULL AND sent_at>now()-interval '1 minute' LIMIT 30) recent")
        .bind(own).fetch_one(&mut *tx).await.map_err(routes::database_error)?;
    if recent >= 30 {
        return Err(AuthError::RateLimited);
    }
    let style = serde_json::from_value(
        serde_json::json!({"color":sender.chat_color,"style":sender.chat_style}),
    )
    .unwrap_or_default();
    let message = ChatEntry {
        message_id: Uuid::new_v4().to_string(),
        client_message_id: request.client_message_id,
        participant_id: own.to_string(),
        participant_name: sender.display_name.clone(),
        recipient_id: Some(peer.to_string()),
        recipient_name: Some(recipient.display_name.clone()),
        content: request.content,
        sent_at: Utc::now().to_rfc3339(),
        removed_at: None,
        revision: 0,
        edited_at: None,
        chat_style: style,
        reply_to: None,
        reactions: Vec::new(),
    };
    let saved = insert_message(&mut tx, None, own, true, PM_RETENTION_DAYS, &message)
        .await
        .map_err(routes::database_error)?;
    tx.commit().await.map_err(routes::database_error)?;
    server.room_manager().deliver_inbox_message(&saved);
    crate::push::enqueue(pool, peer).await;
    Ok((routes::no_store_headers(), Json(saved)))
}

impl super::RoomManager {
    /// Inbox storage owns delivery; live room sockets get a bounded notification
    /// as well. A busy/disconnected socket reconciles from its durable inbox.
    pub(crate) fn deliver_inbox_message(&self, message: &ChatEntry) {
        let Some(recipient) = message.recipient_id.as_deref() else {
            return;
        };
        let Ok(event) = serde_json::to_string(
            &crate::signaling::protocol::ServerMessage::PrivateMessageReceived {
                message: message.clone(),
            },
        ) else {
            return;
        };
        let event = crate::OutboundJson::from(event);
        let rooms: Vec<_> = self
            .rooms
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .values()
            .cloned()
            .collect();
        for lock in rooms {
            let Ok(room) = lock.try_read() else {
                continue;
            };
            if room.deleting {
                continue;
            }
            let Some(participant) = room.participants.get(recipient) else {
                continue;
            };
            if participant.authenticated
                && participant
                    .social
                    .accepts_inbox_from(&message.participant_id)
            {
                let _ =
                    super::try_send_essential(&self.metrics, &participant.sender, event.clone());
            }
        }
    }
}

/// Physically erase expired data in bounded batches; read predicates already
/// make expiry exact even while a large cleanup backlog remains.
pub fn spawn_retention(pool: PgPool) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(300));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            let result=tokio::time::timeout(std::time::Duration::from_secs(15), async {
                for _ in 0..16 {
                    let messages = sqlx::query("DELETE FROM chat_messages WHERE id IN (SELECT id FROM chat_messages WHERE expires_at<=now() ORDER BY expires_at LIMIT $1)")
                        .bind(CLEANUP_BATCH).execute(&pool).await?.rows_affected();
                    let inbox = sqlx::query("DELETE FROM chat_inbox WHERE (user_id,peer_id) IN (SELECT user_id,peer_id FROM chat_inbox WHERE expires_at<=now() ORDER BY expires_at LIMIT $1)")
                        .bind(CLEANUP_BATCH).execute(&pool).await?.rows_affected();
                    let cursors = sqlx::query("DELETE FROM chat_read_cursors WHERE (user_id,conversation) IN (SELECT user_id,conversation FROM chat_read_cursors WHERE expires_at<=now() ORDER BY expires_at LIMIT $1)")
                        .bind(CLEANUP_BATCH).execute(&pool).await?.rows_affected();
                    if messages < CLEANUP_BATCH as u64 && inbox < CLEANUP_BATCH as u64 && cursors < CLEANUP_BATCH as u64 { break; }
                }
                Ok::<_,sqlx::Error>(())
            }).await;
            if !matches!(result, Ok(Ok(()))) {
                tracing::warn!("Chat history cleanup incomplete; the next sweep retries");
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn history_queries_are_bounded_and_cursors_round_trip() {
        let at = Utc::now();
        let id = Uuid::new_v4();
        let encoded = encode_cursor(at, id);
        let decoded = decode_cursor(Some(&encoded)).unwrap().unwrap();
        assert_eq!(decoded.id, id);
        assert_eq!(decoded.at, at);
        assert!(decode_cursor(Some("bad cursor")).is_err());
        for limit in [0, 51, u32::MAX] {
            assert!(
                HistoryQuery {
                    limit: Some(limit),
                    ..Default::default()
                }
                .validate()
                .is_err()
            );
        }
        for query in ["a".to_string(), "x".repeat(129), "abc\n".to_string() + "x"] {
            assert!(
                HistoryQuery {
                    q: Some(query),
                    ..Default::default()
                }
                .validate()
                .is_err()
            );
        }
    }
    #[test]
    fn private_conversation_is_symmetric_and_disjoint_from_room() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        assert_eq!(private_conversation(a, b), private_conversation(b, a));
        assert_ne!(
            private_conversation(a, b),
            private_conversation(a, Uuid::new_v4())
        );
        assert_ne!(
            private_conversation(a, b),
            public_conversation(&a.to_string())
        );
    }
    fn entry(content: &str) -> ChatEntry {
        ChatEntry {
            message_id: Uuid::new_v4().to_string(),
            client_message_id: Uuid::new_v4().to_string(),
            participant_id: Uuid::new_v4().to_string(),
            participant_name: "Author".into(),
            recipient_id: None,
            recipient_name: None,
            content: content.into(),
            sent_at: Utc::now().to_rfc3339(),
            removed_at: None,
            revision: 0,
            edited_at: None,
            chat_style: Default::default(),
            reply_to: None,
            reactions: vec![],
        }
    }

    #[test]
    fn message_edits_are_revision_checked_idempotent_and_removal_is_terminal() {
        let mut message = entry("Original");
        let original_id = message.message_id.clone();
        let request = EditRequest {
            content: "Corrected".into(),
            expected_revision: 0,
        };
        apply_edit(&mut message, &request).unwrap();
        assert_eq!(message.revision, 1);
        assert!(message.edited_at.is_some());
        assert_eq!(message.message_id, original_id);
        let edited_at = message.edited_at.clone();
        apply_edit(&mut message, &request).unwrap();
        assert_eq!(message.edited_at, edited_at);
        assert!(
            apply_edit(
                &mut message,
                &EditRequest {
                    content: "Stale tab".into(),
                    expected_revision: 0
                }
            )
            .is_err()
        );
        assert_eq!(message.content, "Corrected");
        for invalid in ["".into(), " ".into(), "x".repeat(4097), "bad\0text".into()] {
            assert!(
                apply_edit(
                    &mut message,
                    &EditRequest {
                        content: invalid,
                        expected_revision: 1
                    }
                )
                .is_err()
            );
        }
        message.removed_at = Some(Utc::now().to_rfc3339());
        message.content.clear();
        assert!(
            apply_edit(
                &mut message,
                &EditRequest {
                    content: "Restored".into(),
                    expected_revision: 1
                }
            )
            .is_err()
        );
        assert!(message.content.is_empty());
    }

    #[test]
    fn history_positions_are_exclusive_and_message_ids_validated() {
        for query in [
            HistoryQuery {
                around: Some("not-an-id".into()),
                ..Default::default()
            },
            HistoryQuery {
                around: Some(Uuid::new_v4().to_string()),
                resume: Some(true),
                ..Default::default()
            },
            HistoryQuery {
                around: Some(Uuid::new_v4().to_string()),
                q: Some("query".into()),
                ..Default::default()
            },
            HistoryQuery {
                after: Some("invalid".into()),
                ..Default::default()
            },
        ] {
            assert!(query.validate().is_err());
        }
    }

    #[tokio::test]
    #[ignore = "requires a migrated disposable TEST_DATABASE_URL"]
    async fn database_chat_edits_pins_context_and_unread_preserve_visibility_and_retention() {
        let pool = PgPool::connect(&std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL"))
            .await
            .unwrap();
        let author = Uuid::new_v4();
        let reader = Uuid::new_v4();
        for user in [author, reader] {
            sqlx::query(
                "INSERT INTO users(id,email,display_name) VALUES($1,$2,'Chat update tester')",
            )
            .bind(user)
            .bind(format!("{user}@chatupdates.invalid"))
            .execute(&pool)
            .await
            .unwrap();
        }
        let room = format!("updates-{}", Uuid::new_v4());
        sqlx::query(
            "INSERT INTO rooms(id,owner_id,display_name,history_retention_days) VALUES($1,$2,$1,7)",
        )
        .bind(&room)
        .bind(author)
        .execute(&pool)
        .await
        .unwrap();
        let session = Uuid::new_v4();
        let mut messages = vec![];
        for n in 0..6 {
            let mut message = entry(&format!("Message {n}"));
            message.participant_id = author.to_string();
            message.sent_at = (Utc::now() - chrono::Duration::seconds(100 - n)).to_rfc3339();
            if n == 1 {
                let first: &ChatEntry = &messages[0];
                message.reply_to = Some(crate::signaling::protocol::ChatReplyRef {
                    message_id: first.message_id.clone(),
                    participant_id: author.to_string(),
                    participant_name: "Author".into(),
                    excerpt: "Message 0".into(),
                });
            }
            persist_message(&pool, Some(&room), session, true, 7, &message)
                .await
                .unwrap();
            messages.push(message);
        }
        let conversation = public_conversation(&room);
        mark_read(
            &pool,
            &conversation,
            reader,
            messages[1].message_id.parse().unwrap(),
        )
        .await
        .unwrap();
        let resumed = page(
            &pool,
            &conversation,
            Some(reader),
            7,
            &HistoryQuery {
                resume: Some(true),
                limit: Some(4),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(
            resumed.first_unread_message_id.as_deref(),
            Some(messages[2].message_id.as_str())
        );
        assert_eq!(
            resumed
                .messages
                .iter()
                .map(|m| m.message_id.as_str())
                .collect::<Vec<_>>(),
            messages[..4]
                .iter()
                .map(|m| m.message_id.as_str())
                .collect::<Vec<_>>()
        );
        let newer = page(
            &pool,
            &conversation,
            Some(reader),
            7,
            &HistoryQuery {
                after: resumed.newer_cursor,
                limit: Some(4),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(newer.messages.len(), 2);
        assert_eq!(newer.messages[0].message_id, messages[4].message_id);
        assert!(
            page(
                &pool,
                &private_conversation(author, reader),
                Some(reader),
                90,
                &HistoryQuery {
                    around: Some(messages[0].message_id.clone()),
                    ..Default::default()
                }
            )
            .await
            .is_err()
        );
        let request = EditRequest {
            content: "Corrected original".into(),
            expected_revision: 0,
        };
        assert!(
            edit_saved(&pool, Some(&room), reader, None, &messages[0], &request)
                .await
                .is_err()
        );
        let edited = edit_saved(&pool, Some(&room), author, None, &messages[0], &request)
            .await
            .unwrap();
        assert_eq!(edited.revision, 1);
        assert_eq!(
            edit_saved(&pool, Some(&room), author, None, &messages[0], &request)
                .await
                .unwrap()
                .revision,
            1
        );
        let stored = page(&pool, &conversation, None, 7, &HistoryQuery::default())
            .await
            .unwrap();
        assert_eq!(
            stored.messages[1].reply_to.as_ref().unwrap().excerpt,
            "Corrected original"
        );
        // A retry of the original send returns its edited receipt, without redelivery or rollback.
        assert_eq!(
            persist_message(&pool, Some(&room), session, true, 7, &messages[0])
                .await
                .unwrap()
                .content,
            "Corrected original"
        );
        for message in messages.iter().take(3) {
            set_pin(&pool, &room, &message.message_id, true)
                .await
                .unwrap();
        }
        set_pin(&pool, &room, &messages[0].message_id, true)
            .await
            .unwrap();
        assert_eq!(pinned_messages(&pool, &room).await.unwrap().len(), 3);
        assert!(
            set_pin(&pool, &room, &messages[3].message_id, true)
                .await
                .is_err()
        );
        let mut tx = pool.begin().await.unwrap();
        remove_public(
            &mut tx,
            &room,
            &messages[0].message_id,
            &Utc::now().to_rfc3339(),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(pinned_messages(&pool, &room).await.unwrap().len(), 2);
        assert!(
            edit_saved(
                &pool,
                Some(&room),
                author,
                None,
                &edited,
                &EditRequest {
                    content: "Restore".into(),
                    expected_revision: 1
                }
            )
            .await
            .is_err()
        );
        assert!(
            set_pin(&pool, &room, &messages[0].message_id, true)
                .await
                .is_err()
        );
        let removed = page(&pool, &conversation, None, 7, &HistoryQuery::default())
            .await
            .unwrap();
        assert_eq!(
            removed.messages[1].reply_to.as_ref().unwrap().excerpt,
            "Message removed"
        );
        // An expired pin disappears on reads even before the retention sweep runs.
        sqlx::query("UPDATE chat_messages SET expires_at=now()-interval '1 second' WHERE id=$1")
            .bind(messages[2].message_id.parse::<Uuid>().unwrap())
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(pinned_messages(&pool, &room).await.unwrap().len(), 1);
        set_retention(&pool, &room, 0).await.unwrap();
        assert!(pinned_messages(&pool, &room).await.unwrap().is_empty());
        sqlx::query("DELETE FROM rooms WHERE id=$1")
            .bind(&room)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM users WHERE id=ANY($1)")
            .bind(vec![author, reader])
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;
    }

    #[tokio::test]
    #[ignore = "requires a migrated disposable TEST_DATABASE_URL"]
    async fn database_history_retention_pagination_private_isolation_and_monotonic_reads() {
        let pool = PgPool::connect(&std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL"))
            .await
            .unwrap();
        let alice = Uuid::new_v4();
        let bob = Uuid::new_v4();
        let stranger = Uuid::new_v4();
        for user in [alice, bob, stranger] {
            sqlx::query("INSERT INTO users(id,email,display_name) VALUES($1,$2,'History tester')")
                .bind(user)
                .bind(format!("{user}@history.invalid"))
                .execute(&pool)
                .await
                .unwrap();
        }
        let room = format!("history-{}", Uuid::new_v4());
        sqlx::query("INSERT INTO rooms(id,owner_id,display_name) VALUES($1,$2,'History room')")
            .bind(&room)
            .bind(alice)
            .execute(&pool)
            .await
            .unwrap();
        let retention: i32 =
            sqlx::query_scalar("SELECT history_retention_days::integer FROM rooms WHERE id=$1")
                .bind(&room)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(retention, 0, "existing/new rooms begin ephemeral");
        set_retention(&pool, &room, 7).await.unwrap();
        let session = Uuid::new_v4();
        let base = Utc::now() - chrono::Duration::seconds(100);
        let make = |n: i64, recipient: Option<Uuid>| ChatEntry {
            message_id: Uuid::new_v4().to_string(),
            client_message_id: format!("attempt-{n}"),
            participant_id: alice.to_string(),
            participant_name: "Alice".into(),
            recipient_id: recipient.map(|id| id.to_string()),
            recipient_name: recipient.map(|_| "Bob".into()),
            content: format!("Message needle%_{n}"),
            sent_at: (base + chrono::Duration::seconds(n)).to_rfc3339(),
            removed_at: None,
            revision: 0,
            edited_at: None,
            chat_style: Default::default(),
            reply_to: None,
            reactions: vec![],
        };
        let first = make(1, None);
        let second = make(2, None);
        let third = make(3, None);
        for message in [&first, &second, &third] {
            persist_message(&pool, Some(&room), session, true, 7, message)
                .await
                .unwrap();
        }
        let public = public_conversation(&room);
        let page_one = page(
            &pool,
            &public,
            Some(alice),
            7,
            &HistoryQuery {
                limit: Some(2),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(
            page_one
                .messages
                .iter()
                .map(|m| m.message_id.as_str())
                .collect::<Vec<_>>(),
            vec![second.message_id.as_str(), third.message_id.as_str()]
        );
        let page_two = page(
            &pool,
            &public,
            Some(alice),
            7,
            &HistoryQuery {
                before: page_one.next_cursor,
                limit: Some(2),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(page_two.messages.len(), 1);
        assert_eq!(page_two.messages[0].message_id, first.message_id);
        assert!(page_two.next_cursor.is_none());
        let found = page(
            &pool,
            &public,
            None,
            7,
            &HistoryQuery {
                q: Some("needle%_2".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(found.messages.len(), 1);
        assert_eq!(
            found.messages[0].message_id, second.message_id,
            "search treats SQL wildcard characters literally"
        );
        mark_read(&pool, &public, alice, third.message_id.parse().unwrap())
            .await
            .unwrap();
        let read = mark_read(&pool, &public, alice, first.message_id.parse().unwrap())
            .await
            .unwrap();
        assert_eq!(
            read.read_message_id, third.message_id,
            "older device cannot rewind read cursor"
        );
        assert!(
            mark_read(&pool, &public, alice, Uuid::new_v4())
                .await
                .is_err()
        );
        let private = make(4, Some(bob));
        let saved = persist_message(&pool, None, session, true, PM_RETENTION_DAYS, &private)
            .await
            .unwrap();
        let retried = persist_message(&pool, None, session, true, PM_RETENTION_DAYS, &private)
            .await
            .unwrap();
        assert_eq!(
            saved.message_id, retried.message_id,
            "duplicate durable attempt keeps original ID"
        );
        let mut conflict = private.clone();
        conflict.content = "different".into();
        assert!(
            persist_message(&pool, None, session, true, PM_RETENTION_DAYS, &conflict)
                .await
                .is_err()
        );
        for (user, preferences) in [
            (bob, serde_json::json!({"allowPrivateMessages":false})),
            (
                bob,
                serde_json::json!({"ignored":[{"id":alice.to_string(),"name":"Alice"}]}),
            ),
            (
                alice,
                serde_json::json!({"ignored":[{"id":bob.to_string(),"name":"Bob"}]}),
            ),
        ] {
            sqlx::query("UPDATE users SET preferences='{}'::jsonb WHERE id=ANY($1)")
                .bind(vec![alice, bob])
                .execute(&pool)
                .await
                .unwrap();
            sqlx::query("UPDATE users SET preferences=$2 WHERE id=$1")
                .bind(user)
                .bind(preferences)
                .execute(&pool)
                .await
                .unwrap();
            let rejected = persist_message(
                &pool,
                None,
                session,
                true,
                PM_RETENTION_DAYS,
                &make(5, Some(bob)),
            )
            .await;
            assert!(
                matches!(rejected, Err(sqlx::Error::InvalidArgument(_))),
                "room PM persistence observes the account preference saved by another device"
            );
        }
        sqlx::query("UPDATE users SET preferences='{}'::jsonb WHERE id=ANY($1)")
            .bind(vec![alice, bob])
            .execute(&pool)
            .await
            .unwrap();
        let private_page = page(
            &pool,
            &private_conversation(alice, bob),
            Some(bob),
            90,
            &HistoryQuery::default(),
        )
        .await
        .unwrap();
        assert_eq!(private_page.messages.len(), 1);
        assert!(
            page(
                &pool,
                &private_conversation(alice, stranger),
                Some(stranger),
                90,
                &HistoryQuery::default()
            )
            .await
            .unwrap()
            .messages
            .is_empty()
        );
        assert!(
            mark_read(
                &pool,
                &private_conversation(alice, stranger),
                stranger,
                private.message_id.parse().unwrap()
            )
            .await
            .is_err()
        );
        let inbox_rows: i64 =
            sqlx::query_scalar("SELECT count(*) FROM chat_inbox WHERE user_id=ANY($1)")
                .bind(vec![alice, bob])
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(
            inbox_rows, 2,
            "both accounts discover the conversation after reconnect/restart"
        );
        sqlx::query("UPDATE chat_messages SET expires_at=now()-interval '1 second' WHERE id=$1")
            .bind(first.message_id.parse::<Uuid>().unwrap())
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(
            page(&pool, &public, None, 7, &HistoryQuery::default())
                .await
                .unwrap()
                .messages
                .len(),
            2,
            "expired content is hidden before physical cleanup"
        );
        set_retention(&pool, &room, 0).await.unwrap();
        let remaining: i64 =
            sqlx::query_scalar("SELECT count(*) FROM chat_messages WHERE room_id=$1")
                .bind(&room)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(
            remaining, 0,
            "turning history off immediately erases public data"
        );
        let markers: i64 =
            sqlx::query_scalar("SELECT count(*) FROM chat_read_cursors WHERE conversation=$1")
                .bind(&public)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(markers, 0);
        assert_eq!(
            page(
                &pool,
                &private_conversation(alice, bob),
                Some(bob),
                90,
                &HistoryQuery::default()
            )
            .await
            .unwrap()
            .messages
            .len(),
            1,
            "public history settings never erase PMs"
        );
        sqlx::query("DELETE FROM rooms WHERE id=$1")
            .bind(&room)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM users WHERE id=ANY($1)")
            .bind(vec![alice, bob, stranger])
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;
    }
}
