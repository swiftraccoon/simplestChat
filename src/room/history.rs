//! Bounded, opt-in room history and account-only private conversations.
//!
//! Visibility is checked before querying: public history requires live room
//! membership; a PM conversation is always derived from the authenticated UUID.
//! Expiry is enforced by every read, independently of background deletion.
use crate::auth::{
    account, routes,
    types::{AuthError, Claims},
};
use crate::room::refusal::{Refusal, RoomError};
use crate::signaling::{SignalingServer, protocol::ChatEntry};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::HeaderMap,
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::{MessageId, ParticipantId, RoomId};
use sqlx::{PgConnection, PgPool, types::Json as SqlJson};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

pub(crate) const PM_RETENTION_DAYS: i32 = 90;
/// The retention periods a room may choose for its public history, in days (0 keeps none).
pub(crate) const ROOM_HISTORY_RETENTION_DAYS: [i32; 5] = [0, 1, 7, 30, 90];
const PAGE_LIMIT: usize = 50;
/// Search examines the newest 10,000 retained messages, then paginates matches.
const SEARCH_WINDOW: i64 = 10_000;
const CLEANUP_BATCH: i64 = 1_000;

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct HistoryQuery {
    pub before: Option<String>,
    pub after: Option<String>,
    pub around: Option<MessageId>,
    pub resume: Option<bool>,
    pub q: Option<String>,
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryPage {
    pub messages: Vec<ChatEntry>,
    pub next_cursor: Option<String>,
    pub newer_cursor: Option<String>,
    pub first_unread_message_id: Option<MessageId>,
    pub read_message_id: Option<MessageId>,
    pub retention_days: i32,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Conversation {
    pub peer_id: Uuid,
    pub peer_name: String,
    pub last_message: ChatEntry,
    /// At most 1,000: the UI should display the cap as 1,000+.
    pub unread_count: i64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InboxPage {
    pub conversations: Vec<Conversation>,
    pub next_cursor: Option<String>,
    pub retention_days: i32,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ReadRequest {
    pub message_id: Uuid,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReadResult {
    pub read_message_id: MessageId,
    /// Internal commit receipt; a repeated or older read has no visible change.
    #[serde(skip)]
    advanced: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct SendRequest {
    pub client_message_id: String,
    pub content: String,
    pub reply_to: Option<Uuid>,
    pub room_id: Option<String>,
    #[serde(default)]
    pub attachment_ids: Vec<Uuid>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct SetReactionRequest {
    pub emoji: String,
    pub present: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReactionResult {
    pub message_id: MessageId,
    pub reactions: Vec<crate::signaling::protocol::ChatReaction>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct EditRequest {
    pub content: String,
    pub expected_revision: u32,
}

/// Editing never changes identity, order, delivery receipts or conversation.
pub(crate) fn apply_edit(
    message: &mut ChatEntry,
    request: &EditRequest,
) -> Result<(), &'static str> {
    if (request.content.trim().is_empty() && message.attachments.is_empty())
        || request.content.len() > 4096
    {
        return Err("Message must contain 1–4096 bytes of text");
    }
    if !crate::labels::is_plain_text(&request.content) {
        return Err("Message contains hidden formatting characters");
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
    message.edited_at = Some(Utc::now());
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

/// Conversation identity is derived by the server, never supplied in a message
/// body. Return its capability from that authority on every durable read.
fn conversation_message(mut message: ChatEntry, account: bool) -> ChatEntry {
    message.account_conversation = account.then_some(true);
    message
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
        let Ok(preferences) = account::decode_chat_preferences(self.preferences.clone(), self.id)
        else {
            return false;
        };
        preferences.allow_private_messages
            && !preferences
                .ignored
                .iter()
                .any(|person| person.id == peer.to_string())
    }
}

/// Shared by room delivery and offline inbox sends. FOR SHARE keeps account
/// preference updates behind the message's commit; both accounts are always
/// locked in UUID order, before any session or message row is touched.
async fn lock_private_accounts(
    connection: &mut PgConnection,
    sender: Uuid,
    recipient: Uuid,
) -> Result<(ChatAccount, ChatAccount), RoomError> {
    if sender == recipient {
        return Err(Refusal::Unavailable("Private message unavailable".into()).into());
    }
    let people: Vec<ChatAccount> = sqlx::query_as(
        "SELECT id,display_name,preferences,chat_color,chat_style,auth_version FROM users WHERE id=ANY($1) ORDER BY id FOR SHARE")
        .bind(vec![sender, recipient]).fetch_all(connection).await?;
    let sender = people
        .iter()
        .find(|person| person.id == sender)
        .cloned()
        .ok_or_else(|| Refusal::Unavailable("Private message unavailable".into()))?;
    let recipient = people
        .iter()
        .find(|person| person.id == recipient)
        .cloned()
        .ok_or_else(|| Refusal::Unavailable("Private message unavailable".into()))?;
    Ok((sender, recipient))
}

fn private_accounts_allow(sender: &ChatAccount, recipient: &ChatAccount) -> bool {
    sender.allows_private_messages_from(recipient.id)
        && recipient.allows_private_messages_from(sender.id)
}

/// One account's private-messaging write. Both accounts and the session row
/// stay share-locked until the commit, so a logout, a credential reset or a
/// preference change lands after this write or refuses it, never between its
/// steps: the session is validated once, and the accepted claim's expiry
/// checked again at the commit.
struct PrivateWrite {
    tx: Transaction<'static, Postgres>,
    sender: ChatAccount,
    recipient: ChatAccount,
    conversation: String,
}

impl PrivateWrite {
    async fn begin(
        pool: &PgPool,
        claims: &Claims,
        own: Uuid,
        peer: Uuid,
        lock_error: impl FnOnce(RoomError) -> AuthError,
    ) -> Result<Self, AuthError> {
        let mut tx = pool.begin().await.map_err(routes::database_error)?;
        let (sender, recipient) = lock_private_accounts(&mut tx, own, peer)
            .await
            .map_err(lock_error)?;
        if sender.auth_version != claims.auth_version {
            return Err(AuthError::InvalidToken);
        }
        validate_private_session(&mut tx, claims, own).await?;
        Ok(Self {
            tx,
            sender,
            recipient,
            conversation: private_conversation(own, peer),
        })
    }

    /// Whether both accounts accept private messages from each other.
    fn allowed(&self) -> bool {
        private_accounts_allow(&self.sender, &self.recipient)
    }

    /// Serialize with the conversation's other writers.
    async fn lock_conversation(&mut self) -> Result<(), AuthError> {
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,29))")
            .bind(&self.conversation)
            .execute(&mut *self.tx)
            .await
            .map_err(routes::database_error)?;
        Ok(())
    }

    /// Commit while the accepted claim is still unexpired.
    async fn commit(self, claims: &Claims) -> Result<(), AuthError> {
        account::ensure_unexpired_claim(claims)?;
        self.tx.commit().await.map_err(routes::database_error)
    }
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
    attachment_ids: &[Uuid],
) -> Result<ChatEntry, RoomError> {
    let mut tx = pool.begin().await?;
    if let Some(recipient) = message.recipient_id {
        let sender = message.participant_id.as_uuid();
        let recipient = recipient.as_uuid();
        let (sender, recipient) = lock_private_accounts(&mut tx, sender, recipient).await?;
        if !sender_authenticated || !private_accounts_allow(&sender, &recipient) {
            return Err(Refusal::Unavailable("Private message unavailable".into()).into());
        }
    }
    let saved = insert_message(
        &mut tx,
        room,
        sender_session,
        sender_authenticated,
        retention_days,
        message,
        attachment_ids,
    )
    .await?;
    tx.commit().await?;
    if saved.message_id == message.message_id
        && let Some(recipient) = saved.recipient_id
    {
        crate::push::enqueue(pool, recipient.as_uuid()).await;
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
    attachment_ids: &[Uuid],
) -> Result<ChatEntry, RoomError> {
    let sender: Uuid = message.participant_id.as_uuid();
    let recipient = message.recipient_id.map(|id| id.as_uuid());
    let conversation = match (room, recipient) {
        (Some(room), None) => public_conversation(room),
        (None, Some(recipient)) if sender_authenticated => private_conversation(sender, recipient),
        _ => return Err(Refusal::Invalid("Invalid saved conversation".into()).into()),
    };
    // Serialize edits and quoted sends in one conversation so a reply cannot
    // commit a pre-edit excerpt after an edit has already updated older quotes.
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,29))")
        .bind(&conversation)
        .execute(&mut *connection)
        .await?;
    let mut message = conversation_message(message.clone(), recipient.is_some());
    let existing:Option<SqlJson<ChatEntry>>=sqlx::query_scalar("SELECT body FROM chat_messages WHERE conversation=$1 AND sender_session=$2 AND client_message_id=$3")
        .bind(&conversation).bind(sender_session).bind(&message.client_message_id).fetch_optional(&mut *connection).await?;
    if let Some(existing) = existing {
        if existing.0.removed_at.is_none()
            && (existing
                .0
                .attachments
                .iter()
                .map(|file| file.id)
                .collect::<Vec<_>>()
                != attachment_ids
                || (existing.0.edited_at.is_none()
                    && (existing.0.content != message.content
                        || existing.0.reply_to.as_ref().map(|reply| &reply.message_id)
                            != message.reply_to.as_ref().map(|reply| &reply.message_id))))
        {
            return Err(Refusal::Conflict("Message ID was already used".into()).into());
        }
        return Ok(conversation_message(existing.0, recipient.is_some()));
    }
    if !attachment_ids.is_empty() && !sender_authenticated {
        return Err(Refusal::Forbidden("Sign in to attach files".into()).into());
    }
    message.attachments =
        crate::attachments::pending_metadata(connection, sender, attachment_ids).await?;
    if let Some(reply) = &mut message.reply_to {
        let original: Option<SqlJson<ChatEntry>> = sqlx::query_scalar(
            "SELECT body FROM chat_messages WHERE conversation=$1 AND id=$2 AND expires_at>now()",
        )
        .bind(&conversation)
        .bind(reply.message_id.as_uuid())
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
    let id: Uuid = message.message_id.as_uuid();
    let at = message.sent_at;
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
        crate::attachments::bind_durable(connection, attachment_ids, id, room, expires).await?;
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
            return Err(Refusal::Conflict("Message ID was already used".into()).into());
        }
        return Ok(conversation_message(saved.0, recipient.is_some()));
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
) -> Result<HistoryPage, RoomError> {
    query
        .validate()
        .map_err(|message| Refusal::Invalid(message.into()))?;
    let cursor = decode_cursor(query.before.as_deref().or(query.after.as_deref()))
        .map_err(|message| Refusal::Invalid(message.into()))?;
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
    let anchor = query.around.map(|id| id.as_uuid()).or_else(|| {
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
        messages: rows
            .into_iter()
            .map(|(_, _, entry)| conversation_message(entry.0, conversation.starts_with("pm:")))
            .collect(),
        next_cursor,
        newer_cursor,
        first_unread_message_id: first_unread.map(MessageId::from),
        read_message_id: read.map(|(id, _)| MessageId::from(id)),
        retention_days,
    })
}

pub(crate) async fn mark_read(
    pool: &PgPool,
    conversation: &str,
    account: Uuid,
    message: Uuid,
) -> Result<ReadResult, RoomError> {
    let mut transaction = pool.begin().await?;
    let result: Option<Uuid> = sqlx::query_scalar(
        "INSERT INTO chat_read_cursors (user_id,conversation,message_id,sent_at,expires_at)
         SELECT $1,conversation,id,sent_at,expires_at FROM chat_messages
         WHERE conversation=$2 AND id=$3 AND expires_at>now()
         ON CONFLICT (user_id,conversation) DO UPDATE SET
           message_id=EXCLUDED.message_id,sent_at=EXCLUDED.sent_at,
           expires_at=GREATEST(chat_read_cursors.expires_at,EXCLUDED.expires_at)
         WHERE (chat_read_cursors.sent_at,chat_read_cursors.message_id)<(EXCLUDED.sent_at,EXCLUDED.message_id)
         RETURNING message_id")
        .bind(account).bind(conversation).bind(message).fetch_optional(&mut *transaction).await?;
    // A rejected ON CONFLICT update still locks the existing cursor. Keep that
    // lock through this fresh read and COMMIT so concurrent devices receive the
    // authoritative cursor without reporting an unchanged row as a new event.
    let read_message_id = match result {
        Some(id) => id,
        None => {
            let (id, cursor_expiry, requested_expiry): (Uuid, DateTime<Utc>, DateTime<Utc>) =
                sqlx::query_as(
                    "SELECT r.message_id,r.expires_at,m.expires_at FROM chat_read_cursors r
                     JOIN chat_messages m ON m.conversation=r.conversation
                     WHERE r.user_id=$1 AND r.conversation=$2 AND m.id=$3 AND m.expires_at>now()",
                )
                .bind(account)
                .bind(conversation)
                .bind(message)
                .fetch_one(&mut *transaction)
                .await?;
            // A retained older message may have a longer expiry. Preserve the
            // original lifetime maximum without calling it a cursor advance.
            if requested_expiry > cursor_expiry {
                sqlx::query(
                    "UPDATE chat_read_cursors SET expires_at=$3 WHERE user_id=$1 AND conversation=$2",
                )
                .bind(account)
                .bind(conversation)
                .bind(requested_expiry)
                .execute(&mut *transaction)
                .await?;
            }
            id
        }
    };
    transaction.commit().await?;
    Ok(ReadResult {
        read_message_id: MessageId::from(read_message_id),
        advanced: result.is_some(),
    })
}

pub(crate) async fn save_reactions(
    pool: &PgPool,
    id: &MessageId,
    reactions: &[crate::signaling::protocol::ChatReaction],
) -> Result<(), RoomError> {
    let id: Uuid = id.as_uuid();
    sqlx::query("UPDATE chat_messages SET body=jsonb_set(body,'{reactions}',$2) WHERE id=$1 AND expires_at>now() AND body->>'removedAt' IS NULL")
        .bind(id).bind(SqlJson(reactions)).execute(pool).await?;
    Ok(())
}

/// Toggle against the authoritative retained public row, serialized with edits,
/// quoted sends and removal. The admitted caller supplies its current ignore
/// list; private conversations and expired/removed rows cannot enter this path.
pub(crate) async fn react_public(
    pool: &PgPool,
    room: &str,
    id: &MessageId,
    participant: &ParticipantId,
    ignored: &[ParticipantId],
    emoji: &str,
) -> Result<Option<ChatEntry>, RoomError> {
    let id: Uuid = id.as_uuid();
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,29))")
        .bind(public_conversation(room))
        .execute(&mut *tx)
        .await?;
    let saved: Option<SqlJson<ChatEntry>> = sqlx::query_scalar(
        "SELECT body FROM chat_messages WHERE room_id=$1 AND id=$2 AND expires_at>now()
          AND body->>'removedAt' IS NULL AND NOT (body->>'participantId'=ANY($3)) FOR UPDATE",
    )
    .bind(room)
    .bind(id)
    .bind(ignored.iter().map(ToString::to_string).collect::<Vec<_>>())
    .fetch_optional(&mut *tx)
    .await?;
    let Some(SqlJson(mut message)) = saved else {
        return Ok(None);
    };
    message.reactions = super::social::toggle_reactions(&message.reactions, participant, emoji)
        .map_err(|message| Refusal::Invalid(message.into()))?;
    sqlx::query("UPDATE chat_messages SET body=jsonb_set(body,'{reactions}',$2) WHERE id=$1")
        .bind(id)
        .bind(SqlJson(&message.reactions))
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(Some(message))
}

pub(crate) async fn lookup_public(
    pool: &PgPool,
    room: &str,
    id: &MessageId,
) -> Result<Option<(ChatEntry, bool)>, RoomError> {
    let id: Uuid = id.as_uuid();
    let row: Option<(SqlJson<ChatEntry>,bool)> = sqlx::query_as(
        "SELECT body,sender_account IS NOT NULL FROM chat_messages WHERE room_id=$1 AND id=$2 AND expires_at>now()")
        .bind(room).bind(id).fetch_optional(pool).await?;
    Ok(row.map(|(message, authenticated)| (message.0, authenticated)))
}

/// Call with the same transaction that records the moderation decision.
pub(crate) async fn remove_public(
    connection: &mut PgConnection,
    room: &str,
    id: &MessageId,
    removed_at: DateTime<Utc>,
) -> Result<Option<(ChatEntry, bool)>, RoomError> {
    let id: Uuid = id.as_uuid();
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,29))")
        .bind(public_conversation(room))
        .execute(&mut *connection)
        .await?;
    let row: Option<(SqlJson<ChatEntry>,bool)> = sqlx::query_as(
        "UPDATE chat_messages SET body=(body-'replyTo'-'attachments') || jsonb_build_object('content','','reactions','[]'::jsonb,'removedAt',COALESCE(body->>'removedAt',$3))
         WHERE room_id=$1 AND id=$2 AND expires_at>now() RETURNING body,sender_account IS NOT NULL")
        .bind(room).bind(id).bind(removed_at.to_rfc3339()).fetch_optional(&mut *connection).await?;
    sqlx::query("DELETE FROM attachments WHERE message_id=$1 AND room_id=$2")
        .bind(id)
        .bind(room)
        .execute(&mut *connection)
        .await?;
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
) -> Result<ChatEntry, RoomError> {
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
    let mut edited = conversation_message(saved.0, conversation.starts_with("pm:"));
    let old_revision = edited.revision;
    apply_edit(&mut edited, request).map_err(|message| Refusal::Invalid(message.into()))?;
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
) -> Result<ChatEntry, RoomError> {
    let mut tx = pool.begin().await?;
    let conversation = if let Some(peer) = original.recipient_id {
        let peer = peer.as_uuid();
        let (sender, recipient) = lock_private_accounts(&mut tx, actor, peer).await?;
        if guest_session.is_some() || !private_accounts_allow(&sender, &recipient) {
            return Err(Refusal::Unavailable("Private message unavailable".into()).into());
        }
        private_conversation(actor, peer)
    } else {
        public_conversation(room.ok_or_else(|| Refusal::Unavailable("Room unavailable".into()))?)
    };
    let message = original.message_id.as_uuid();
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

pub(crate) async fn edit_message(
    State(server): State<SignalingServer>,
    headers: HeaderMap,
    Path((peer, message)): Path<(Uuid, Uuid)>,
    Json(request): Json<EditRequest>,
) -> Result<(HeaderMap, Json<ChatEntry>), AuthError> {
    let permit = routes::acquire_account_request(&server)?;
    let claims = account::authenticated_claims(&server, &headers).await?;
    let saved = tokio::spawn(async move {
        let _permit = permit;
        let saved = tokio::time::timeout(
            std::time::Duration::from_secs(15),
            save_private_edit(&server, &claims, peer, message, request),
        )
        .await
        .map_err(|_| AuthError::ServiceBusy)??;
        server.room_manager().deliver_inbox_edit(&saved).await;
        Ok::<_, AuthError>(saved)
    })
    .await
    .map_err(|_| AuthError::ServiceBusy)??;
    Ok((routes::no_store_headers(), Json(saved)))
}

async fn save_private_edit(
    server: &SignalingServer,
    claims: &Claims,
    peer: Uuid,
    message: Uuid,
    request: EditRequest,
) -> Result<ChatEntry, AuthError> {
    account::ensure_unexpired_claim(claims)?;
    let own: Uuid = claims.sub.parse().map_err(|_| AuthError::InvalidToken)?;
    let pool = server.db_pool().ok_or(AuthError::NotConfigured)?;
    let mut write = PrivateWrite::begin(pool, claims, own, peer, edit_error).await?;
    if !write.allowed() {
        return Err(AuthError::InvalidInput("Private message unavailable"));
    }
    write.lock_conversation().await?;
    let edited = edit_in_transaction(
        &mut write.tx,
        &write.conversation,
        own,
        None,
        message,
        &request,
    )
    .await
    .map_err(edit_error)?;
    write.commit(claims).await?;
    Ok(edited)
}

/// An HTTP route answers a refusal with its own status and a database failure
/// with the shared account-service response.
fn route_error(error: RoomError) -> AuthError {
    match error {
        RoomError::Refused(refusal) => AuthError::Refused(refusal),
        RoomError::Database(error) => routes::database_error(error),
    }
}

fn edit_error(error: impl Into<RoomError>) -> AuthError {
    match error.into() {
        RoomError::Database(sqlx::Error::RowNotFound) => {
            AuthError::InvalidInput("Message unavailable")
        }
        RoomError::Refused(_) => {
            AuthError::InvalidInput("Message unavailable or changed; refresh before editing")
        }
        RoomError::Database(other) => routes::database_error(other),
    }
}

/// Latest durable account message for owned cross-room reconciliation. Neither
/// an arbitrary room ID nor a client-provided message body selects its scope.
pub(crate) async fn private_message(
    pool: &PgPool,
    own: Uuid,
    peer: Uuid,
    message: Uuid,
) -> Result<Option<ChatEntry>, RoomError> {
    let saved: Option<SqlJson<ChatEntry>> = sqlx::query_scalar(
        "SELECT body FROM chat_messages WHERE conversation=$1 AND id=$2 AND expires_at>now() AND body->>'removedAt' IS NULL",
    ).bind(private_conversation(own, peer)).bind(message).fetch_optional(pool).await?;
    Ok(saved.map(|saved| conversation_message(saved.0, true)))
}

async fn set_private_reaction_in_transaction(
    connection: &mut PgConnection,
    conversation: &str,
    message: Uuid,
    actor: &ParticipantId,
    request: &SetReactionRequest,
) -> Result<ChatEntry, RoomError> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,29))")
        .bind(conversation)
        .execute(&mut *connection)
        .await?;
    let saved: SqlJson<ChatEntry> = sqlx::query_scalar(
        "SELECT body FROM chat_messages WHERE conversation=$1 AND id=$2 AND expires_at>now() AND body->>'removedAt' IS NULL FOR UPDATE",
    ).bind(conversation).bind(message).fetch_one(&mut *connection).await?;
    let mut message = conversation_message(saved.0, true);
    message.reactions =
        super::social::set_reaction(&message.reactions, actor, &request.emoji, request.present)
            .map_err(|message| Refusal::Invalid(message.into()))?;
    sqlx::query("UPDATE chat_messages SET body=jsonb_set(body,'{reactions}',$2) WHERE id=$1")
        .bind(message.message_id.as_uuid())
        .bind(SqlJson(&message.reactions))
        .execute(&mut *connection)
        .await?;
    Ok(message)
}

/// Explicit presence makes repeated requests safe after a lost response. Current
/// account consent/session authority is held through the transaction commit.
pub(crate) async fn set_private_reaction(
    State(server): State<SignalingServer>,
    headers: HeaderMap,
    Path((peer, message)): Path<(Uuid, Uuid)>,
    Json(request): Json<SetReactionRequest>,
) -> Result<(HeaderMap, Json<ReactionResult>), AuthError> {
    let permit = routes::acquire_account_request(&server)?;
    let claims = account::authenticated_claims(&server, &headers).await?;
    let saved = tokio::spawn(async move {
        let _permit = permit;
        let saved = tokio::time::timeout(
            std::time::Duration::from_secs(15),
            save_private_reaction(&server, &claims, peer, message, request),
        )
        .await
        .map_err(|_| AuthError::ServiceBusy)??;
        server.room_manager().deliver_inbox_reactions(&saved).await;
        Ok::<_, AuthError>(saved)
    })
    .await
    .map_err(|_| AuthError::ServiceBusy)??;
    Ok((
        routes::no_store_headers(),
        Json(ReactionResult {
            message_id: saved.message_id,
            reactions: saved.reactions,
        }),
    ))
}

async fn save_private_reaction(
    server: &SignalingServer,
    claims: &Claims,
    peer: Uuid,
    message: Uuid,
    request: SetReactionRequest,
) -> Result<ChatEntry, AuthError> {
    account::ensure_unexpired_claim(claims)?;
    let own: Uuid = claims.sub.parse().map_err(|_| AuthError::InvalidToken)?;
    if own == peer || !crate::signaling::protocol::REACTIONS.contains(&request.emoji.as_str()) {
        return Err(AuthError::InvalidInput("Private reaction unavailable"));
    }
    let pool = server.db_pool().ok_or(AuthError::NotConfigured)?;
    let mut write = PrivateWrite::begin(pool, claims, own, peer, edit_error).await?;
    if !write.allowed() {
        return Err(AuthError::InvalidInput("Private reaction unavailable"));
    }
    write.lock_conversation().await?;
    let saved = set_private_reaction_in_transaction(
        &mut write.tx,
        &write.conversation,
        message,
        &ParticipantId::from(own),
        &request,
    )
    .await
    .map_err(edit_error)?;
    write.commit(claims).await?;
    Ok(saved)
}

pub(crate) async fn pinned_messages(
    pool: &PgPool,
    room: &str,
) -> Result<Vec<ChatEntry>, RoomError> {
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
    message: &MessageId,
    pinned: bool,
) -> Result<(), RoomError> {
    let message: Uuid = message.as_uuid();
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
            return Err(sqlx::Error::RowNotFound.into());
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
                Refusal::Conflict(
                    "Unpin a message before pinning another; rooms keep up to three".into(),
                )
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
    Ok(tx.commit().await?)
}

/// Public history stops being readable and is erased in the settings transaction.
pub(crate) async fn set_retention(pool: &PgPool, room: &str, days: i32) -> Result<(), RoomError> {
    if !ROOM_HISTORY_RETENTION_DAYS.contains(&days) {
        return Err(Refusal::Invalid("Invalid history retention".into()).into());
    }
    let mut tx = pool.begin().await?;
    let updated =
        sqlx::query("UPDATE rooms SET history_retention_days=$2,updated_at=now() WHERE id=$1")
            .bind(room)
            .bind(i64::from(days))
            .execute(&mut *tx)
            .await?;
    if updated.rows_affected() != 1 {
        return Err(sqlx::Error::RowNotFound.into());
    }
    sqlx::query("DELETE FROM chat_messages WHERE room_id=$1 AND ($2=0 OR sent_at<=now()-make_interval(days=>$2))")
        .bind(room).bind(days).execute(&mut *tx).await?;
    // Extending retention affects future messages only. Never extend a promise
    // already made when a message was saved under a shorter retention period.
    sqlx::query("UPDATE chat_messages SET expires_at=LEAST(expires_at,sent_at+make_interval(days=>$2)) WHERE room_id=$1")
        .bind(room).bind(days).execute(&mut *tx).await?;
    sqlx::query("UPDATE attachments a SET expires_at=LEAST(a.expires_at,m.expires_at) FROM chat_messages m WHERE a.message_id=m.id AND m.room_id=$1")
        .bind(room).execute(&mut *tx).await?;
    if days == 0 {
        sqlx::query("DELETE FROM chat_read_cursors WHERE conversation=$1")
            .bind(public_conversation(room))
            .execute(&mut *tx)
            .await?;
    }
    Ok(tx.commit().await?)
}

pub(crate) async fn inbox(
    State(server): State<SignalingServer>,
    headers: HeaderMap,
    Query(query): Query<HistoryQuery>,
) -> Result<(HeaderMap, Json<InboxPage>), AuthError> {
    let _permit = routes::acquire_account_request(&server)?;
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
                        last_message: conversation_message(message.0, true),
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
pub(crate) struct UnreadSummary {
    pub unread_count: i64,
}

pub(crate) async fn unread_summary(
    State(server): State<SignalingServer>,
    headers: HeaderMap,
) -> Result<(HeaderMap, Json<UnreadSummary>), AuthError> {
    let _permit = routes::acquire_account_request(&server)?;
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

pub(crate) async fn messages(
    State(server): State<SignalingServer>,
    headers: HeaderMap,
    Path(peer): Path<Uuid>,
    Query(query): Query<HistoryQuery>,
) -> Result<(HeaderMap, Json<HistoryPage>), AuthError> {
    let _permit = routes::acquire_account_request(&server)?;
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
    .map_err(route_error)?;
    Ok((routes::no_store_headers(), Json(result)))
}

pub(crate) async fn read(
    State(server): State<SignalingServer>,
    headers: HeaderMap,
    Path(peer): Path<Uuid>,
    Json(request): Json<ReadRequest>,
) -> Result<(HeaderMap, Json<ReadResult>), AuthError> {
    let permit = routes::acquire_account_request(&server)?;
    let claims = account::authenticated_claims(&server, &headers).await?;
    let own: Uuid = claims.sub.parse().map_err(|_| AuthError::InvalidToken)?;
    let result = crate::auth::updates::owned(permit, async move {
        let pool = server.db_pool().ok_or(AuthError::NotConfigured)?;
        let result = mark_read(
            pool,
            &private_conversation(own, peer),
            own,
            request.message_id,
        )
        .await
        .map_err(|error| match error {
            RoomError::Database(sqlx::Error::RowNotFound) | RoomError::Refused(_) => {
                AuthError::InvalidInput("Message unavailable")
            }
            RoomError::Database(other) => routes::database_error(other),
        })?;
        if result.advanced {
            server.room_manager().account_updates().changed(
                own,
                crate::auth::updates::Scope::Inbox,
                Some(peer),
            );
        }
        Ok(result)
    })
    .await?;
    Ok((routes::no_store_headers(), Json(result)))
}

pub(crate) async fn send_message(
    State(server): State<SignalingServer>,
    headers: HeaderMap,
    Path(peer): Path<Uuid>,
    Json(request): Json<SendRequest>,
) -> Result<(HeaderMap, Json<ChatEntry>), AuthError> {
    let permit = routes::acquire_account_request(&server)?;
    let claims = account::authenticated_claims(&server, &headers).await?;
    let saved = tokio::spawn(async move {
        let _permit = permit;
        let (saved, created) = tokio::time::timeout(
            std::time::Duration::from_secs(15),
            save_private_message(&server, &claims, peer, request),
        )
        .await
        .map_err(|_| AuthError::ServiceBusy)??;
        if created {
            server.room_manager().deliver_inbox_message(&saved);
            if let Some(pool) = server.db_pool() {
                crate::push::enqueue(pool, peer).await;
            }
        }
        Ok::<_, AuthError>(saved)
    })
    .await
    .map_err(|_| AuthError::ServiceBusy)??;
    Ok((routes::no_store_headers(), Json(saved)))
}

async fn private_relationship(
    connection: &mut PgConnection,
    own: Uuid,
    peer: Uuid,
) -> Result<bool, RoomError> {
    let conversation: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM chat_inbox WHERE user_id=$1 AND peer_id=$2 AND expires_at>now())")
        .bind(own).bind(peer).fetch_one(&mut *connection).await?;
    Ok(conversation || crate::auth::discovery::accepted_contact(connection, own, peer).await?)
}

async fn validate_private_session(
    connection: &mut PgConnection,
    claims: &Claims,
    own: Uuid,
) -> Result<(), AuthError> {
    account::ensure_unexpired_claim(claims)?;
    let current: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM sessions WHERE id=$1 AND user_id=$2 AND expires_at>clock_timestamp() FOR SHARE)")
        .bind(claims.sid).bind(own).fetch_one(connection).await.map_err(routes::database_error)?;
    if !current {
        return Err(AuthError::InvalidToken);
    }
    account::ensure_unexpired_claim(claims)
}

pub(super) async fn save_private_message(
    server: &SignalingServer,
    claims: &Claims,
    peer: Uuid,
    request: SendRequest,
) -> Result<(ChatEntry, bool), AuthError> {
    account::ensure_unexpired_claim(claims)?;
    let own: Uuid = claims.sub.parse().map_err(|_| AuthError::InvalidToken)?;
    if own == peer
        || !crate::signaling::protocol::valid_correlation_id(&request.client_message_id)
        || (request.content.trim().is_empty() && request.attachment_ids.is_empty())
        || !crate::attachments::valid_ids(&request.attachment_ids)
        || request.content.len() > 4096
        || !crate::labels::is_plain_text(&request.content)
    {
        return Err(AuthError::InvalidInput("Invalid private message"));
    }
    let pool = server.db_pool().ok_or(AuthError::NotConfigured)?;
    // Read before taking any row lock. A first message acquires room control
    // before users/sessions, matching room sends and avoiding a lock inversion.
    let context = if let Some(id) = &request.room_id {
        let related = {
            let mut connection = pool.acquire().await.map_err(routes::database_error)?;
            private_relationship(&mut connection, own, peer)
                .await
                .map_err(route_error)?
        };
        if related {
            None
        } else {
            Some(
                server
                    .room_manager()
                    .private_creation_context(
                        &RoomId::new(id.as_str())
                            .map_err(|_| AuthError::InvalidInput("Invalid room"))?,
                        own,
                        peer,
                    )
                    .await?,
            )
        }
    } else {
        None
    };
    let mut write = PrivateWrite::begin(pool, claims, own, peer, |error| match error {
        RoomError::Refused(_) => AuthError::InvalidInput("Private message unavailable"),
        RoomError::Database(error) => routes::database_error(error),
    })
    .await?;
    if context.is_none()
        && !private_relationship(&mut write.tx, own, peer)
            .await
            .map_err(route_error)?
    {
        return Err(AuthError::InvalidInput(
            "Add this person as a contact or start the conversation in a room",
        ));
    }
    // One account's inbox writes serialize with each other, bounding a burst
    // across multiple sessions. Retries do not consume a second send slot.
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1::text,17))")
        .bind(own.to_string())
        .execute(&mut *write.tx)
        .await
        .map_err(routes::database_error)?;
    write.lock_conversation().await?;
    let existing:Option<SqlJson<ChatEntry>>=sqlx::query_scalar("SELECT body FROM chat_messages WHERE conversation=$1 AND sender_session=$2 AND client_message_id=$3 AND expires_at>now()")
        .bind(&write.conversation).bind(own).bind(&request.client_message_id).fetch_optional(&mut *write.tx).await.map_err(routes::database_error)?;
    if let Some(existing) = existing {
        if (existing.0.edited_at.is_none() && existing.0.content != request.content)
            || existing.0.reply_to.as_ref().map(|reply| reply.message_id)
                != request.reply_to.map(MessageId::from)
            || existing
                .0
                .attachments
                .iter()
                .map(|file| file.id)
                .collect::<Vec<_>>()
                != request.attachment_ids
        {
            return Err(AuthError::InvalidInput("Message ID was already used"));
        }
        return Ok((conversation_message(existing.0, true), false));
    }
    if !write.allowed() {
        return Err(AuthError::InvalidInput("Private message unavailable"));
    }
    let recent:i64=sqlx::query_scalar("SELECT count(*) FROM (SELECT 1 FROM chat_messages WHERE sender_account=$1 AND sender_session=$1 AND recipient_account IS NOT NULL AND sent_at>now()-interval '1 minute' LIMIT 30) recent")
        .bind(own).fetch_one(&mut *write.tx).await.map_err(routes::database_error)?;
    if recent >= 30 {
        return Err(AuthError::RateLimited);
    }
    let reply_to = if let Some(id) = request.reply_to {
        let original: SqlJson<ChatEntry> = sqlx::query_scalar(
            "SELECT body FROM chat_messages WHERE conversation=$1 AND id=$2 AND expires_at>now() AND body->>'removedAt' IS NULL",
        ).bind(&write.conversation).bind(id).fetch_one(&mut *write.tx).await.map_err(edit_error)?;
        Some(crate::signaling::protocol::ChatReplyRef {
            message_id: MessageId::from(id),
            participant_id: original.0.participant_id,
            participant_name: original.0.participant_name,
            excerpt: quote_excerpt(&original.0.content),
        })
    } else {
        None
    };
    let style = crate::signaling::protocol::ChatStyle {
        color: write.sender.chat_color.clone(),
        style: crate::signaling::protocol::ChatStyleKind::named(&write.sender.chat_style)
            .unwrap_or_default(),
    };
    let message = ChatEntry {
        message_id: MessageId::random(),
        client_message_id: request.client_message_id,
        participant_id: ParticipantId::from(own),
        participant_name: write.sender.display_name.clone(),
        recipient_id: Some(ParticipantId::from(peer)),
        account_conversation: Some(true),
        recipient_name: Some(write.recipient.display_name.clone()),
        content: request.content,
        attachments: Vec::new(),
        sent_at: Utc::now(),
        removed_at: None,
        revision: 0,
        edited_at: None,
        chat_style: style,
        reply_to,
        reactions: Vec::new(),
    };
    let saved = insert_message(
        &mut write.tx,
        None,
        own,
        true,
        PM_RETENTION_DAYS,
        &message,
        &request.attachment_ids,
    )
    .await
    .map_err(route_error)?;
    write.commit(claims).await?;
    Ok((saved, true))
}

