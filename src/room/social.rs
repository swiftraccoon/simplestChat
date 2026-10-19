//! Room-session chat and owner/moderator community tools.
//! Private text stays out of reports and logs; account PM retention lives in `history`.
use super::*;
use crate::ids::{MessageId, ParticipantId, RoomId, UserId};
use crate::room::moderation::{
    Actor, MAX_MODERATION_EVENTS, ModerationAction, ModerationEvent, ModerationTargetId,
    ReportStatus, Target, record_event,
};
use crate::signaling::protocol::{
    BanEntry, ChatEntry, ChatReaction, ChatReplyRef, ChatRetryOutcome, ChatRetryReason, ChatStyle,
    ChatStyleKind, ClientMessage, ErrorCode, MemberEntry, ModerationEventView, REACTIONS,
    ReportEntry, ReportOutcome, RoomSnapshot, SnapshotLobbyEntry, SocialResponse,
    valid_correlation_id,
};
use uuid::Uuid;

const HISTORY_MESSAGES: usize = 300;
/// Reaction records one message keeps, so a popular message stays within budget.
const MESSAGE_REACTIONS: usize = 64;
/// Characters a reply quotes from the message it answers.
const REPLY_EXCERPT_CHARS: usize = 140;
const HISTORY_BYTES: usize = 256 * 1024;
const MAX_IGNORED: usize = 100;
const MAX_REPORTS: usize = 500;
const MAX_RUNTIME_BANS: usize = 2000;
/// History entries an unpersisted room keeps in memory.
const MAX_RUNTIME_MODERATION_EVENTS: usize = 200;
const PAGE_SIZE: usize = 100;
/// One typing notice per sender per conversation this often, at most.
const TYPING_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);
const CHAT_RECEIPT_TTL: std::time::Duration = std::time::Duration::from_secs(300);
const CHAT_RECEIPTS: usize = 512;
const CHAT_RECEIPT_BYTES: usize = 512 * 1024;
const CHAT_RECEIPTS_PER_MEMBER: usize = 128;
const MAX_CHAT_SEQUENCE: u64 = 9_007_199_254_740_991;

type BanRow = (
    Uuid,
    Option<String>,
    bool,
    Option<String>,
    Option<chrono::DateTime<chrono::Utc>>,
);
type ReportRow = (
    Uuid,
    ParticipantId,
    String,
    ParticipantId,
    String,
    String,
    ReportStatus,
    chrono::DateTime<chrono::Utc>,
    Option<chrono::DateTime<chrono::Utc>>,
    Option<String>,
    Option<chrono::DateTime<chrono::Utc>>,
);

#[derive(Clone)]
pub(crate) struct ParticipantSocial {
    pub(super) connected: bool,
    chat_session_id: Uuid,
    chat_high_water: u64,
    joined_sequence: u64,
    allow_private_messages: bool,
    ignored: HashSet<ParticipantId>,
    guest_ignored: HashSet<ParticipantId>,
    last_typing: Option<std::time::Instant>,
}

#[cfg(test)]
#[path = "social_tests.rs"]
mod tests;
impl ParticipantSocial {
    /// Account policy is authoritative; temporary guest identities remain owned
    /// by this membership and cannot be used to smuggle an ignored account in.
    pub(crate) fn apply_account_preferences(
        &mut self,
        preferences: &crate::auth::account::ChatPreferences,
    ) {
        self.allow_private_messages = preferences.allow_private_messages;
        self.ignored = preferences
            .ignored
            .iter()
            .filter_map(|person| person.id.parse().ok())
            .collect();
        self.ignored.extend(self.guest_ignored.iter().cloned());
    }

    pub(super) fn accepts_inbox_from(&self, sender: &ParticipantId) -> bool {
        self.connected && self.allow_private_messages && !self.ignored.contains(sender)
    }
    pub(crate) fn new(joined_sequence: u64) -> Self {
        Self {
            connected: true,
            chat_session_id: Uuid::new_v4(),
            chat_high_water: 0,
            joined_sequence,
            allow_private_messages: true,
            ignored: HashSet::new(),
            guest_ignored: HashSet::new(),
            last_typing: None,
        }
    }
}

struct HistoryEntry {
    sequence: u64,
    sender_session: Uuid,
    recipient_session: Option<Uuid>,
    sender_authenticated: bool,
    message: ChatEntry,
    /// Serialized size counted against `HISTORY_BYTES`; reactions change it.
    bytes: usize,
}

struct ChatReceipt {
    sender_session: Uuid,
    sequence: Option<u64>,
    accepted_at: std::time::Instant,
    bytes: usize,
    message: ChatEntry,
}

struct PreparedChat {
    message: ChatEntry,
    attachment_ids: Vec<Uuid>,
    sender_session: Uuid,
    sender_authenticated: bool,
    recipient: Option<(String, Uuid, mpsc::Sender<crate::OutboundJson>)>,
    sequence: Option<u64>,
    receipt_bytes: usize,
    now: std::time::Instant,
    persist: bool,
    durable: bool,
}

struct ChatAttempt {
    content: String,
    attachment_ids: Vec<Uuid>,
    client_message_id: Option<String>,
    recipient_id: Option<ParticipantId>,
    sequence: Option<u64>,
    retry_session: Option<String>,
    reply_to: Option<MessageId>,
}

const REPORT_COOLDOWN: std::time::Duration = std::time::Duration::from_secs(30);

/// Cooldown identity for abuse reports: the account for signed-in members,
/// the canonical guest address otherwise. A session-scoped cooldown reset on
/// every rejoin and let one identity fill the room's report capacity.
pub(crate) fn reporter_key(participant: &Participant) -> String {
    if participant.authenticated {
        format!("user:{}", participant.id)
    } else if let Some(ip) = participant.ip {
        format!("ip:{ip}")
    } else {
        format!("session:{}", participant.id)
    }
}

#[derive(Default)]
pub(crate) struct RoomSocial {
    pub(crate) next_sequence: u64,
    report_cooldowns: HashMap<String, std::time::Instant>,
    history: VecDeque<HistoryEntry>,
    history_bytes: usize,
    pins: Vec<ChatEntry>,
    receipts: VecDeque<ChatReceipt>,
    receipt_bytes: usize,
    bans: HashMap<String, RuntimeBan>,
    reports: VecDeque<ReportEntry>,
    moderation_events: VecDeque<ModerationEvent>,
}

struct RuntimeBan {
    entry: BanEntry,
    participant_ids: Vec<ParticipantId>,
    guest_ip: Option<IpAddr>,
    expiry: Option<std::time::Instant>,
}

/// A request the room refused: the code the client switches on and a
/// message for people.
#[derive(Debug)]
pub(crate) struct SocialFailure {
    pub code: ErrorCode,
    pub message: String,
}
impl std::fmt::Display for SocialFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for SocialFailure {}
/// An account's saved chat look, or `None` for an unknown account. A color the
/// palette has since dropped falls back to the automatic one.
pub(crate) async fn load_chat_style(
    pool: &sqlx::PgPool,
    user: Uuid,
) -> Result<Option<ChatStyle>, sqlx::Error> {
    let row: Option<(Option<String>, String)> =
        sqlx::query_as("SELECT chat_color, chat_style FROM users WHERE id = $1")
            .bind(user)
            .fetch_optional(pool)
            .await?;
    Ok(row.map(|(color, style)| {
        let style = match style.as_str() {
            "text" => ChatStyleKind::Text,
            "bubble" => ChatStyleKind::Bubble,
            _ => ChatStyleKind::Accent,
        };
        ChatStyle { color, style }
            .validated()
            .unwrap_or(ChatStyle { color: None, style })
    }))
}

async fn save_chat_style(
    pool: &sqlx::PgPool,
    user: Uuid,
    style: &ChatStyle,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE users SET chat_color = $2, chat_style = $3, updated_at = now() WHERE id = $1",
    )
    .bind(user)
    .bind(style.color.as_deref())
    .bind(style.style.as_str())
    .execute(pool)
    .await?;
    Ok(())
}

fn refusal(code: ErrorCode, message: &str) -> anyhow::Error {
    SocialFailure {
        code,
        message: message.to_string(),
    }
    .into()
}
fn invalid(message: &str) -> anyhow::Error {
    refusal(ErrorCode::Invalid, message)
}
fn forbidden(message: &str) -> anyhow::Error {
    refusal(ErrorCode::Forbidden, message)
}
fn not_found(message: &str) -> anyhow::Error {
    refusal(ErrorCode::NotFound, message)
}
fn rate_limited(message: &str) -> anyhow::Error {
    refusal(ErrorCode::RateLimited, message)
}
fn unavailable(message: &str) -> anyhow::Error {
    refusal(ErrorCode::Unavailable, message)
}
fn timed_out(message: &str) -> anyhow::Error {
    refusal(ErrorCode::TimedOut, message)
}
fn failed(message: &str) -> anyhow::Error {
    refusal(ErrorCode::RequestFailed, message)
}
/// Sentinel texts raised inside the persistence layer carry no code of their
/// own; classify them by the words the layer uses.
fn rejected(message: &str) -> anyhow::Error {
    refusal(code_for_sentinel(message), message)
}
fn code_for_sentinel(message: &str) -> ErrorCode {
    let lower = message.to_ascii_lowercase();
    let has = |needles: &[&str]| needles.iter().any(|needle| lower.contains(needle));
    if has(&["timed out"]) {
        ErrorCode::TimedOut
    } else if has(&["not found", "no longer", "has been removed", "unknown"]) {
        ErrorCode::NotFound
    } else if has(&["unavailable"]) {
        ErrorCode::Unavailable
    } else if has(&["rate limit", "please wait", "busy"]) {
        ErrorCode::RateLimited
    } else if has(&[
        "not allowed",
        "require",
        "only ",
        "sign in",
        "capacity",
        "cannot",
        "insufficient",
        "quota",
    ]) {
        ErrorCode::Forbidden
    } else if has(&[
        "invalid", "must", "already", "too long", "empty", "choose", "exceeds", "at most",
    ]) {
        ErrorCode::Invalid
    } else {
        ErrorCode::RequestFailed
    }
}

/// Which room-wide budget a social request must reserve before it runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SocialBudget {
    None,
    ChatBroadcast,
    AdminMutation,
}

impl ClientMessage {
    /// Read-only queries and self-scoped preferences never draw on a room-wide
    /// window, so ordinary members cannot starve moderation or public chat.
    /// A nickname change fans out like a chat message; durable moderation
    /// writes share the administration window with kick, ban and settings.
    pub(crate) fn social_budget(&self) -> SocialBudget {
        match self {
            Self::GetRoomSnapshot { .. }
            | Self::GetAttachmentAccess { .. }
            | Self::GetPinnedMessages { .. }
            | Self::GetChatHistory { .. }
            | Self::MarkChatRead { .. }
            | Self::ListRoomBans { .. }
            | Self::ListRoomMembers { .. }
            | Self::ListRoomReports { .. }
            | Self::ListModerationEvents { .. }
            | Self::SetChatPreferences { .. } => SocialBudget::None,
            Self::ChangeNickname { .. }
            | Self::SetChatStyle { .. }
            | Self::ReactToMessage { .. }
            | Self::EditChatMessage { .. } => SocialBudget::ChatBroadcast,
            _ => SocialBudget::AdminMutation,
        }
    }

    pub(crate) fn social_request(&self) -> Option<(&str, &'static str)> {
        match self {
            Self::SetChatPreferences { request_id, .. } => Some((request_id, "setChatPreferences")),
            Self::GetAttachmentAccess { request_id, .. } => {
                Some((request_id, "getAttachmentAccess"))
            }
            Self::ChangeNickname { request_id, .. } => Some((request_id, "changeNickname")),
            Self::SetChatStyle { request_id, .. } => Some((request_id, "setChatStyle")),
            Self::EditChatMessage { request_id, .. } => Some((request_id, "editChatMessage")),
            Self::GetPinnedMessages { request_id } => Some((request_id, "getPinnedMessages")),
            Self::SetPinnedMessage { request_id, .. } => Some((request_id, "setPinnedMessage")),
            Self::RemoveChatMessage { request_id, .. } => Some((request_id, "removeChatMessage")),
            Self::ReactToMessage { request_id, .. } => Some((request_id, "reactToMessage")),
            Self::GetRoomSnapshot { request_id } => Some((request_id, "getRoomSnapshot")),
            Self::GetChatHistory { request_id, .. } => Some((request_id, "getChatHistory")),
            Self::MarkChatRead { request_id, .. } => Some((request_id, "markChatRead")),
            Self::SetRoomHistory { request_id, .. } => Some((request_id, "setRoomHistory")),
            Self::ListRoomBans { request_id, .. } => Some((request_id, "listRoomBans")),
            Self::RemoveRoomBan { request_id, .. } => Some((request_id, "removeRoomBan")),
            Self::ListRoomMembers { request_id, .. } => Some((request_id, "listRoomMembers")),
            Self::SetMemberRole { request_id, .. } => Some((request_id, "setMemberRole")),
            Self::ReportParticipant { request_id, .. } => Some((request_id, "reportParticipant")),
            Self::ListRoomReports { request_id, .. } => Some((request_id, "listRoomReports")),
            Self::ResolveRoomReport { request_id, .. } => Some((request_id, "resolveRoomReport")),
            Self::ListModerationEvents { request_id, .. } => {
                Some((request_id, "listModerationEvents"))
            }
            _ => None,
        }
    }
    pub(crate) fn social_error(&self, code: ErrorCode, message: &str) -> Option<ServerMessage> {
        let request_id = self.social_request().map(|(id, _)| id.to_string());
        let client_message_id = match self {
            Self::ChatMessage {
                client_message_id, ..
            } => client_message_id.clone(),
            Self::PrivateMessage {
                client_message_id, ..
            } => Some(client_message_id.clone()),
            Self::RetryChatMessage(message) => Some(message.client_message_id.clone()),
            _ => None,
        };
        if request_id.is_none() && client_message_id.is_none() {
            return None;
        }
        Some(ServerMessage::SocialError {
            request_id,
            client_message_id,
            code,
            message: message.to_string(),
        })
    }
}

fn send(
    metrics: &ServerMetrics,
    sender: &mpsc::Sender<crate::OutboundJson>,
    message: &ServerMessage,
) -> Result<()> {
    try_send_essential(
        metrics,
        sender,
        crate::OutboundJson::from(serde_json::to_string(message)?),
    )
    .map_err(|_| rate_limited("Connection is busy; please retry"))
}