impl super::RoomManager {
    /// First-account-PM authority comes from a concrete admitted room context.
    /// Retaining its gate keeps membership and room chat permissions stable
    /// while the caller subsequently acquires users, session and message locks.
    async fn private_creation_context(
        &self,
        id: &RoomId,
        own: Uuid,
        peer: Uuid,
    ) -> Result<tokio::sync::OwnedMutexGuard<()>, AuthError> {
        let unavailable = || AuthError::InvalidInput("Private message unavailable");
        let room = self.get_room(id).map_err(|_| unavailable())?;
        let control = tokio::time::timeout(
            super::control::ADMISSION_TIMEOUT,
            super::control::lock_room(&room),
        )
        .await
        .map_err(|_| AuthError::ServiceBusy)?;
        let state = room.read().await;
        if state.ensure_live().is_err()
            || self.drain.is_draining()
            || !self
                .rooms
                .read()
                .get(id)
                .is_some_and(|current| std::sync::Arc::ptr_eq(current, &room))
        {
            return Err(unavailable());
        }
        if !super::social::can_start_account_conversation(
            &state,
            &ParticipantId::from(own),
            &ParticipantId::from(peer),
        ) {
            return Err(unavailable());
        }
        drop(state);
        Ok(control)
    }

    /// Inbox storage owns delivery; live room sockets get a bounded notification
    /// as well. A busy/disconnected socket reconciles from its durable inbox.
    pub(crate) fn deliver_inbox_message(&self, message: &ChatEntry) {
        self.account_updates().private_message(message);
        let Some(recipient) = message.recipient_id else {
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
        let rooms: Vec<_> = self.rooms.read().values().cloned().collect();
        for lock in rooms {
            let Ok(room) = lock.try_read() else {
                continue;
            };
            if room.deleting {
                continue;
            }
            for participant in room.participants.values().filter(|person| {
                person.authenticated
                    && (person.id == recipient || person.id == message.participant_id)
            }) {
                let peer = if participant.id == recipient {
                    message.participant_id
                } else {
                    recipient
                };
                if participant.social.accepts_inbox_from(&peer) {
                    let _ = super::try_send_essential(
                        &self.metrics,
                        &participant.sender,
                        event.clone(),
                    );
                }
            }
        }
    }
}

/// Physically erase expired data in bounded batches; read predicates already
/// make expiry exact even while a large cleanup backlog remains.
pub(crate) fn spawn_retention(pool: PgPool) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(300));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            let result=tokio::time::timeout(std::time::Duration::from_secs(15), async {
                for _ in 0..16 {
                    let attachments=crate::attachments::cleanup(&pool).await?;
                    let messages = sqlx::query("DELETE FROM chat_messages WHERE id IN (SELECT id FROM chat_messages WHERE expires_at<=now() ORDER BY expires_at LIMIT $1)")
                        .bind(CLEANUP_BATCH).execute(&pool).await?.rows_affected();
                    let inbox = sqlx::query("DELETE FROM chat_inbox WHERE (user_id,peer_id) IN (SELECT user_id,peer_id FROM chat_inbox WHERE expires_at<=now() ORDER BY expires_at LIMIT $1)")
                        .bind(CLEANUP_BATCH).execute(&pool).await?.rows_affected();
                    let cursors = sqlx::query("DELETE FROM chat_read_cursors WHERE (user_id,conversation) IN (SELECT user_id,conversation FROM chat_read_cursors WHERE expires_at<=now() ORDER BY expires_at LIMIT $1)")
                        .bind(CLEANUP_BATCH).execute(&pool).await?.rows_affected();
                    if attachments < CLEANUP_BATCH as u64 && messages < CLEANUP_BATCH as u64 && inbox < CLEANUP_BATCH as u64 && cursors < CLEANUP_BATCH as u64 { break; }
                }
                Ok::<_,RoomError>(())
            }).await;
            if !matches!(result, Ok(Ok(()))) {
                tracing::warn!("Chat history cleanup incomplete; the next sweep retries");
            }
        }
    })
}

#[cfg(test)]
#[path = "history_tests.rs"]
mod tests;