/// An uncertain commit is not a rejected send. Its owned room was quarantined
/// already; preserve the client's unconfirmed attempt instead of inviting a new ID.
fn chat_persistence_failure(
    metrics: &ServerMetrics,
    sender: &mpsc::Sender<crate::OutboundJson>,
    client_message_id: &str,
    error: sqlx::Error,
) -> Result<()> {
    crate::db::record_error(&error);
    if control::persistence_is_indeterminate(&error) {
        let _ = send(
            metrics,
            sender,
            &ServerMessage::MessageRetryResult {
                client_message_id: client_message_id.to_owned(),
                outcome: ChatRetryOutcome::Unknown,
                reason: ChatRetryReason::StorageUnconfirmed,
            },
        );
        Ok(())
    } else {
        Err(failed("Message could not be saved; try again"))
    }
}

fn acknowledge_chat(
    metrics: &ServerMetrics,
    sender: &mpsc::Sender<crate::OutboundJson>,
    message: ChatEntry,
) -> Result<()> {
    let json = crate::OutboundJson::from(serde_json::to_string(&ServerMessage::MessageAck {
        client_message_id: message.client_message_id.clone(),
        message,
    })?);
    // Acceptance already happened. Queue rejection is counted, but must not
    // become a SocialError claiming the message failed. The sender retains an
    // unconfirmed attempt and can reconcile its receipt after reconnecting.
    let _ = try_send_essential(metrics, sender, json);
    Ok(())
}
fn page_offset(offset: Option<u32>) -> Result<usize> {
    let offset = offset.unwrap_or(0) as usize;
    if offset > 10_000 {
        return Err(invalid("Invalid page"));
    }
    Ok(offset)
}
fn require_role(actual: roles::Role, required: roles::Role) -> Result<()> {
    if actual < required {
        return Err(forbidden("This action requires a room moderator"));
    }
    Ok(())
}
fn visible(entry: &HistoryEntry, viewer: &Participant) -> bool {
    if entry.sequence < viewer.social.joined_sequence {
        return false;
    }
    if entry.message.recipient_id.is_some() {
        entry.sender_session == viewer.media_session_id
            || entry.recipient_session == Some(viewer.media_session_id)
    } else {
        !viewer
            .social
            .ignored
            .contains(&entry.message.participant_id)
    }
}
fn replied_to(message: &ChatEntry) -> Option<MessageId> {
    message.reply_to.as_ref().map(|reply| reply.message_id)
}
/// Erase message text and references without retaining a second hidden copy.
pub(super) fn redact_message(
    message: &mut ChatEntry,
    message_id: &MessageId,
    removed_at: chrono::DateTime<chrono::Utc>,
) {
    if message.recipient_id.is_some() {
        return;
    }
    if message.message_id == *message_id {
        message.content.clear();
        message.attachments.clear();
        message.reply_to = None;
        message.reactions.clear();
        message.removed_at = Some(removed_at);
    } else if let Some(reply) = &mut message.reply_to
        && reply.message_id == *message_id
    {
        reply.excerpt = "Message removed".to_owned();
    }
}
/// Quotes the retained message a reply answers. The replier must be able to see it,
/// and it must belong to the same conversation: public with public, or the same pair.
fn quote_reply(
    history: &VecDeque<HistoryEntry>,
    sender: &Participant,
    recipient_id: Option<&ParticipantId>,
    message_id: &MessageId,
) -> Result<ChatReplyRef> {
    let original = history
        .iter()
        .rev()
        .find(|entry| entry.message.message_id == *message_id && visible(entry, sender))
        .ok_or_else(|| not_found("That message is no longer available to reply to"))?;
    reply_reference(&original.message, sender, recipient_id)
}

/// A saved public row is readable by every currently admitted, non-ignoring
/// viewer, even when the row predates their runtime replay membership.
fn saved_public_reply(message: &ChatEntry, sender: &Participant) -> Result<ChatReplyRef> {
    if message.recipient_id.is_some() || sender.social.ignored.contains(&message.participant_id) {
        return Err(not_found("That message is no longer available to reply to"));
    }
    reply_reference(message, sender, None)
}

fn reply_reference(
    message: &ChatEntry,
    sender: &Participant,
    recipient_id: Option<&ParticipantId>,
) -> Result<ChatReplyRef> {
    if message.removed_at.is_some() {
        return Err(not_found("That message has been removed"));
    }
    let same_conversation = match (recipient_id, message.recipient_id.as_ref()) {
        (None, None) => true,
        (Some(to), Some(other)) => {
            let pair = [&message.participant_id, other];
            pair.contains(&to) && pair.contains(&&sender.id)
        }
        _ => false,
    };
    if !same_conversation {
        return Err(invalid(
            "Reply within the conversation the message belongs to",
        ));
    }
    let flat = message
        .content
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let mut chars = flat.chars();
    let head: String = chars.by_ref().take(REPLY_EXCERPT_CHARS).collect();
    let excerpt = if chars.next().is_some() {
        format!("{}…", head.trim_end())
    } else {
        head
    };
    Ok(ChatReplyRef {
        message_id: message.message_id,
        participant_id: message.participant_id,
        participant_name: message.participant_name.clone(),
        excerpt,
    })
}
/// Adds `participant_id`'s reaction to a retained message it can see, or takes it
/// back, and tells everyone who can see the message, the reactor included.
fn prepare_reaction(
    room: &Room,
    participant_id: &ParticipantId,
    message_id: &MessageId,
    emoji: &str,
) -> Result<Vec<ChatReaction>> {
    let actor = room
        .participants
        .get(participant_id)
        .ok_or_else(|| not_found("Participant not found"))?;
    let index = room
        .social
        .history
        .iter()
        .position(|entry| entry.message.message_id == *message_id && visible(entry, actor))
        .ok_or_else(|| not_found("That message is no longer available"))?;
    if room.social.history[index].message.removed_at.is_some() {
        return Err(not_found("That message has been removed"));
    }
    toggle_reactions(
        &room.social.history[index].message.reactions,
        participant_id,
        emoji,
    )
    .map_err(rejected)
}

/// Shared by the runtime and transaction-locked saved-message paths. The
/// caller owns target visibility; this function enforces the bounded palette.
pub(super) fn toggle_reactions(
    current: &[ChatReaction],
    participant_id: &ParticipantId,
    emoji: &str,
) -> std::result::Result<Vec<ChatReaction>, &'static str> {
    if !REACTIONS.contains(&emoji) {
        return Err("Choose one of the offered reactions");
    }
    let reactor = participant_id.to_owned();
    let mut reactions = current.to_vec();
    match reactions
        .iter()
        .position(|reaction| reaction.emoji == emoji)
    {
        Some(at) if reactions[at].participant_ids.contains(&reactor) => {
            reactions[at].participant_ids.retain(|id| *id != reactor);
            if reactions[at].participant_ids.is_empty() {
                reactions.remove(at);
            }
        }
        found => {
            let total: usize = reactions.iter().map(|r| r.participant_ids.len()).sum();
            if total >= MESSAGE_REACTIONS {
                return Err("This message has all the reactions it can hold");
            }
            match found {
                Some(at) => reactions[at].participant_ids.push(reactor),
                None => reactions.push(ChatReaction {
                    emoji: emoji.to_string(),
                    participant_ids: vec![reactor],
                }),
            }
        }
    }
    Ok(reactions)
}

/// HTTP retries express the desired presence, so losing a response cannot
/// toggle the same reaction off again.
pub(super) fn set_reaction(
    current: &[ChatReaction],
    participant_id: &ParticipantId,
    emoji: &str,
    present: bool,
) -> std::result::Result<Vec<ChatReaction>, &'static str> {
    if !REACTIONS.contains(&emoji) {
        return Err("Choose one of the offered reactions");
    }
    let existing = current.iter().any(|reaction| {
        reaction.emoji == emoji
            && reaction
                .participant_ids
                .iter()
                .any(|id| id == participant_id)
    });
    if existing == present {
        Ok(current.to_vec())
    } else {
        toggle_reactions(current, participant_id, emoji)
    }
}

/// Saved reaction completion does not insert an old message into bounded replay.
/// Public history access also permits pre-join viewers to observe its changes.
fn publish_saved_reaction(room: &mut Room, message: &ChatEntry) -> Result<()> {
    if message.recipient_id.is_some() || message.removed_at.is_some() {
        return Err(unavailable("Public message unavailable"));
    }
    if let Some(index) = room.social.history.iter().position(|entry| {
        entry.message.message_id == message.message_id && entry.message.removed_at.is_none()
    }) {
        room.social.history[index]
            .message
            .reactions
            .clone_from(&message.reactions);
        room.social.recount(index);
    }
    for pin in &mut room.social.pins {
        if pin.message_id == message.message_id && pin.removed_at.is_none() {
            pin.reactions.clone_from(&message.reactions);
        }
    }
    let event =
        crate::OutboundJson::from(serde_json::to_string(&ServerMessage::MessageReactions {
            message_id: message.message_id,
            reactions: message.reactions.clone(),
        })?);
    for viewer in room.participants.values().filter(|viewer| {
        viewer.social.connected && !viewer.social.ignored.contains(&message.participant_id)
    }) {
        let _ = try_send_essential(&room.metrics, &viewer.sender, event.clone());
    }
    Ok(())
}

fn publish_reaction(
    room: &mut Room,
    message_id: &MessageId,
    reactions: Vec<ChatReaction>,
) -> Result<Vec<ChatReaction>> {
    // Ephemeral chat can trim replay while a saved reaction waits for SQL.
    // Resolve the identity again instead of applying it to the old slot.
    let index = room
        .social
        .history
        .iter()
        .position(|entry| entry.message.message_id == *message_id)
        .ok_or_else(|| not_found("That message is no longer available"))?;
    if room.social.history[index].message.removed_at.is_some() {
        return Err(not_found("That message has been removed"));
    }
    room.social.history[index].message.reactions = reactions.clone();
    let event =
        crate::OutboundJson::from(serde_json::to_string(&ServerMessage::MessageReactions {
            message_id: *message_id,
            reactions: reactions.clone(),
        })?);
    let entry = &room.social.history[index];
    for viewer in room.participants.values().filter(|p| visible(entry, p)) {
        let _ = try_send_essential(&room.metrics, &viewer.sender, event.clone());
    }
    room.social.recount(index);
    Ok(reactions)
}
#[cfg(test)]
fn react_to_message(
    room: &mut Room,
    participant_id: &ParticipantId,
    message_id: &MessageId,
    emoji: &str,
) -> Result<Vec<ChatReaction>> {
    let reactions = prepare_reaction(room, participant_id, message_id, emoji)?;
    publish_reaction(room, message_id, reactions)
}

fn prepare_edit(
    room: &Room,
    actor: &Participant,
    message_id: &MessageId,
    request: &history::EditRequest,
) -> Result<Option<ChatEntry>> {
    if room.settings.as_ref().is_some_and(|s| !s.allow_chat)
        || !moderation::can_chat(
            &actor.punitive,
            actor.role,
            room.settings.as_ref().is_some_and(|s| s.moderated),
        )
    {
        return Err(forbidden("You are not allowed to chat"));
    }
    let Some(entry) = room
        .social
        .history
        .iter()
        .find(|entry| entry.message.message_id == *message_id)
    else {
        return Ok(None);
    };
    if entry.message.participant_id != actor.id
        || (!actor.authenticated && entry.sender_session != actor.media_session_id)
        || (entry.message.recipient_id.is_some() && !visible(entry, actor))
    {
        return Err(forbidden("Only the author can edit this message"));
    }
    if let Some(peer) = &entry.message.recipient_id {
        let recipient = room
            .participants
            .get(peer)
            .ok_or_else(|| forbidden("Edit this saved conversation in Messages"))?;
        if !can_private_message(actor, recipient) {
            return Err(unavailable("Private message unavailable"));
        }
    }
    let mut edited = entry.message.clone();
    history::apply_edit(&mut edited, request).map_err(rejected)?;
    Ok(Some(edited))
}

fn publish_edit(room: &mut Room, message: &ChatEntry) -> Result<()> {
    let event =
        crate::OutboundJson::from(serde_json::to_string(&ServerMessage::ChatMessageEdited {
            message: message.clone(),
        })?);
    let viewers: Vec<_> = if message.recipient_id.is_some() {
        room.social
            .history
            .iter()
            .find(|entry| entry.message.message_id == message.message_id)
            .map(|entry| {
                room.participants
                    .values()
                    .filter(|p| visible(entry, p))
                    .map(|p| p.sender.clone())
                    .collect()
            })
            .unwrap_or_default()
    } else {
        room.participants
            .values()
            .filter(|p| !p.social.ignored.contains(&message.participant_id))
            .map(|p| p.sender.clone())
            .collect()
    };
    room.social.edit_message(message);
    for sender in viewers {
        let _ = try_send_essential(&room.metrics, &sender, event.clone());
    }
    Ok(())
}

fn publish_pins(room: &Room) -> Result<()> {
    let mut encoded = HashMap::new();
    for viewer in room.participants.values() {
        let mut mask = 0u8;
        let messages: Vec<_> = room
            .social
            .pins
            .iter()
            .enumerate()
            .filter_map(|(index, message)| {
                if viewer.social.ignored.contains(&message.participant_id) {
                    None
                } else {
                    mask |= 1 << index;
                    Some(message.clone())
                }
            })
            .collect();
        let event = match encoded.entry(mask) {
            std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(crate::OutboundJson::from(serde_json::to_string(
                    &ServerMessage::PinnedMessagesChanged { messages },
                )?))
            }
        };
        let _ = try_send_essential(&room.metrics, &viewer.sender, event.clone());
    }
    Ok(())
}

fn can_private_message(sender: &Participant, recipient: &Participant) -> bool {
    sender.id != recipient.id
        && recipient.social.allow_private_messages
        && !recipient.social.ignored.contains(&sender.id)
        && !sender.social.ignored.contains(&recipient.id)
}

/// An HTTP first message may use the same account-to-account authority as an
/// admitted room send. The caller holds that room's control gate through commit.
pub(super) fn can_start_account_conversation(
    room: &Room,
    sender: &ParticipantId,
    recipient: &ParticipantId,
) -> bool {
    let (Some(sender), Some(recipient)) = (
        room.participants.get(sender),
        room.participants.get(recipient),
    ) else {
        return false;
    };
    sender.authenticated
        && recipient.authenticated
        && sender.social.connected
        && recipient.social.connected
        && !sender.sender.is_closed()
        && !recipient.sender.is_closed()
        && !room
            .settings
            .as_ref()
            .is_some_and(|settings| !settings.allow_chat)
        && moderation::can_chat(
            &sender.punitive,
            sender.role,
            room.settings
                .as_ref()
                .is_some_and(|settings| settings.moderated),
        )
        && can_private_message(sender, recipient)
}
impl RoomSocial {
    fn reconcile_reactions(&mut self, message: &ChatEntry) {
        if let Some(index) = self.history.iter().position(|entry| {
            entry.message.message_id == message.message_id && entry.message.removed_at.is_none()
        }) {
            self.history[index]
                .message
                .reactions
                .clone_from(&message.reactions);
            self.recount(index);
        }
    }

    /// Scrub both replay and deduplication receipts before broadcasting removal.
    fn remove_message(
        &mut self,
        message_id: &MessageId,
        removed_at: chrono::DateTime<chrono::Utc>,
    ) {
        self.pins
            .retain(|message| message.message_id != *message_id);
        self.history_bytes = 0;
        for entry in &mut self.history {
            redact_message(&mut entry.message, message_id, removed_at);
            entry.bytes = serde_json::to_vec(&entry.message).map_or(HISTORY_BYTES, |v| v.len());
            self.history_bytes += entry.bytes;
        }
        self.receipt_bytes = 0;
        for entry in &mut self.receipts {
            redact_message(&mut entry.message, message_id, removed_at);
            entry.bytes =
                serde_json::to_vec(&entry.message).map_or(CHAT_RECEIPT_BYTES, |v| v.len());
            self.receipt_bytes += entry.bytes;
        }
        self.trim_history();
        while self.receipt_bytes > CHAT_RECEIPT_BYTES {
            let Some(entry) = self.receipts.pop_front() else {
                break;
            };
            self.receipt_bytes = self.receipt_bytes.saturating_sub(entry.bytes);
        }
    }
    fn edit_message(&mut self, edited: &ChatEntry) {
        let stale = self
            .history
            .iter()
            .map(|entry| &entry.message)
            .chain(self.receipts.iter().map(|entry| &entry.message))
            .chain(self.pins.iter())
            .any(|message| {
                message.message_id == edited.message_id
                    && (message.removed_at.is_some() || message.revision > edited.revision)
            });
        if stale {
            return;
        }
        let excerpt = history::quote_excerpt(&edited.content);
        let update = |message: &mut ChatEntry| {
            if message.removed_at.is_some() {
                return;
            }
            if message.message_id == edited.message_id && message.revision <= edited.revision {
                // Reactions have their own ordered event; an older edited body must not undo them.
                message.content.clone_from(&edited.content);
                message.revision = edited.revision;
                message.edited_at.clone_from(&edited.edited_at);
            }
            if let Some(reply) = &mut message.reply_to
                && reply.message_id == edited.message_id
            {
                reply.excerpt.clone_from(&excerpt);
            }
        };
        self.history_bytes = 0;
        for entry in &mut self.history {
            update(&mut entry.message);
            entry.bytes = serde_json::to_vec(&entry.message).map_or(HISTORY_BYTES, |v| v.len());
            self.history_bytes += entry.bytes;
        }
        self.receipt_bytes = 0;
        for entry in &mut self.receipts {
            update(&mut entry.message);
            entry.bytes =
                serde_json::to_vec(&entry.message).map_or(CHAT_RECEIPT_BYTES, |v| v.len());
            self.receipt_bytes += entry.bytes;
        }
        for message in &mut self.pins {
            update(message);
        }
        self.trim_history();
        while self.receipt_bytes > CHAT_RECEIPT_BYTES {
            let Some(entry) = self.receipts.pop_front() else {
                break;
            };
            self.receipt_bytes = self.receipt_bytes.saturating_sub(entry.bytes);
        }
    }
    fn prune_receipts(&mut self, now: std::time::Instant) {
        while self
            .receipts
            .front()
            .is_some_and(|entry| now.duration_since(entry.accepted_at) >= CHAT_RECEIPT_TTL)
        {
            if let Some(entry) = self.receipts.pop_front() {
                self.receipt_bytes = self.receipt_bytes.saturating_sub(entry.bytes);
            }
        }
    }

    fn make_receipt_room(&mut self, sender_session: Uuid, bytes: usize) {
        // Accepted newer messages must not stall on a confirmation cache. The
        // membership watermark still prevents evicted sequenced attempts from
        // being delivered twice; their status simply becomes unconfirmed.
        while self
            .receipts
            .iter()
            .filter(|entry| entry.sender_session == sender_session)
            .count()
            >= CHAT_RECEIPTS_PER_MEMBER
        {
            let index = self
                .receipts
                .iter()
                .position(|entry| entry.sender_session == sender_session)
                .expect("member receipt count checked");
            let entry = self.receipts.remove(index).expect("receipt index checked");
            self.receipt_bytes -= entry.bytes;
        }
        while self.receipts.len() >= CHAT_RECEIPTS
            || bytes > CHAT_RECEIPT_BYTES.saturating_sub(self.receipt_bytes)
        {
            let Some(entry) = self.receipts.pop_front() else {
                break;
            };
            self.receipt_bytes -= entry.bytes;
        }
    }

    /// Whether this identity reported within the cooldown. Expired entries
    /// are dropped here, so the map never outgrows one cooldown of reporters.
    pub(crate) fn report_cooldown_active(&mut self, key: &str, now: std::time::Instant) -> bool {
        self.report_cooldowns
            .retain(|_, at| now.duration_since(*at) < REPORT_COOLDOWN);
        self.report_cooldowns
            .get(key)
            .is_some_and(|at| now.duration_since(*at) < REPORT_COOLDOWN)
    }

    pub(crate) fn note_report(&mut self, key: String, now: std::time::Instant) {
        self.report_cooldowns.insert(key, now);
    }

    /// Drop an account's runtime ban and say what name it was recorded under.
    pub(crate) fn forget_user_ban(&mut self, participant_id: &ParticipantId) -> Option<String> {
        let mut name = None;
        self.bans.retain(|_, ban| {
            let matches = ban.entry.authenticated
                && ban.participant_ids.iter().any(|id| id == participant_id);
            if matches {
                name = Some(ban.entry.display_name.clone());
            }
            !matches
        });
        name
    }

    /// Retire every cached sanction for the selected account or canonical guest
    /// address. The caller removes the returned identities from room gate caches.
    pub(super) fn forget_ban(
        &mut self,
        user: Option<&ParticipantId>,
        guest_ip: Option<IpAddr>,
    ) -> Vec<ParticipantId> {
        let mut affected = Vec::new();
        self.bans.retain(|_, ban| {
            let matches = user.is_some_and(|user| ban.participant_ids.iter().any(|id| id == user))
                || guest_ip.is_some_and(|ip| ban.guest_ip == Some(ip));
            if matches {
                affected.extend(ban.participant_ids.iter().cloned());
            }
            !matches
        });
        affected
    }

    pub(crate) fn record_moderation_event(&mut self, event: ModerationEvent) {
        if self.moderation_events.len() >= MAX_RUNTIME_MODERATION_EVENTS {
            self.moderation_events.pop_front();
        }
        self.moderation_events.push_back(event);
    }

    /// Resolve the open runtime report a sanction answers; a report this room
    /// never received is refused.
    pub(crate) fn link_report(&mut self, report_id: &str) -> Result<()> {
        let report = self
            .reports
            .iter_mut()
            .find(|report| report.report_id == report_id)
            .ok_or_else(|| not_found("Report not found"))?;
        if report.status == ReportStatus::Open {
            report.status = ReportStatus::Resolved;
            report.resolved_at = Some(chrono::Utc::now().to_rfc3339());
        }
        Ok(())
    }

    fn report_outcome(&self, report_id: &str) -> Option<ReportOutcome> {
        self.moderation_events
            .iter()
            .rev()
            .find(|event| event.report_id.as_deref() == Some(report_id))
            .map(|event| ReportOutcome {
                action: event.action.as_str().to_string(),
                created_at: event.created_at.to_rfc3339(),
            })
    }
    fn remember(
        &mut self,
        sender_session: Uuid,
        recipient_session: Option<Uuid>,
        sender_authenticated: bool,
        message: ChatEntry,
    ) {
        // Names and identifiers also count, keeping retention bounded even with short text.
        let bytes = serde_json::to_vec(&message).map_or(HISTORY_BYTES, |v| v.len());
        self.history_bytes += bytes;
        self.history.push_back(HistoryEntry {
            sequence: self.next_sequence,
            sender_session,
            recipient_session,
            sender_authenticated,
            message,
            bytes,
        });
        self.next_sequence = self.next_sequence.saturating_add(1);
        self.trim_history();
    }
    /// Re-counts an entry whose reactions changed, then trims to the budget.
    fn recount(&mut self, index: usize) {
        if let Some(entry) = self.history.get_mut(index) {
            let bytes = serde_json::to_vec(&entry.message).map_or(HISTORY_BYTES, |v| v.len());
            self.history_bytes = self.history_bytes.saturating_sub(entry.bytes) + bytes;
            entry.bytes = bytes;
        }
        self.trim_history();
    }
    fn trim_history(&mut self) {
        while self.history.len() > HISTORY_MESSAGES || self.history_bytes > HISTORY_BYTES {
            match self.history.pop_front() {
                Some(old) => self.history_bytes = self.history_bytes.saturating_sub(old.bytes),
                None => break,
            }
        }
    }
    pub(crate) fn reserve_ban(&mut self) -> Result<()> {
        let now = std::time::Instant::now();
        self.bans
            .retain(|_, ban| ban.expiry.is_none_or(|expiry| expiry > now));
        if self.bans.len() >= MAX_RUNTIME_BANS {
            return Err(forbidden("Room ban capacity reached"));
        }
        Ok(())
    }
    pub(crate) fn record_ban(
        &mut self,
        name: String,
        authenticated: bool,
        participant_ids: Vec<ParticipantId>,
        ip: Option<IpAddr>,
        reason: Option<&str>,
        duration: Option<u64>,
    ) {
        let id = Uuid::new_v4().to_string();
        self.bans.insert(
            id.clone(),
            RuntimeBan {
                entry: BanEntry {
                    ban_id: id,
                    display_name: name,
                    authenticated,
                    reason: reason.map(String::from),
                    expires_at: duration.map(|s| {
                        (chrono::Utc::now() + chrono::Duration::seconds(s as i64)).to_rfc3339()
                    }),
                },
                participant_ids,
                guest_ip: (!authenticated)
                    .then_some(ip)
                    .flatten()
                    .map(crate::net::cohort),
                expiry: duration.and_then(|s| {
                    std::time::Instant::now().checked_add(std::time::Duration::from_secs(s))
                }),
            },
        );
    }
}

impl Room {
    /// Tell a conversation's recipients that `sender_id` is composing: everyone
    /// who would receive the message and does not ignore the sender, at most
    /// once per `TYPING_INTERVAL`. A sender who cannot chat is silently dropped.
    pub(crate) fn relay_typing(
        &mut self,
        sender_id: &ParticipantId,
        expected_sender: &mpsc::Sender<crate::OutboundJson>,
        target: Option<&ParticipantId>,
    ) -> Result<()> {
        let now = std::time::Instant::now();
        let moderated = self.settings.as_ref().is_some_and(|s| s.moderated);
        let chat_allowed = !self.settings.as_ref().is_some_and(|s| !s.allow_chat);
        let (relay, target_accepts) = {
            let sender = RoomManager::participant_for_sender(self, sender_id, expected_sender)?;
            let allowed = chat_allowed
                && moderation::can_chat(&sender.punitive, sender.role, moderated)
                && !sender
                    .social
                    .last_typing
                    .is_some_and(|last| now.duration_since(last) < TYPING_INTERVAL);
            let target_accepts = target.is_some_and(|target| {
                target != sender_id
                    && !sender.social.ignored.contains(target)
                    && self.participants.get(target).is_some_and(|recipient| {
                        recipient.social.allow_private_messages
                            && !recipient.social.ignored.contains(sender_id)
                    })
            });
            (allowed, target_accepts)
        };
        if !relay {
            return Ok(());
        }
        if let Some(sender) = self.participants.get_mut(sender_id) {
            sender.social.last_typing = Some(now);
        }
        let event = ServerMessage::ParticipantTyping {
            participant_id: *sender_id,
            target_participant_id: target.copied(),
        };
        let json = crate::OutboundJson::from(serde_json::to_string(&event)?);
        match target {
            Some(target) => {
                if target_accepts && let Some(recipient) = self.participants.get(target) {
                    self.try_send_broadcast(&recipient.sender, json, &event);
                }
            }
            None => {
                for participant in self.participants.values() {
                    if participant.id != *sender_id
                        && !participant.social.ignored.contains(sender_id)
                    {
                        self.try_send_broadcast(&participant.sender, json.clone(), &event);
                    }
                }
            }
        }
        Ok(())
    }
}

impl RoomManager {
    /// Live membership and original-message visibility are checked for every
    /// grant and every byte fetch, including reconnect/rejoin and removal.
    pub(crate) async fn attachment_grant_context(
        &self,
        room_id: &RoomId,
        participant_id: &ParticipantId,
        expected_sender: Option<&mpsc::Sender<crate::OutboundJson>>,
        media_session: Option<Uuid>,
        attachment_id: Uuid,
        location: &crate::attachments::AttachmentLocation,
    ) -> Option<(Uuid, bool)> {
        let room_lock = self.get_room(room_id).ok()?;
        let room = room_lock.read().await;
        if room.deleting {
            return None;
        }
        let viewer = room.participants.get(participant_id)?;
        if !viewer.social.connected
            || viewer.sender.is_closed()
            || expected_sender.is_some_and(|sender| !viewer.sender.same_channel(sender))
            || media_session.is_some_and(|session| viewer.media_session_id != session)
        {
            return None;
        }
        let live = room.social.history.iter().any(|entry| {
            visible(entry, viewer)
                && entry.message.removed_at.is_none()
                && entry
                    .message
                    .attachments
                    .iter()
                    .any(|file| file.id == attachment_id)
                && Some(entry.message.message_id.as_uuid())
                    == location.message_id.or(location.ephemeral_message_id)
        });
        let retained_public = location.message_id.is_some()
            && location.room_id.as_deref() == Some(room_id.as_str())
            && room.persisted
            && room
                .settings
                .as_ref()
                .is_some_and(|settings| settings.history_retention_days > 0);
        (live || retained_public).then_some((viewer.media_session_id, viewer.authenticated))
    }

    pub(crate) async fn relay_typing(
        &self,
        room_id: &RoomId,
        sender_id: &ParticipantId,
        expected_sender: &mpsc::Sender<crate::OutboundJson>,
        target: Option<&ParticipantId>,
    ) -> Result<()> {
        let room_lock = self.get_room(room_id)?;
        let mut room = room_lock.write().await;
        room.relay_typing(sender_id, expected_sender, target)
    }

    /// Test convenience: a public or private chat command exactly as a socket
    /// would deliver it, dispatched through [`Self::handle_chat_command`].
    #[cfg(test)]
    pub(crate) async fn send_social_chat(
        &self,
        room_id: &RoomId,
        sender_id: &ParticipantId,
        expected_sender: &mpsc::Sender<crate::OutboundJson>,
        content: String,
        client_message_id: Option<String>,
        recipient_id: Option<&ParticipantId>,
    ) -> Result<()> {
        let command = match recipient_id {
            Some(target) => ClientMessage::PrivateMessage {
                target_participant_id: target.to_owned(),
                content,
                attachment_ids: Vec::new(),
                client_message_id: client_message_id
                    .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
                sequence: None,
                reply_to: None,
            },
            None => ClientMessage::ChatMessage {
                content,
                attachment_ids: Vec::new(),
                client_message_id,
                sequence: None,
                reply_to: None,
            },
        };
        self.handle_chat_command(room_id, sender_id, expected_sender, &command)
            .await
    }

    pub(crate) async fn handle_chat_command(
        &self,
        room_id: &RoomId,
        sender_id: &ParticipantId,
        expected_sender: &mpsc::Sender<crate::OutboundJson>,
        command: &ClientMessage,
    ) -> Result<()> {
        let attempt = match command {
            ClientMessage::ChatMessage {
                content,
                attachment_ids,
                client_message_id,
                sequence,
                reply_to,
            } => ChatAttempt {
                content: content.clone(),
                attachment_ids: attachment_ids.clone(),
                client_message_id: client_message_id.clone(),
                recipient_id: None,
                sequence: *sequence,
                retry_session: None,
                reply_to: *reply_to,
            },
            ClientMessage::PrivateMessage {
                content,
                attachment_ids,
                client_message_id,
                target_participant_id,
                sequence,
                reply_to,
            } => ChatAttempt {
                content: content.clone(),
                attachment_ids: attachment_ids.clone(),
                client_message_id: Some(client_message_id.clone()),
                recipient_id: Some(*target_participant_id),
                sequence: *sequence,
                retry_session: None,
                reply_to: *reply_to,
            },
            ClientMessage::RetryChatMessage(message) => ChatAttempt {
                content: message.content.clone(),
                attachment_ids: message.attachment_ids.clone(),
                client_message_id: Some(message.client_message_id.clone()),
                recipient_id: message.target_participant_id,
                sequence: Some(message.sequence),
                retry_session: Some(message.chat_session_id.clone()),
                reply_to: message.reply_to,
            },
            _ => return Err(invalid("Invalid chat command")),
        };
        self.submit_chat_attempt(room_id, sender_id, expected_sender, attempt)
            .await
    }

    async fn submit_chat_attempt(
        &self,
        room_id: &RoomId,
        sender_id: &ParticipantId,
        expected_sender: &mpsc::Sender<crate::OutboundJson>,
        attempt: ChatAttempt,
    ) -> Result<()> {
        let room_lock = self.get_room(room_id)?;
        let mut room = room_lock.write().await;
        let durable = self.db_pool.is_some()
            && if let Some(recipient) = attempt.recipient_id.as_ref() {
                room.participants
                    .get(sender_id)
                    .is_some_and(|sender| sender.authenticated)
                    && room
                        .participants
                        .get(recipient)
                        .is_some_and(|recipient| recipient.authenticated)
            } else {
                room.persisted
                    && room
                        .settings
                        .as_ref()
                        .is_some_and(|settings| settings.history_retention_days > 0)
            };
        if !durable && attempt.attachment_ids.is_empty() {
            // Validate and publish ephemeral chat in this same state-lock turn.
            // It has no persistence phase that could race a consent change.
            return Self::process_chat_attempt(&mut room, sender_id, expected_sender, attempt);
        }
        drop(room);
        let sender_id = sender_id.to_owned();
        let expected_sender = expected_sender.clone();
        self.run_room_control(room_id, move |manager, room_id, _control| async move {
            let room_lock = manager.get_room(&room_id)?;
            let mut room = room_lock.write().await;
            let saved_reply = if attempt.recipient_id.is_none()
                && room.persisted
                && room
                    .settings
                    .as_ref()
                    .is_some_and(|settings| settings.history_retention_days > 0)
                && let Some(id) = attempt.reply_to.as_ref()
            {
                let actor = Self::participant_for_sender(&room, &sender_id, &expected_sender)?;
                if quote_reply(&room.social.history, actor, None, id).is_err() {
                    let pool = manager
                        .db_pool
                        .as_ref()
                        .ok_or_else(|| unavailable("History unavailable"))?;
                    drop(room);
                    let saved = tokio::time::timeout(
                        control::PERSISTENCE_TIMEOUT,
                        history::lookup_public(pool, room_id.as_str(), id),
                    )
                    .await
                    .map_err(|_| timed_out("Chat history timed out"))??;
                    room = room_lock.write().await;
                    room.ensure_live()?;
                    Self::participant_for_sender(&room, &sender_id, &expected_sender)?;
                    saved.map(|(message, _authenticated)| message)
                } else {
                    None
                }
            } else {
                None
            };
            let Some(mut prepared) = Self::prepare_chat_attempt_with_saved_reply(
                &mut room,
                &sender_id,
                &expected_sender,
                attempt,
                saved_reply.as_ref(),
            )?
            else {
                return Ok(());
            };
            let retention_days = if prepared.message.recipient_id.is_some() {
                history::PM_RETENTION_DAYS
            } else {
                room.settings
                    .as_ref()
                    .map_or(0, |s| s.history_retention_days)
            };
            if prepared.persist {
                let pool = manager
                    .db_pool
                    .as_ref()
                    .ok_or_else(|| unavailable("History unavailable"))?;
                drop(room);
                let public_room = prepared
                    .message
                    .recipient_id
                    .is_none()
                    .then_some(room_id.as_str());
                let persisted = manager
                    .persist_room(
                        &room_id,
                        &room_lock,
                        history::persist_message(
                            pool,
                            public_room,
                            prepared.sender_session,
                            prepared.sender_authenticated,
                            retention_days,
                            &prepared.message,
                            &prepared.attachment_ids,
                        ),
                    )
                    .await;
                prepared.message = match persisted {
                    Ok(message) => {
                        manager.account_updates().private_message(&message);
                        message
                    }
                    Err(error) => {
                        return chat_persistence_failure(
                            &manager.metrics,
                            &expected_sender,
                            &prepared.message.client_message_id,
                            error,
                        );
                    }
                };
                prepared.receipt_bytes = serde_json::to_vec(&prepared.message)?.len();
                prepared.durable = true;
                room = room_lock.write().await;
                room.ensure_live()?;
                Self::participant_for_sender(&room, &sender_id, &expected_sender)?;
            } else if !prepared.attachment_ids.is_empty() {
                let pool = manager
                    .db_pool
                    .as_ref()
                    .ok_or_else(|| unavailable("Attachments unavailable"))?;
                drop(room);
                let attached = manager
                    .persist_room(
                        &room_id,
                        &room_lock,
                        crate::attachments::bind_ephemeral(
                            pool,
                            sender_id.as_uuid(),
                            &prepared.attachment_ids,
                            prepared.message.message_id.as_uuid(),
                            room_id.as_str(),
                        ),
                    )
                    .await;
                prepared.message.attachments = match attached {
                    Ok(metadata) => metadata,
                    Err(error) => {
                        return chat_persistence_failure(
                            &manager.metrics,
                            &expected_sender,
                            &prepared.message.client_message_id,
                            error,
                        );
                    }
                };
                prepared.receipt_bytes = serde_json::to_vec(&prepared.message)?.len();
                room = room_lock.write().await;
                room.ensure_live()?;
                Self::participant_for_sender(&room, &sender_id, &expected_sender)?;
            }
            Self::finish_chat_attempt(&mut room, &sender_id, &expected_sender, prepared)
        })
        .await
    }

    /// Own committed completion before another await can cancel the request.
    /// Scheduling never acquires another room's gate while the origin holds one.
    fn schedule_inbox_edit(&self, message: &ChatEntry) -> Option<tokio::task::JoinHandle<()>> {
        self.schedule_inbox_update(message, false)
    }

    fn schedule_inbox_update(
        &self,
        message: &ChatEntry,
        reactions: bool,
    ) -> Option<tokio::task::JoinHandle<()>> {
        use futures_util::StreamExt as _;
        use tracing::Instrument as _;
        let recipient = message.recipient_id?;
        let encoded = serde_json::to_string(&ServerMessage::ChatMessageEdited {
            message: message.clone(),
        })
        .ok()?;
        let event = crate::OutboundJson::from(encoded);
        let rooms: Vec<_> = self
            .rooms
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|(id, lock)| (id.clone(), lock.clone()))
            .collect();
        let manager = self.clone();
        let message = message.clone();
        Some(tokio::spawn(
            async move {
                let completion =
                    futures_util::stream::iter(rooms).for_each_concurrent(4, |(id, lock)| {
                        let manager = &manager;
                        let message = &message;
                        let event = &event;
                        let recipient = &recipient;
                        async move {
                            {
                                let room = lock.read().await;
                                if room.deleting
                                    || (!room.participants.contains_key(&message.participant_id)
                                        && !room.participants.contains_key(recipient))
                                {
                                    return;
                                }
                            }
                            // This edit already committed. The admission deadline for
                            // a new write must not discard its runtime completion.
                            let _control = control::lock_room(&lock).await;
                            // HTTP reaction transactions can commit while another
                            // room's gate is busy. Re-read under that gate so a
                            // delayed completion never publishes an older list.
                            let latest;
                            let message = if reactions {
                                let Some(pool) = manager.db_pool.as_ref() else {
                                    return;
                                };
                                let (own, peer, id) = (
                                    message.participant_id.as_uuid(),
                                    recipient.as_uuid(),
                                    message.message_id.as_uuid(),
                                );
                                let Ok(Ok(Some(saved))) = tokio::time::timeout(
                                    control::PERSISTENCE_TIMEOUT,
                                    history::private_message(pool, own, peer, id),
                                )
                                .await
                                else {
                                    return;
                                };
                                latest = saved;
                                &latest
                            } else {
                                message
                            };
                            let reaction_event;
                            let event = if reactions {
                                let Ok(encoded) =
                                    serde_json::to_string(&ServerMessage::MessageReactions {
                                        message_id: message.message_id,
                                        reactions: message.reactions.clone(),
                                    })
                                else {
                                    return;
                                };
                                reaction_event = crate::OutboundJson::from(encoded);
                                &reaction_event
                            } else {
                                event
                            };
                            let mut room = lock.write().await;
                            let Ok(_admission) = manager.drain.admit() else {
                                return;
                            };
                            let current_rooms =
                                manager.rooms.read().unwrap_or_else(|e| e.into_inner());
                            if room.deleting
                                || !current_rooms
                                    .get(&id)
                                    .is_some_and(|current| Arc::ptr_eq(current, &lock))
                                || (!room.participants.contains_key(&message.participant_id)
                                    && !room.participants.contains_key(recipient))
                            {
                                return;
                            }
                            if reactions {
                                room.social.reconcile_reactions(message);
                            } else {
                                room.social.edit_message(message);
                            }
                            for person in room.participants.values().filter(|p| {
                                p.authenticated
                                    && (p.id == message.participant_id || p.id == *recipient)
                            }) {
                                let peer = if person.id == *recipient {
                                    message.participant_id
                                } else {
                                    *recipient
                                };
                                if person.social.accepts_inbox_from(&peer) {
                                    let _ = try_send_essential(
                                        &manager.metrics,
                                        &person.sender,
                                        event.clone(),
                                    );
                                }
                            }
                        }
                    });
                tokio::select! {
                    biased;
                    _ = manager.drain.wait() => {},
                    () = completion => {},
                }
            }
            .in_current_span(),
        ))
    }

    /// Observe completion when the HTTP caller stays connected. Dropping this
    /// observer detaches the owned task, preserving a committed edit's fan-out.
    pub(crate) async fn deliver_inbox_edit(&self, message: &ChatEntry) {
        self.account_updates().private_message(message);
        if let Some(completion) = self.schedule_inbox_edit(message)
            && let Err(error) = completion.await
        {
            warn!(%error, "Committed private edit reconciliation failed");
        }
    }

    /// The mutation is already committed; the owned completion survives its
    /// caller disappearing and reconciles each room from current durable state.
    pub(crate) async fn deliver_inbox_reactions(&self, message: &ChatEntry) {
        self.account_updates().private_message(message);
        if let Some(completion) = self.schedule_inbox_update(message, true)
            && let Err(error) = completion.await
        {
            warn!(%error, "Committed private reaction reconciliation failed");
        }
    }

    #[cfg(test)]
    fn process_social_chat(
        room: &mut Room,
        sender_id: &ParticipantId,
        expected_sender: &mpsc::Sender<crate::OutboundJson>,
        content: String,
        client_message_id: Option<String>,
        recipient_id: Option<&ParticipantId>,
    ) -> Result<()> {
        Self::process_chat_attempt(
            room,
            sender_id,
            expected_sender,
            ChatAttempt {
                content,
                attachment_ids: Vec::new(),
                client_message_id,
                recipient_id: recipient_id.copied(),
                sequence: None,
                retry_session: None,
                reply_to: None,
            },
        )
    }

    fn prepare_chat_attempt(
        room: &mut Room,
        sender_id: &ParticipantId,
        expected_sender: &mpsc::Sender<crate::OutboundJson>,
        attempt: ChatAttempt,
    ) -> Result<Option<PreparedChat>> {
        Self::prepare_chat_attempt_with_saved_reply(room, sender_id, expected_sender, attempt, None)
    }

    fn prepare_chat_attempt_with_saved_reply(
        room: &mut Room,
        sender_id: &ParticipantId,
        expected_sender: &mpsc::Sender<crate::OutboundJson>,
        attempt: ChatAttempt,
        saved_reply: Option<&ChatEntry>,
    ) -> Result<Option<PreparedChat>> {
        let ChatAttempt {
            content,
            attachment_ids,
            client_message_id,
            recipient_id,
            sequence,
            retry_session,
            reply_to,
        } = attempt;
        let recipient_id = recipient_id.as_ref();
        if (content.trim().is_empty() && attachment_ids.is_empty()) || content.len() > 4096 {
            return Err(invalid("Message must contain 1–4096 bytes of text"));
        }
        if !crate::labels::is_plain_text(&content) {
            return Err(invalid("Message contains hidden formatting characters"));
        }
        if !crate::attachments::valid_ids(&attachment_ids) {
            return Err(invalid("Attach at most four different files"));
        }
        let client_message_id = client_message_id.unwrap_or_else(|| Uuid::new_v4().to_string());
        if !valid_correlation_id(&client_message_id) {
            return Err(invalid("Invalid message ID"));
        }
        if sequence.is_some_and(|value| value == 0 || value > MAX_CHAT_SEQUENCE) {
            return Err(invalid("Invalid chat attempt"));
        }
        room.ensure_live()?;
        let now = std::time::Instant::now();
        room.social.prune_receipts(now);
        let sender = Self::participant_for_sender(room, sender_id, expected_sender)?;
        if !attachment_ids.is_empty() && !sender.authenticated {
            return Err(forbidden("Sign in to attach files"));
        }
        let metrics = room.metrics.clone();
        let unknown = |reason| {
            send(
                &metrics,
                expected_sender,
                &ServerMessage::MessageRetryResult {
                    client_message_id: client_message_id.clone(),
                    outcome: ChatRetryOutcome::Unknown,
                    reason,
                },
            )
        };
        if retry_session.as_ref().is_some_and(|session| {
            session.parse::<Uuid>().ok() != Some(sender.social.chat_session_id)
        }) {
            return unknown(ChatRetryReason::SessionChanged).map(|()| None);
        }
        let sender_session = sender.media_session_id;
        if let Some(existing) = room.social.receipts.iter().find(|entry| {
            entry.sender_session == sender_session
                && entry.message.client_message_id == client_message_id
        }) {
            if (existing.message.removed_at.is_none()
                && existing.message.edited_at.is_none()
                && (existing.message.content != content
                    || replied_to(&existing.message) != reply_to))
                || (existing.message.removed_at.is_none()
                    && existing
                        .message
                        .attachments
                        .iter()
                        .map(|file| file.id)
                        .collect::<Vec<_>>()
                        != attachment_ids)
                || existing.message.recipient_id.as_ref() != recipient_id
                || existing.sequence != sequence
            {
                if retry_session.is_none() {
                    return Err(invalid("Message ID was already used"));
                }
                return unknown(ChatRetryReason::Conflict).map(|()| None);
            }
            return acknowledge_chat(&room.metrics, expected_sender, existing.message.clone())
                .map(|()| None);
        }
        // This watermark survives receipt expiry and history eviction for the
        // exact membership. Missing older attempts must never be broadcast again.
        if let Some(sequence) = sequence
            && sequence <= sender.social.chat_high_water
        {
            return unknown(if sequence == sender.social.chat_high_water {
                ChatRetryReason::ReceiptExpired
            } else {
                ChatRetryReason::SequenceSuperseded
            })
            .map(|()| None);
        }
        // Without a receipt the original recipient's membership is unknown.
        // Never deliver retained private text into a replacement membership.
        if retry_session.is_some() && recipient_id.is_some() {
            return unknown(ChatRetryReason::RecipientUnconfirmed).map(|()| None);
        }
        let moderated = room.settings.as_ref().is_some_and(|s| s.moderated);
        if room.settings.as_ref().is_some_and(|s| !s.allow_chat)
            || !moderation::can_chat(&sender.punitive, sender.role, moderated)
        {
            return Err(forbidden("You are not allowed to chat"));
        }
        let sender_authenticated = sender.authenticated;
        let sender_name = sender.name.clone();
        let sender_style = sender.chat_style.clone();
        if let Some(existing) = room.social.history.iter().find(|entry| {
            entry.sender_session == sender_session
                && entry.message.client_message_id == client_message_id
        }) {
            if (existing.message.removed_at.is_none()
                && existing.message.edited_at.is_none()
                && (existing.message.content != content
                    || replied_to(&existing.message) != reply_to))
                || (existing.message.removed_at.is_none()
                    && existing
                        .message
                        .attachments
                        .iter()
                        .map(|file| file.id)
                        .collect::<Vec<_>>()
                        != attachment_ids)
                || existing.message.recipient_id.as_ref() != recipient_id
            {
                return Err(invalid("Message ID was already used"));
            }
            return acknowledge_chat(&room.metrics, expected_sender, existing.message.clone())
                .map(|()| None);
        }
        let recipient = if let Some(id) = recipient_id {
            let recipient = room
                .participants
                .get(id)
                .ok_or_else(|| unavailable("Private message unavailable"))?;
            if !can_private_message(sender, recipient) {
                return Err(unavailable("Private message unavailable"));
            }
            Some((
                recipient.name.clone(),
                recipient.media_session_id,
                recipient.sender.clone(),
            ))
        } else {
            None
        };
        let reply_to = match reply_to {
            Some(id) => Some(
                quote_reply(&room.social.history, sender, recipient_id, &id).or_else(|error| {
                    if recipient_id.is_none()
                        && let Some(original) =
                            saved_reply.filter(|message| message.message_id == id)
                    {
                        return saved_public_reply(original, sender);
                    }
                    Err(error)
                })?,
            ),
            None => None,
        };
        let message = ChatEntry {
            message_id: MessageId::random(),
            client_message_id: client_message_id.clone(),
            participant_id: *sender_id,
            participant_name: sender_name,
            recipient_id: recipient_id.copied(),
            account_conversation: None,
            recipient_name: recipient.as_ref().map(|p| p.0.clone()),
            content,
            attachments: Vec::new(),
            sent_at: chrono::Utc::now(),
            chat_style: sender_style,
            reply_to,
            removed_at: None,
            revision: 0,
            edited_at: None,
            reactions: Vec::new(),
        };
        let receipt_bytes = serde_json::to_vec(&message)?.len();
        if receipt_bytes > CHAT_RECEIPT_BYTES {
            if retry_session.is_some() {
                return unknown(ChatRetryReason::Capacity).map(|()| None);
            }
            return Err(invalid("Chat confirmation exceeds its size limit"));
        }
        // A private message reaches one bounded queue; only public fan-out
        // draws on the room-wide window. Rejecting a send must not evict an
        // accepted message's confirmation receipt.
        if recipient.is_none() && !room.chat_broadcasts.reserve(now) {
            return Err(rate_limited("Room chat rate limit exceeded"));
        }
        let persist = if recipient_id.is_some() {
            sender_authenticated
                && recipient_id
                    .and_then(|id| room.participants.get(id))
                    .is_some_and(|p| p.authenticated)
        } else {
            room.persisted
                && room
                    .settings
                    .as_ref()
                    .is_some_and(|settings| settings.history_retention_days > 0)
        };
        Ok(Some(PreparedChat {
            message,
            attachment_ids,
            sender_session,
            sender_authenticated,
            recipient,
            sequence,
            receipt_bytes,
            now,
            persist,
            durable: false,
        }))
    }

    fn process_chat_attempt(
        room: &mut Room,
        sender_id: &ParticipantId,
        expected_sender: &mpsc::Sender<crate::OutboundJson>,
        attempt: ChatAttempt,
    ) -> Result<()> {
        if let Some(prepared) =
            Self::prepare_chat_attempt(room, sender_id, expected_sender, attempt)?
        {
            Self::finish_chat_attempt(room, sender_id, expected_sender, prepared)?;
        }
        Ok(())
    }

    fn finish_chat_attempt(
        room: &mut Room,
        sender_id: &ParticipantId,
        expected_sender: &mpsc::Sender<crate::OutboundJson>,
        prepared: PreparedChat,
    ) -> Result<()> {
        let PreparedChat {
            message,
            sender_session,
            sender_authenticated,
            recipient,
            sequence,
            receipt_bytes,
            now,
            durable,
            ..
        } = prepared;
        if let Some((_, _, recipient_sender)) = &recipient {
            // Only acknowledged when the recipient's live connection accepted delivery.
            let delivered = send(
                &room.metrics,
                recipient_sender,
                &ServerMessage::PrivateMessageReceived {
                    message: message.clone(),
                },
            );
            if !durable {
                delivered?;
            }
        } else {
            let event = ServerMessage::ChatReceived {
                participant_id: message.participant_id,
                participant_name: message.participant_name.clone(),
                content: message.content.clone(),
                attachments: message.attachments.clone(),
                message_id: message.message_id,
                client_message_id: message.client_message_id.clone(),
                sent_at: message.sent_at,
                removed_at: message.removed_at,
                revision: message.revision,
                edited_at: message.edited_at,
                chat_style: message.chat_style.clone(),
                reply_to: message.reply_to.clone(),
            };
            let json = crate::OutboundJson::from(serde_json::to_string(&event)?);
            for participant in room.participants.values() {
                if participant.id != *sender_id && !participant.social.ignored.contains(sender_id) {
                    let _ = try_send_essential(&room.metrics, &participant.sender, json.clone());
                }
            }
        }
        room.social.make_receipt_room(sender_session, receipt_bytes);
        room.social.receipt_bytes += receipt_bytes;
        room.social.receipts.push_back(ChatReceipt {
            sender_session,
            sequence,
            accepted_at: now,
            bytes: receipt_bytes,
            message: message.clone(),
        });
        if let Some(sequence) = sequence {
            room.participants
                .get_mut(sender_id)
                .expect("current sender checked")
                .social
                .chat_high_water = sequence;
        }
        room.social.remember(
            sender_session,
            recipient.as_ref().map(|r| r.1),
            sender_authenticated,
            message.clone(),
        );
        acknowledge_chat(&room.metrics, expected_sender, message)
    }

    pub(crate) async fn handle_social_request(
        &self,
        room_id: &RoomId,
        participant_id: &ParticipantId,
        expected_sender: &mpsc::Sender<crate::OutboundJson>,
        command: &ClientMessage,
    ) -> Result<()> {
        let (request_id, _) = command
            .social_request()
            .ok_or_else(|| not_found("Unknown request"))?;
        if !valid_correlation_id(request_id) {
            return Err(invalid("Invalid request ID"));
        }
        if let ClientMessage::GetRoomSnapshot { request_id } = command {
            let room_lock = self.get_room(room_id)?;
            let room = room_lock.read().await;
            room.ensure_live()?;
            let actor = Self::participant_for_sender(&room, participant_id, expected_sender)?;
            let snapshot = self.room_snapshot(&room, actor);
            drop(room);
            return send(
                &self.metrics,
                expected_sender,
                &ServerMessage::SocialResponse {
                    request_id: request_id.clone(),
                    response: SocialResponse::GetRoomSnapshot(snapshot),
                },
            );
        }
        // PM consent changes share control with durable sends, so their ACK
        // cannot overtake a send while it waits for its database commit.
        if matches!(command, ClientMessage::ChangeNickname { .. }) {
            // Keep the large shared dispatcher's state off the signaling
            // task's stack on this direct, non-owned call path.
            return Box::pin(self.handle_social_request_inner(
                room_id,
                participant_id,
                expected_sender,
                command,
                None,
            ))
            .await;
        }

        let participant_id = participant_id.to_owned();
        let sender = expected_sender.clone();
        let command = command.clone();
        self.run_room_control(room_id, move |manager, room_id, control| async move {
            manager
                .handle_social_request_inner(
                    &room_id,
                    &participant_id,
                    &sender,
                    &command,
                    Some(control),
                )
                .await
        })
        .await
    }

    fn room_snapshot(&self, room: &Room, actor: &Participant) -> RoomSnapshot {
        RoomSnapshot {
            participants: room
                .participants
                .values()
                .filter(|p| p.id != actor.id)
                .map(|p| ParticipantInfo {
                    id: p.id,
                    name: p.name.clone(),
                    role: p.role,
                    authenticated: p.authenticated,
                    chat_style: p.chat_style.clone(),
                    producers: p
                        .producers
                        .iter()
                        .map(|(id, (kind, source))| ProducerMetadata {
                            id: id.clone(),
                            kind: *kind,
                            source: source.clone(),
                        })
                        .collect(),
                })
                .collect(),
            messages: room
                .social
                .history
                .iter()
                .filter(|entry| visible(entry, actor))
                .map(|entry| entry.message.clone())
                .collect(),
            lobby: if actor.role >= roles::Role::Moderator {
                room.lobby
                    .values()
                    .map(|p| SnapshotLobbyEntry {
                        participant_id: p.participant_id,
                        display_name: p.name.clone(),
                        authenticated: p.authenticated,
                    })
                    .collect()
            } else {
                Vec::new()
            },
            your_role: actor.role,
            chat_session_id: actor.social.chat_session_id,
            room_settings: room.settings.clone(),
            nickname: actor.name.clone(),
            allow_private_messages: actor.social.allow_private_messages,
            ignored_participant_ids: actor.social.ignored.iter().cloned().collect(),
            paused_producer_ids: room
                .participants
                .values()
                .flat_map(|p| p.producers.keys())
                .filter(|id| {
                    self.media_server
                        .transport_manager()
                        .find_producer_paused(id)
                        == Some(true)
                })
                .cloned()
                .collect(),
            text_muted: actor.punitive.text_muted,
            cam_banned: actor.punitive.cam_banned,
            can_chat: !room.settings.as_ref().is_some_and(|s| !s.allow_chat)
                && moderation::can_chat(
                    &actor.punitive,
                    actor.role,
                    room.settings.as_ref().is_some_and(|s| s.moderated),
                ),
            local_producer_ids: actor.producers.keys().cloned().collect(),
            can_broadcast: Self::participant_can_produce(
                room,
                actor,
                MediaKind::Audio,
                "microphone",
            ),
        }
    }

    async fn handle_social_request_inner(
        &self,
        room_id: &RoomId,
        participant_id: &ParticipantId,
        expected_sender: &mpsc::Sender<crate::OutboundJson>,
        command: &ClientMessage,
        control: Option<super::control::RoomControl>,
    ) -> Result<()> {
        let (request_id, _) = command
            .social_request()
            .ok_or_else(|| not_found("Unknown request"))?;
        if !valid_correlation_id(request_id) {
            return Err(invalid("Invalid request ID"));
        }
        // Control admission keeps the online/offline decision stable. Reuse the
        // already-controlled role path rather than trying to acquire this gate again.
        if let ClientMessage::SetMemberRole {
            target_user_id,
            role,
            ..
        } = command
        {
            let new_role = *role;
            let room_lock = self.get_room(room_id)?;
            let room = room_lock.read().await;
            Self::participant_for_sender(&room, participant_id, expected_sender)?;
            let online = room
                .participants
                .contains_key(&ParticipantId::from(target_user_id.as_uuid()));
            drop(room);
            if online {
                self.set_participant_role_controlled(
                    room_id,
                    participant_id,
                    expected_sender,
                    &ParticipantId::from(target_user_id.as_uuid()),
                    new_role,
                    control.ok_or_else(|| unavailable("Room control is unavailable"))?,
                )
                .await?;
                return send(
                    &self.metrics,
                    expected_sender,
                    &ServerMessage::SocialResponse {
                        request_id: request_id.to_string(),
                        response: SocialResponse::SetMemberRole { updated: true },
                    },
                );
            }
        }
        let room_lock = self.get_room(room_id)?;
        let mut room = room_lock.write().await;
        room.ensure_live()?;
        let actor = Self::participant_for_sender(&room, participant_id, expected_sender)?;
        let actor_role = actor.role;
        let actor_name = actor.name.clone();
        let actor_authenticated = actor.authenticated;
        match command.social_budget() {
            SocialBudget::None => {}
            SocialBudget::ChatBroadcast => {
                if !room.chat_broadcasts.reserve(std::time::Instant::now()) {
                    return Err(rate_limited("Room chat rate limit exceeded"));
                }
            }
            SocialBudget::AdminMutation => {
                if !room.admin_mutations.reserve(std::time::Instant::now()) {
                    return Err(rate_limited("Room requests are rate limited"));
                }
            }
        }
        let response = match command {
            ClientMessage::GetChatHistory {
                before,
                after,
                around,
                resume,
                q,
                limit,
                ..
            } => {
                let days = room
                    .settings
                    .as_ref()
                    .map_or(0, |settings| settings.history_retention_days);
                let query = history::HistoryQuery {
                    before: before.clone(),
                    after: after.clone(),
                    around: *around,
                    resume: *resume,
                    q: q.clone(),
                    limit: *limit,
                };
                query.validate().map_err(rejected)?;
                if days == 0 || !room.persisted {
                    SocialResponse::GetChatHistory(history::HistoryPage {
                        messages: Vec::new(),
                        next_cursor: None,
                        newer_cursor: None,
                        first_unread_message_id: None,
                        read_message_id: None,
                        retention_days: 0,
                    })
                } else {
                    let pool = self
                        .db_pool
                        .as_ref()
                        .ok_or_else(|| unavailable("History unavailable"))?;
                    let account = actor_authenticated.then(|| participant_id.as_uuid());
                    drop(room);
                    let mut page = tokio::time::timeout(
                        control::PERSISTENCE_TIMEOUT,
                        history::page(
                            pool,
                            &history::public_conversation(room_id.as_str()),
                            account,
                            days,
                            &query,
                        ),
                    )
                    .await
                    .map_err(|_| timed_out("History request timed out"))??;
                    room = room_lock.write().await;
                    room.ensure_live()?;
                    Self::participant_for_sender(&room, participant_id, expected_sender)?;
                    let ignored = &room
                        .participants
                        .get(participant_id)
                        .unwrap()
                        .social
                        .ignored;
                    page.messages
                        .retain(|message| !ignored.contains(&message.participant_id));
                    SocialResponse::GetChatHistory(page)
                }
            }
            ClientMessage::MarkChatRead { message_id, .. } => {
                if !actor_authenticated
                    || !room.persisted
                    || !room
                        .settings
                        .as_ref()
                        .is_some_and(|settings| settings.history_retention_days > 0)
                {
                    return Err(unavailable("Saved room history is unavailable"));
                }
                let account = participant_id.as_uuid();
                let message = message_id.as_uuid();
                let pool = self
                    .db_pool
                    .as_ref()
                    .ok_or_else(|| unavailable("History unavailable"))?;
                drop(room);
                let read = tokio::time::timeout(
                    control::PERSISTENCE_TIMEOUT,
                    history::mark_read(
                        pool,
                        &history::public_conversation(room_id.as_str()),
                        account,
                        message,
                    ),
                )
                .await
                .map_err(|_| failed("Read marker could not be saved"))?
                .map_err(|error| {
                    if matches!(error, sqlx::Error::RowNotFound) {
                        unavailable("Message unavailable")
                    } else {
                        error.into()
                    }
                })?;
                room = room_lock.write().await;
                room.ensure_live()?;
                Self::participant_for_sender(&room, participant_id, expected_sender)?;
                SocialResponse::MarkChatRead {
                    read_message_id: Some(read.read_message_id),
                }
            }
            ClientMessage::SetRoomHistory { retention_days, .. } => {
                require_role(actor_role, roles::Role::Owner)?;
                if !room.persisted {
                    return Err(forbidden("Only saved rooms can keep chat history"));
                }
                if ![0, 1, 7, 30, 90].contains(retention_days) {
                    return Err(invalid("Choose off, 1, 7, 30 or 90 days"));
                }
                let pool = self
                    .db_pool
                    .as_ref()
                    .ok_or_else(|| unavailable("History unavailable"))?;
                drop(room);
                self.persist_room(
                    room_id,
                    &room_lock,
                    history::set_retention(pool, room_id.as_str(), *retention_days),
                )
                .await?;
                room = room_lock.write().await;
                room.ensure_live()?;
                Self::participant_for_sender(&room, participant_id, expected_sender)?;
                let settings = room
                    .settings
                    .as_mut()
                    .ok_or_else(|| unavailable("Room settings unavailable"))?;
                settings.history_retention_days = *retention_days;
                let settings = settings.clone();
                room.broadcast_all(&ServerMessage::RoomSettingsChanged { settings });
                if *retention_days == 0 {
                    room.social.pins.clear();
                }
                publish_pins(&room)?;
                SocialResponse::SetRoomHistory {
                    retention_days: *retention_days,
                }
            }
            ClientMessage::SetChatPreferences {
                allow_private_messages,
                ignored_participant_ids,
                ..
            } => {
                if ignored_participant_ids.len() > MAX_IGNORED
                    || ignored_participant_ids
                        .iter()
                        .any(|id| id == participant_id)
                {
                    return Err(invalid(
                        "Choose at most 100 valid participant identities to ignore",
                    ));
                }
                if actor_authenticated {
                    let pool = self
                        .db_pool
                        .as_ref()
                        .ok_or_else(|| unavailable("Account preferences unavailable"))?;
                    let account = participant_id.as_uuid();
                    drop(room);
                    let preferences = tokio::time::timeout(
                        control::PERSISTENCE_TIMEOUT,
                        crate::auth::account::load_chat_preferences(pool, account),
                    )
                    .await
                    .map_err(|_| timed_out("Account preferences timed out"))??;
                    room = room_lock.write().await;
                    room.ensure_live()?;
                    let actor =
                        Self::participant_for_sender(&room, participant_id, expected_sender)?;
                    let guests = ignored_participant_ids
                        .iter()
                        .filter(|id| {
                            room.participants
                                .get(*id)
                                .is_some_and(|person| !person.authenticated)
                                || actor.social.guest_ignored.contains(*id)
                        })
                        .cloned()
                        .collect();
                    let actor = room.participants.get_mut(participant_id).unwrap();
                    actor.social.guest_ignored = guests;
                    actor.social.apply_account_preferences(&preferences);
                } else {
                    let actor = room.participants.get_mut(participant_id).unwrap();
                    actor.social.allow_private_messages = *allow_private_messages;
                    actor.social.ignored = ignored_participant_ids.iter().cloned().collect();
                }
                let actor = room.participants.get(participant_id).unwrap();
                SocialResponse::SetChatPreferences {
                    allow_private_messages: actor.social.allow_private_messages,
                    ignored_participant_ids: actor.social.ignored.iter().cloned().collect(),
                }
            }
            ClientMessage::ChangeNickname { nickname, .. } => {
                let nickname = nickname.trim();
                if nickname.is_empty()
                    || nickname.len() > 64
                    || !crate::labels::is_plain(nickname)
                    || crate::labels::is_reserved_name(nickname)
                {
                    return Err(invalid(
                        "Nickname must be 1–64 bytes of plain text; \"You\" is taken",
                    ));
                }
                if room.name_in_use(nickname, Some(participant_id)) {
                    return Err(invalid("That name is already in use in this room"));
                }
                room.participants.get_mut(participant_id).unwrap().name = nickname.to_string();
                room.broadcast_all(&ServerMessage::NicknameChanged {
                    participant_id: *participant_id,
                    nickname: nickname.to_string(),
                });
                SocialResponse::ChangeNickname {
                    nickname: nickname.to_string(),
                }
            }
            ClientMessage::SetChatStyle { chat_style, .. } => {
                let chat_style = chat_style
                    .validated()
                    .ok_or_else(|| invalid("Choose a chat color from the palette"))?;
                if actor_authenticated && let Some(pool) = self.db_pool.clone() {
                    let user: Uuid = participant_id.as_uuid();
                    // The account keeps the look before anyone sees it.
                    drop(room);
                    tokio::time::timeout(
                        control::PERSISTENCE_TIMEOUT,
                        save_chat_style(&pool, user, &chat_style),
                    )
                    .await
                    .map_err(|_| failed("Your chat look could not be saved"))?
                    .map_err(|_| failed("Your chat look could not be saved"))?;
                    room = room_lock.write().await;
                    room.ensure_live()?;
                    Self::participant_for_sender(&room, participant_id, expected_sender)?;
                }
                room.participants
                    .get_mut(participant_id)
                    .ok_or_else(|| not_found("Participant not found"))?
                    .chat_style = chat_style.clone();
                room.broadcast_all(&ServerMessage::ChatStyleChanged {
                    participant_id: *participant_id,
                    chat_style: chat_style.clone(),
                });
                SocialResponse::SetChatStyle {
                    chat_style: chat_style.clone(),
                }
            }
            ClientMessage::EditChatMessage {
                message_id,
                content,
                expected_revision,
                ..
            } => {
                let account: Uuid = participant_id.as_uuid();
                let request = history::EditRequest {
                    content: content.clone(),
                    expected_revision: *expected_revision,
                };
                let actor = Self::participant_for_sender(&room, participant_id, expected_sender)?;
                let guest_session = (!actor.authenticated).then_some(actor.media_session_id);
                let mut edited = prepare_edit(&room, actor, message_id, &request)?;
                let keeps_history = room.persisted
                    && room
                        .settings
                        .as_ref()
                        .is_some_and(|s| s.history_retention_days > 0);
                if edited.is_none() && keeps_history {
                    let pool = self
                        .db_pool
                        .as_ref()
                        .ok_or_else(|| unavailable("History unavailable"))?;
                    drop(room);
                    let found = tokio::time::timeout(
                        control::PERSISTENCE_TIMEOUT,
                        history::lookup_public(pool, room_id.as_str(), message_id),
                    )
                    .await
                    .map_err(|_| timed_out("History request timed out"))??;
                    edited = found.map(|(message, _)| message);
                    room = room_lock.write().await;
                    room.ensure_live()?;
                    Self::participant_for_sender(&room, participant_id, expected_sender)?;
                }
                let mut edited = edited.ok_or_else(|| unavailable("Message unavailable"))?;
                if edited.participant_id != *participant_id {
                    return Err(forbidden("Only the author can edit this message"));
                }
                let durable = self.db_pool.is_some()
                    && if let Some(peer) = &edited.recipient_id {
                        actor_authenticated
                            && room.participants.get(peer).is_some_and(|p| p.authenticated)
                    } else {
                        keeps_history
                    };
                if durable {
                    let pool = self.db_pool.as_ref().unwrap();
                    drop(room);
                    edited = self
                        .persist_room(
                            room_id,
                            &room_lock,
                            history::edit_saved(
                                pool,
                                Some(room_id.as_str()),
                                account,
                                guest_session,
                                &edited,
                                &request,
                            ),
                        )
                        .await
                        .map_err(|error| {
                            if let sqlx::Error::InvalidArgument(message) = error {
                                rejected(&message)
                            } else {
                                error.into()
                            }
                        })?;
                    self.account_updates().private_message(&edited);
                    room = room_lock.write().await;
                    room.ensure_live()?;
                    Self::participant_for_sender(&room, participant_id, expected_sender)?;
                } else {
                    history::apply_edit(&mut edited, &request).map_err(rejected)?;
                }
                publish_edit(&mut room, &edited)?;
                drop(self.schedule_inbox_edit(&edited));
                SocialResponse::EditChatMessage { message: edited }
            }
            ClientMessage::GetPinnedMessages { .. } | ClientMessage::SetPinnedMessage { .. } => {
                let change = if let ClientMessage::SetPinnedMessage {
                    message_id, pinned, ..
                } = command
                {
                    require_role(actor_role, roles::Role::Moderator)?;
                    Some((message_id, *pinned))
                } else {
                    None
                };
                let durable = room.persisted
                    && room
                        .settings
                        .as_ref()
                        .is_some_and(|s| s.history_retention_days > 0);
                if durable {
                    let pool = self
                        .db_pool
                        .as_ref()
                        .ok_or_else(|| unavailable("History unavailable"))?;
                    drop(room);
                    if let Some((message, pinned)) = change {
                        self.persist_room(
                            room_id,
                            &room_lock,
                            history::set_pin(pool, room_id.as_str(), message, pinned),
                        )
                        .await
                        .map_err(|error| {
                            if let sqlx::Error::InvalidArgument(message) = error {
                                rejected(&message)
                            } else {
                                error.into()
                            }
                        })?;
                    }
                    let pins = tokio::time::timeout(
                        control::PERSISTENCE_TIMEOUT,
                        history::pinned_messages(pool, room_id.as_str()),
                    )
                    .await
                    .map_err(|_| timed_out("Pinned messages request timed out"))??;
                    room = room_lock.write().await;
                    room.ensure_live()?;
                    Self::participant_for_sender(&room, participant_id, expected_sender)?;
                    room.social.pins = pins;
                } else if let Some((message, pinned)) = change {
                    if pinned && !room.social.pins.iter().any(|m| m.message_id == *message) {
                        if room.social.pins.len() >= 3 {
                            return Err(forbidden(
                                "Unpin a message before pinning another; rooms keep up to three",
                            ));
                        }
                        let original = room
                            .social
                            .history
                            .iter()
                            .find(|entry| {
                                entry.message.message_id == *message
                                    && entry.message.recipient_id.is_none()
                                    && entry.message.removed_at.is_none()
                            })
                            .ok_or_else(|| unavailable("Public message unavailable"))?
                            .message
                            .clone();
                        room.social.pins.insert(0, original);
                    } else if !pinned {
                        room.social.pins.retain(|m| m.message_id != *message);
                    }
                }
                if change.is_some() {
                    publish_pins(&room)?;
                }
                let actor = Self::participant_for_sender(&room, participant_id, expected_sender)?;
                let messages: Vec<_> = room
                    .social
                    .pins
                    .iter()
                    .filter(|message| !actor.social.ignored.contains(&message.participant_id))
                    .cloned()
                    .collect();
                if matches!(command, ClientMessage::SetPinnedMessage { .. }) {
                    SocialResponse::SetPinnedMessage { messages }
                } else {
                    SocialResponse::GetPinnedMessages { messages }
                }
            }
            ClientMessage::RemoveChatMessage { message_id, .. } => {
                require_role(actor_role, roles::Role::Moderator)?;
                let mut original = room
                    .social
                    .history
                    .iter()
                    .find(|entry| {
                        entry.message.message_id == *message_id
                            && entry.message.recipient_id.is_none()
                    })
                    .map(|entry| (entry.message.clone(), entry.sender_authenticated));
                let persisted = room.persisted;
                let keeps_history = room
                    .settings
                    .as_ref()
                    .is_some_and(|s| s.history_retention_days > 0);
                let pool = self.db_pool.clone();
                if original.is_none() && persisted && keeps_history {
                    let pool = pool
                        .as_ref()
                        .ok_or_else(|| unavailable("Chat history unavailable"))?;
                    drop(room);
                    original = tokio::time::timeout(
                        control::PERSISTENCE_TIMEOUT,
                        super::history::lookup_public(pool, room_id.as_str(), message_id),
                    )
                    .await
                    .map_err(|_| timed_out("Chat history timed out"))??;
                    room = room_lock.write().await;
                    room.ensure_live()?;
                    Self::participant_for_sender(&room, participant_id, expected_sender)?;
                }
                let (original, authenticated) =
                    original.ok_or_else(|| not_found("Message not found"))?;
                let removed_at = original.removed_at.unwrap_or_else(chrono::Utc::now);
                if original.removed_at.is_none() {
                    let event = ModerationEvent::new(
                        ModerationAction::MessageRemoved,
                        Actor {
                            id: participant_id,
                            name: &actor_name,
                        },
                        Target {
                            id: original.participant_id.into(),
                            name: &original.participant_name,
                            authenticated,
                            ip: None,
                        },
                        Some(&format!("Message {message_id}")),
                        None,
                        None,
                    );
                    if persisted {
                        let pool = pool
                            .as_ref()
                            .ok_or_else(|| unavailable("Moderation unavailable"))?;
                        drop(room);
                        self.persist_room(room_id, &room_lock, async {
                            let mut transaction = pool.begin().await?;
                            if keeps_history {
                                super::history::remove_public(
                                    &mut transaction,
                                    room_id.as_str(),
                                    message_id,
                                    removed_at,
                                )
                                .await?;
                            }
                            sqlx::query("DELETE FROM attachments WHERE room_id=$1 AND ephemeral_message_id=$2")
                                .bind(room_id).bind(message_id.as_uuid())
                                .execute(&mut *transaction).await?;
                            record_event(&mut transaction, room_id, &event, MAX_MODERATION_EVENTS)
                                .await?;
                            transaction.commit().await
                        })
                        .await?;
                        room = room_lock.write().await;
                        room.ensure_live()?;
                    } else {
                        if let Some(pool) = &pool {
                            drop(room);
                            self.persist_room(room_id,&room_lock,async {
                                sqlx::query("DELETE FROM attachments WHERE room_id=$1 AND ephemeral_message_id=$2")
                                    .bind(room_id).bind(message_id.as_uuid())
                                    .execute(pool).await.map(|_|())
                            }).await?;
                            room = room_lock.write().await;
                            room.ensure_live()?;
                        }
                        room.social.record_moderation_event(event);
                    }
                }
                room.social.remove_message(message_id, removed_at);
                room.broadcast_all(&ServerMessage::ChatMessageRemoved {
                    message_id: *message_id,
                    removed_at,
                });
                publish_pins(&room)?;
                SocialResponse::RemoveChatMessage {
                    message_id: *message_id,
                    removed_at,
                }
            }
            ClientMessage::ReactToMessage {
                message_id, emoji, ..
            } => {
                let saved_public = room.persisted
                    && room
                        .settings
                        .as_ref()
                        .is_some_and(|settings| settings.history_retention_days > 0)
                    && room
                        .social
                        .history
                        .iter()
                        .find(|entry| entry.message.message_id == *message_id)
                        .is_none_or(|entry| entry.message.recipient_id.is_none());
                if saved_public {
                    let ignored: Vec<_> = room
                        .participants
                        .get(participant_id)
                        .unwrap()
                        .social
                        .ignored
                        .iter()
                        .cloned()
                        .collect();
                    let pool = self
                        .db_pool
                        .as_ref()
                        .ok_or_else(|| unavailable("History unavailable"))?;
                    drop(room);
                    let saved = self
                        .persist_room(
                            room_id,
                            &room_lock,
                            history::react_public(
                                pool,
                                room_id.as_str(),
                                message_id,
                                participant_id,
                                &ignored,
                                emoji,
                            ),
                        )
                        .await
                        .map_err(|error| {
                            if let sqlx::Error::InvalidArgument(message) = error {
                                rejected(&message)
                            } else {
                                error.into()
                            }
                        })?;
                    room = room_lock.write().await;
                    room.ensure_live()?;
                    Self::participant_for_sender(&room, participant_id, expected_sender)?;
                    if let Some(message) = saved {
                        publish_saved_reaction(&mut room, &message)?;
                        return send(
                            &self.metrics,
                            expected_sender,
                            &ServerMessage::SocialResponse {
                                request_id: request_id.to_owned(),
                                response: SocialResponse::ReactToMessage {
                                    message_id: *message_id,
                                    reactions: message.reactions.clone(),
                                },
                            },
                        );
                    }
                }
                let reactions = prepare_reaction(&room, participant_id, message_id, emoji)?;
                let private_message = room
                    .social
                    .history
                    .iter()
                    .find(|entry| entry.message.message_id == *message_id)
                    .map(|entry| entry.message.clone());
                if !saved_public && let Some(pool) = &self.db_pool {
                    drop(room);
                    self.persist_room(
                        room_id,
                        &room_lock,
                        history::save_reactions(pool, message_id, &reactions),
                    )
                    .await?;
                    if let Some(message) = &private_message {
                        self.account_updates().private_message(message);
                    }
                    room = room_lock.write().await;
                    room.ensure_live()?;
                    Self::participant_for_sender(&room, participant_id, expected_sender)?;
                }
                let reactions = publish_reaction(&mut room, message_id, reactions)?;
                SocialResponse::ReactToMessage {
                    message_id: *message_id,
                    reactions,
                }
            }
            ClientMessage::ListRoomBans { offset, .. } => {
                require_role(actor_role, roles::Role::Admin)?;
                let offset = page_offset(*offset)?;
                let mut bans: Vec<BanEntry> = if room.persisted {
                    let pool = self
                        .db_pool
                        .as_ref()
                        .ok_or_else(|| unavailable("Ban service unavailable"))?;
                    drop(room);
                    let rows: Vec<BanRow> = tokio::time::timeout(
                        control::PERSISTENCE_TIMEOUT,
                        sqlx::query_as(
                            "SELECT s.id,u.display_name,s.user_id IS NOT NULL,s.reason,s.expires_at FROM room_states s LEFT JOIN users u ON u.id=s.user_id WHERE s.room_id=$1 AND s.state='banned' AND (s.expires_at IS NULL OR s.expires_at>now()) ORDER BY s.created_at DESC,s.id LIMIT 101 OFFSET $2")
                            .bind(room_id).bind(offset as i64).fetch_all(pool),
                    ).await.map_err(|_| timed_out("Ban service timed out"))??;
                    room = room_lock.write().await;
                    room.ensure_live()?;
                    rows.into_iter()
                        .map(|(id, name, authenticated, reason, expiry)| BanEntry {
                            ban_id: id.to_string(),
                            display_name: name.unwrap_or_else(|| "Guest".to_string()),
                            authenticated,
                            reason,
                            expires_at: expiry.map(|e| e.to_rfc3339()),
                        })
                        .collect()
                } else {
                    room.social
                        .bans
                        .retain(|_, b| b.expiry.is_none_or(|e| e > std::time::Instant::now()));
                    let mut bans: Vec<_> =
                        room.social.bans.values().map(|b| b.entry.clone()).collect();
                    bans.sort_by(|a, b| a.ban_id.cmp(&b.ban_id));
                    bans.into_iter().skip(offset).take(PAGE_SIZE + 1).collect()
                };
                let has_more = bans.len() > PAGE_SIZE;
                bans.truncate(PAGE_SIZE);
                SocialResponse::ListRoomBans { bans, has_more }
            }
            ClientMessage::RemoveRoomBan { ban_id, .. } => {
                require_role(actor_role, roles::Role::Admin)?;
                let id: Uuid = ban_id.parse().map_err(|_| invalid("Invalid ban"))?;
                let (user_id, guest_ip, ids) = if room.persisted {
                    let pool = self
                        .db_pool
                        .as_ref()
                        .ok_or_else(|| unavailable("Ban service unavailable"))?;
                    drop(room);
                    let actor_id = *participant_id;
                    let actor_name = actor_name.clone();
                    let row: Option<(Option<Uuid>, Option<String>)> = self
                        .persist_room(room_id, &room_lock, async {
                            let mut transaction = pool.begin().await?;
                            let row: Option<(Option<Uuid>, Option<String>, Option<String>)> = sqlx::query_as("DELETE FROM room_states s WHERE s.room_id=$1 AND s.id=$2 AND s.state='banned' RETURNING s.user_id,host(s.ip_address),(SELECT u.display_name FROM users u WHERE u.id=s.user_id)")
                                .bind(room_id).bind(id).fetch_optional(&mut *transaction).await?;
                            let Some((uid, ip, name)) = row else {
                                return Ok(None);
                            };
                            let target_id = uid.map_or_else(
                                || ModerationTargetId::Ban(id.to_string()),
                                |uid| ModerationTargetId::from(ParticipantId::from(uid)),
                            );
                            let event = ModerationEvent::new(
                                ModerationAction::Unban,
                                Actor { id: &actor_id, name: &actor_name },
                                Target {
                                    id: target_id.clone(),
                                    name: name.as_deref().unwrap_or(if uid.is_some() { "Removed account" } else { "Guest" }),
                                    authenticated: uid.is_some(),
                                    ip: ip.as_deref().and_then(|ip| ip.parse().ok()),
                                },
                                None,
                                None,
                                None,
                            );
                            record_event(&mut transaction, room_id, &event, MAX_MODERATION_EVENTS).await?;
                            transaction.commit().await?;
                            Ok(Some((uid, ip)))
                        })
                        .await?;
                    room = room_lock.write().await;
                    room.ensure_live()?;
                    let (uid, ip) = row.ok_or_else(|| not_found("Ban not found"))?;
                    (
                        uid.map(ParticipantId::from),
                        if uid.is_none() {
                            ip.and_then(|ip| ip.parse::<IpAddr>().ok())
                        } else {
                            None
                        },
                        Vec::new(),
                    )
                } else {
                    let ban = room
                        .social
                        .bans
                        .remove(ban_id)
                        .ok_or_else(|| not_found("Ban not found"))?;
                    let target_id = ban
                        .entry
                        .authenticated
                        .then(|| ban.participant_ids.first().cloned())
                        .flatten();
                    room.social.record_moderation_event(ModerationEvent::new(
                        ModerationAction::Unban,
                        Actor {
                            id: participant_id,
                            name: &actor_name,
                        },
                        Target {
                            id: target_id.map_or_else(
                                || ModerationTargetId::Ban(ban_id.to_string()),
                                ModerationTargetId::from,
                            ),
                            name: &ban.entry.display_name,
                            authenticated: ban.entry.authenticated,
                            ip: ban.guest_ip,
                        },
                        None,
                        None,
                        None,
                    ));
                    (target_id, ban.guest_ip, ban.participant_ids)
                };
                if let Some(uid) = &user_id {
                    room.banned_participants.remove(uid);
                }
                if let Some(ip) = guest_ip {
                    room.banned_guest_ips.remove(&ip);
                }
                let mut affected = ids;
                room.social.bans.retain(|_, ban| {
                    let matches = user_id
                        .as_ref()
                        .is_some_and(|uid| ban.participant_ids.contains(uid))
                        || guest_ip.is_some_and(|ip| ban.guest_ip == Some(ip));
                    if matches {
                        affected.extend(ban.participant_ids.iter().cloned());
                    }
                    !matches
                });
                for id in affected {
                    room.banned_participants.remove(&id);
                    room.banned_guest_participants.remove(&id);
                }
                room.policy_revision = room.policy_revision.wrapping_add(1);
                SocialResponse::RemoveRoomBan { removed: true }
            }
            ClientMessage::ListRoomMembers { offset, .. } => {
                require_role(actor_role, roles::Role::Moderator)?;
                let offset = page_offset(*offset)?;
                let mut members: Vec<MemberEntry> = if room.persisted {
                    let pool = self
                        .db_pool
                        .as_ref()
                        .ok_or_else(|| unavailable("Member service unavailable"))?;
                    drop(room);
                    let rows: Vec<(Uuid, String, roles::Role)> = tokio::time::timeout(
                        control::PERSISTENCE_TIMEOUT,
                        sqlx::query_as("WITH roster AS (SELECT owner_id AS user_id,5::smallint AS role FROM rooms WHERE id=$1 UNION ALL SELECT rr.user_id,rr.role FROM room_roles rr JOIN rooms r ON r.id=rr.room_id WHERE rr.room_id=$1 AND rr.user_id<>r.owner_id) SELECT u.id,u.display_name,roster.role FROM roster JOIN users u ON u.id=roster.user_id ORDER BY u.display_name,u.id LIMIT 101 OFFSET $2")
                            .bind(room_id).bind(offset as i64).fetch_all(pool),
                    ).await.map_err(|_| timed_out("Member service timed out"))??;
                    room = room_lock.write().await;
                    room.ensure_live()?;
                    rows.into_iter()
                        .map(|(id, name, role)| MemberEntry {
                            online: room.participants.contains_key(&ParticipantId::from(id)),
                            user_id: UserId::from(id),
                            display_name: name,
                            role,
                            authenticated: true,
                        })
                        .collect()
                } else {
                    let mut members: Vec<_> = room
                        .participants
                        .values()
                        .map(|p| MemberEntry {
                            user_id: UserId::from(p.id.as_uuid()),
                            display_name: p.name.clone(),
                            role: p.role,
                            online: true,
                            authenticated: p.authenticated,
                        })
                        .collect();
                    members.sort_by(|a, b| {
                        a.display_name
                            .cmp(&b.display_name)
                            .then(a.user_id.cmp(&b.user_id))
                    });
                    members
                        .into_iter()
                        .skip(offset)
                        .take(PAGE_SIZE + 1)
                        .collect()
                };
                let has_more = members.len() > PAGE_SIZE;
                members.truncate(PAGE_SIZE);
                SocialResponse::ListRoomMembers { members, has_more }
            }
            ClientMessage::SetMemberRole {
                target_user_id,
                role,
                ..
            } => {
                require_role(actor_role, roles::Role::Moderator)?;
                if !room.persisted || !actor_authenticated {
                    return Err(forbidden("Offline roles require a persistent room"));
                }
                let target: Uuid = target_user_id.as_uuid();
                let setter: Uuid = participant_id.as_uuid();
                let new_role = *role;
                if new_role < roles::Role::User {
                    return Err(forbidden(
                        "Registered accounts require at least the user role",
                    ));
                }
                let pool = self
                    .db_pool
                    .as_ref()
                    .ok_or_else(|| unavailable("Member service unavailable"))?;
                drop(room);
                // Absent targets must already belong to this room's roster; no account enumeration.
                let current: Option<roles::Role> = tokio::time::timeout(
                    control::PERSISTENCE_TIMEOUT,
                    sqlx::query_scalar("SELECT CASE WHEN $2=owner_id THEN 5 ELSE (SELECT role FROM room_roles WHERE room_id=$1 AND user_id=$2) END::smallint FROM rooms WHERE id=$1 AND ($2=owner_id OR EXISTS(SELECT 1 FROM room_roles WHERE room_id=$1 AND user_id=$2))")
                        .bind(room_id).bind(target).fetch_optional(pool),
                ).await.map_err(|_| timed_out("Member service timed out"))??;
                let current = current.ok_or_else(|| not_found("Room member not found"))?;
                if !actor_role.can_set_role(current, new_role) {
                    return Err(forbidden("Insufficient role permissions"));
                }
                self.persist_room(
                    room_id,
                    &room_lock,
                    roles::set_role(pool, room_id, &target, new_role, &setter),
                )
                .await?;
                room = room_lock.write().await;
                room.ensure_live()?;
                room.policy_revision = room.policy_revision.wrapping_add(1);
                SocialResponse::SetMemberRole { updated: true }
            }
            ClientMessage::ReportParticipant {
                target_participant_id,
                reason,
                ..
            } => {
                let reason = reason.trim();
                if reason.is_empty() || reason.len() > 1024 || reason.chars().any(char::is_control)
                {
                    return Err(invalid("Report reason must be 1–1024 bytes"));
                }
                if target_participant_id == participant_id {
                    return Err(invalid("Choose another participant"));
                }
                let target = room
                    .participants
                    .get(target_participant_id)
                    .ok_or_else(|| not_found("Participant is no longer in this room"))?;
                let target_name = target.name.clone();
                let reporter = reporter_key(room.participants.get(participant_id).unwrap());
                if room
                    .social
                    .report_cooldown_active(&reporter, std::time::Instant::now())
                {
                    return Err(rate_limited("Please wait before submitting another report"));
                }
                let report_id = Uuid::new_v4();
                let report = ReportEntry {
                    report_id: report_id.to_string(),
                    reporter_id: *participant_id,
                    reporter_name: actor_name,
                    target_participant_id: *target_participant_id,
                    target_name,
                    reason: reason.to_string(),
                    status: ReportStatus::Open,
                    created_at: chrono::Utc::now().to_rfc3339(),
                    resolved_at: None,
                    outcome: None,
                };
                if room.persisted {
                    let pool = self
                        .db_pool
                        .as_ref()
                        .ok_or_else(|| unavailable("Report service unavailable"))?;
                    drop(room);
                    let inserted = self.persist_room(room_id, &room_lock, async {
                        let mut transaction = pool.begin().await?;
                        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,71339))")
                            .bind(room_id)
                            .execute(&mut *transaction)
                            .await?;
                        let count: i64 =
                            sqlx::query_scalar("SELECT COUNT(*) FROM room_reports WHERE room_id=$1")
                                .bind(room_id)
                                .fetch_one(&mut *transaction)
                                .await?;
                        if count >= MAX_REPORTS as i64 {
                            let removed=sqlx::query("DELETE FROM room_reports WHERE id IN (SELECT id FROM room_reports WHERE room_id=$1 AND status<>'open' ORDER BY created_at,id LIMIT 1)").bind(room_id).execute(&mut *transaction).await?.rows_affected();
                            if removed == 0 {
                                return Ok(false);
                            }
                        }
                        sqlx::query("INSERT INTO room_reports(id,room_id,reporter_id,reporter_name,target_participant_id,target_name,reason) VALUES($1,$2,$3,$4,$5,$6,$7)")
                            .bind(report_id).bind(room_id).bind(participant_id).bind(&report.reporter_name).bind(target_participant_id).bind(&report.target_name).bind(reason).execute(&mut *transaction).await?;
                        transaction.commit().await?;
                        Ok(true)
                    })
                    .await?;
                    if !inserted {
                        return Err(forbidden(
                            "Room report capacity reached; contact a moderator",
                        ));
                    }
                    room = room_lock.write().await;
                    room.ensure_live()?;
                } else {
                    if room.social.reports.len() >= MAX_REPORTS {
                        if let Some(index) = room
                            .social
                            .reports
                            .iter()
                            .position(|r| r.status != ReportStatus::Open)
                        {
                            room.social.reports.remove(index);
                        } else {
                            return Err(forbidden(
                                "Room report capacity reached; contact a moderator",
                            ));
                        }
                    }
                    room.social.reports.push_back(report.clone());
                }
                room.social.note_report(reporter, std::time::Instant::now());
                // Only the submitting participant receives the report ID here.
                SocialResponse::ReportParticipant {
                    report_id: report.report_id.clone(),
                    status: ReportStatus::Open,
                }
            }
            ClientMessage::ListRoomReports { offset, .. } => {
                require_role(actor_role, roles::Role::Moderator)?;
                let offset = page_offset(*offset)?;
                let mut reports: Vec<ReportEntry> = if room.persisted {
                    let pool = self
                        .db_pool
                        .as_ref()
                        .ok_or_else(|| unavailable("Report service unavailable"))?;
                    drop(room);
                    let rows: Vec<ReportRow> = tokio::time::timeout(
                        control::PERSISTENCE_TIMEOUT,
                        sqlx::query_as("SELECT r.id,r.reporter_id,r.reporter_name,r.target_participant_id,r.target_name,r.reason,r.status,r.created_at,r.resolved_at,o.action,o.created_at FROM room_reports r LEFT JOIN LATERAL (SELECT e.action,e.created_at FROM moderation_events e WHERE e.report_id=r.id ORDER BY e.created_at DESC,e.id LIMIT 1) o ON true WHERE r.room_id=$1 ORDER BY r.created_at DESC,r.id LIMIT 101 OFFSET $2")
                            .bind(room_id).bind(offset as i64).fetch_all(pool),
                    ).await.map_err(|_| timed_out("Report service timed out"))??;
                    room = room_lock.write().await;
                    room.ensure_live()?;
                    rows.into_iter()
                        .map(
                            |(
                                id,
                                rid,
                                rname,
                                tid,
                                tname,
                                reason,
                                status,
                                created,
                                resolved,
                                outcome_action,
                                outcome_at,
                            )| {
                                ReportEntry {
                                    report_id: id.to_string(),
                                    reporter_id: rid,
                                    reporter_name: rname,
                                    target_participant_id: tid,
                                    target_name: tname,
                                    reason,
                                    status,
                                    created_at: created.to_rfc3339(),
                                    resolved_at: resolved.map(|r| r.to_rfc3339()),
                                    outcome: outcome_action.zip(outcome_at).map(|(action, at)| {
                                        ReportOutcome {
                                            action,
                                            created_at: at.to_rfc3339(),
                                        }
                                    }),
                                }
                            },
                        )
                        .collect()
                } else {
                    room.social
                        .reports
                        .iter()
                        .rev()
                        .skip(offset)
                        .take(PAGE_SIZE + 1)
                        .map(|report| ReportEntry {
                            outcome: room.social.report_outcome(&report.report_id),
                            ..report.clone()
                        })
                        .collect()
                };
                let has_more = reports.len() > PAGE_SIZE;
                reports.truncate(PAGE_SIZE);
                SocialResponse::ListRoomReports { reports, has_more }
            }
            ClientMessage::ResolveRoomReport {
                report_id, status, ..
            } => {
                require_role(actor_role, roles::Role::Moderator)?;
                let id: Uuid = report_id.parse().map_err(|_| invalid("Invalid report"))?;
                let action = status.action();
                if room.persisted {
                    let pool = self
                        .db_pool
                        .as_ref()
                        .ok_or_else(|| unavailable("Report service unavailable"))?;
                    drop(room);
                    let actor_id = *participant_id;
                    let actor_name = actor_name.clone();
                    let resolved = self
                        .persist_room(room_id, &room_lock, async {
                            let mut transaction = pool.begin().await?;
                            let target: Option<(String, String, bool)> = sqlx::query_as("UPDATE room_reports r SET status=$3,resolved_at=now(),resolved_by=$4 WHERE r.room_id=$1 AND r.id=$2 AND r.status='open' RETURNING r.target_participant_id,r.target_name,EXISTS(SELECT 1 FROM users u WHERE u.id::text=r.target_participant_id)")
                                .bind(room_id).bind(id).bind(status).bind(actor_id).fetch_optional(&mut *transaction).await?;
                            let Some((target_id, target_name, target_authenticated)) = target else {
                                return Ok(false);
                            };
                            let event = ModerationEvent::new(
                                action,
                                Actor { id: &actor_id, name: &actor_name },
                                Target { id: ModerationTargetId::from_text(&target_id), name: &target_name, authenticated: target_authenticated, ip: None },
                                None,
                                None,
                                Some(id),
                            );
                            record_event(&mut transaction, room_id, &event, MAX_MODERATION_EVENTS).await?;
                            transaction.commit().await?;
                            Ok(true)
                        })
                        .await?;
                    room = room_lock.write().await;
                    room.ensure_live()?;
                    if !resolved {
                        return Err(not_found("Open report not found"));
                    }
                } else {
                    let report = room
                        .social
                        .reports
                        .iter_mut()
                        .find(|r| r.report_id == *report_id && r.status == ReportStatus::Open)
                        .ok_or_else(|| not_found("Open report not found"))?;
                    report.status = status.status();
                    report.resolved_at = Some(chrono::Utc::now().to_rfc3339());
                    let target_id = report.target_participant_id;
                    let target_name = report.target_name.clone();
                    let target_authenticated = room
                        .participants
                        .get(&target_id)
                        .is_some_and(|participant| participant.authenticated);
                    room.social.record_moderation_event(ModerationEvent::new(
                        action,
                        Actor {
                            id: participant_id,
                            name: &actor_name,
                        },
                        Target {
                            id: target_id.into(),
                            name: &target_name,
                            authenticated: target_authenticated,
                            ip: None,
                        },
                        None,
                        None,
                        Some(id),
                    ));
                }
                SocialResponse::ResolveRoomReport {
                    report_id: report_id.to_string(),
                    status: *status,
                }
            }
            ClientMessage::ListModerationEvents { offset, .. } => {
                require_role(actor_role, roles::Role::Moderator)?;
                let offset = page_offset(*offset)?;
                let owner = actor_role >= roles::Role::Owner;
                let mut events: Vec<ModerationEvent> = if room.persisted {
                    let pool = self
                        .db_pool
                        .as_ref()
                        .ok_or_else(|| unavailable("History service unavailable"))?;
                    drop(room);
                    let rows = tokio::time::timeout(
                        control::PERSISTENCE_TIMEOUT,
                        moderation::list_events(
                            pool,
                            room_id,
                            offset as i64,
                            (PAGE_SIZE + 1) as i64,
                        ),
                    )
                    .await
                    .map_err(|_| timed_out("History service timed out"))??;
                    room = room_lock.write().await;
                    room.ensure_live()?;
                    rows
                } else {
                    room.social
                        .moderation_events
                        .iter()
                        .rev()
                        .skip(offset)
                        .take(PAGE_SIZE + 1)
                        .cloned()
                        .collect()
                };
                let has_more = events.len() > PAGE_SIZE;
                events.truncate(PAGE_SIZE);
                // Only the owner reads a target's address; moderators never do.
                let events: Vec<ModerationEventView> = events
                    .iter()
                    .map(|event| ModerationEventView::new(event.clone(), owner))
                    .collect();
                SocialResponse::ListModerationEvents { events, has_more }
            }
            _ => return Err(not_found("Unknown request")),
        };
        drop(room);
        if let Some(control) = &control {
            control.release();
        }
        send(
            &self.metrics,
            expected_sender,
            &ServerMessage::SocialResponse {
                request_id: request_id.to_string(),
                response,
            },
        )
    }
}
