#![forbid(unsafe_code)]

use crate::ids::{ParticipantId, RoomId};
use crate::room::refusal::{Refusal, RoomError};
use crate::room::roles::Role;
use crate::signaling::protocol::{BanEntry, ReportEntry, ReportOutcome};
use chrono::{DateTime, Utc};
use mediasoup::prelude::MediaKind;
use serde::{Deserialize, Serialize};
use sqlx::{PgConnection, PgExecutor, PgPool};
use std::net::IpAddr;
use uuid::Uuid;

/// In-memory punitive state for a participant
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct PunitiveState {
    pub cam_banned: bool,
    pub text_muted: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PunitiveKind {
    CamBanned,
    Muted,
}

impl PunitiveKind {
    fn as_db_str(self) -> &'static str {
        match self {
            Self::CamBanned => "cambanned",
            Self::Muted => "muted",
        }
    }
}

/// Whom a saved sanction binds: an account (with the address it used, for the
/// owner's history) or a guest's address cohort.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SanctionIdentity {
    User { id: Uuid, ip: Option<IpAddr> },
    Guest(IpAddr),
}

impl SanctionIdentity {
    /// The durable identity of a participant: an account as itself, a guest as
    /// its address cohort; a guest without an address cannot carry a saved sanction.
    pub(crate) fn of(
        participant: &ParticipantId,
        authenticated: bool,
        ip: Option<IpAddr>,
    ) -> Result<Self, RoomError> {
        if authenticated {
            Ok(Self::User {
                id: participant.as_uuid(),
                ip,
            })
        } else if let Some(ip) = ip {
            Ok(Self::Guest(crate::net::cohort(ip)))
        } else {
            Err(Refusal::Invalid(
                "The participant cannot be identified for a saved sanction".into(),
            )
            .into())
        }
    }

    fn user_id(self) -> Option<Uuid> {
        match self {
            Self::User { id, .. } => Some(id),
            Self::Guest(_) => None,
        }
    }

    fn address(self) -> Option<String> {
        match self {
            Self::User { ip, .. } => ip.map(|ip| ip.to_string()),
            Self::Guest(ip) => Some(ip.to_string()),
        }
    }
}

/// One saved sanction: whom it binds, why, and which moderator applied it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Sanction<'a> {
    pub(crate) identity: SanctionIdentity,
    pub(crate) reason: Option<&'a str>,
    pub(crate) applied_by: Uuid,
}

/// Durable history entries kept per room; the oldest go when a new one arrives.
pub(crate) const MAX_MODERATION_EVENTS: usize = 1000;

/// Whom a moderation entry is about: a participant, or a ban that no longer
/// names a participant (guest bans are keyed by their own identifier). Both
/// serialize as the plain string they always were.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ModerationTargetId {
    Participant(ParticipantId),
    Ban(String),
}

impl From<ParticipantId> for ModerationTargetId {
    fn from(value: ParticipantId) -> Self {
        Self::Participant(value)
    }
}

impl From<&ParticipantId> for ModerationTargetId {
    fn from(value: &ParticipantId) -> Self {
        Self::Participant(*value)
    }
}

impl std::fmt::Display for ModerationTargetId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Participant(id) => std::fmt::Display::fmt(id, f),
            Self::Ban(id) => f.write_str(id),
        }
    }
}

impl ModerationTargetId {
    /// A stored identifier: a participant when it parses as one, else a ban key.
    pub(crate) fn from_text(value: &str) -> Self {
        value
            .parse()
            .map_or_else(|_| Self::Ban(value.to_owned()), Self::Participant)
    }
}

impl std::str::FromStr for ModerationTargetId {
    type Err = std::convert::Infallible;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Ok(Self::from_text(value))
    }
}

/// Stores a `Display`/`FromStr` type in a text column.
macro_rules! text_column {
    ($name:ty) => {
        impl sqlx::Type<sqlx::Postgres> for $name {
            fn type_info() -> sqlx::postgres::PgTypeInfo {
                <String as sqlx::Type<sqlx::Postgres>>::type_info()
            }

            fn compatible(ty: &sqlx::postgres::PgTypeInfo) -> bool {
                <String as sqlx::Type<sqlx::Postgres>>::compatible(ty)
            }
        }

        impl sqlx::Encode<'_, sqlx::Postgres> for $name {
            fn encode_by_ref(
                &self,
                buf: &mut sqlx::postgres::PgArgumentBuffer,
            ) -> Result<sqlx::encode::IsNull, sqlx::error::BoxDynError> {
                <String as sqlx::Encode<'_, sqlx::Postgres>>::encode(self.to_string(), buf)
            }
        }

        impl sqlx::Decode<'_, sqlx::Postgres> for $name {
            fn decode(
                value: sqlx::postgres::PgValueRef<'_>,
            ) -> Result<Self, sqlx::error::BoxDynError> {
                let text = <&str as sqlx::Decode<'_, sqlx::Postgres>>::decode(value)?;
                Ok(text.parse()?)
            }
        }
    };
}

text_column!(ModerationTargetId);
text_column!(ReportStatus);
text_column!(ReportResolution);

/// Where a report stands: open until a moderator resolves or dismisses it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReportStatus {
    Open,
    Resolved,
    Dismissed,
}

impl ReportStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Resolved => "resolved",
            Self::Dismissed => "dismissed",
        }
    }
}

impl std::fmt::Display for ReportStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for ReportStatus {
    type Err = UnknownReportStatus;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "open" => Ok(Self::Open),
            "resolved" => Ok(Self::Resolved),
            "dismissed" => Ok(Self::Dismissed),
            _ => Err(UnknownReportStatus),
        }
    }
}

/// A stored report status outside the three the server writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("unknown report status")]
pub struct UnknownReportStatus;

/// The two ways a moderator closes a report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReportResolution {
    Resolved,
    Dismissed,
}

impl ReportResolution {
    pub fn status(self) -> ReportStatus {
        match self {
            Self::Resolved => ReportStatus::Resolved,
            Self::Dismissed => ReportStatus::Dismissed,
        }
    }

    pub fn action(self) -> ModerationAction {
        match self {
            Self::Resolved => ModerationAction::ReportResolved,
            Self::Dismissed => ModerationAction::ReportDismissed,
        }
    }
}

impl std::fmt::Display for ReportResolution {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.status().as_str())
    }
}

impl std::str::FromStr for ReportResolution {
    type Err = UnknownReportStatus;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "resolved" => Ok(Self::Resolved),
            "dismissed" => Ok(Self::Dismissed),
            _ => Err(UnknownReportStatus),
        }
    }
}

/// The longest reason a sanction, report resolution or lobby decision may carry, in bytes.
pub(crate) const MAX_REASON_LEN: usize = 256;

/// Whether a given reason fits the stored rule: at most [`MAX_REASON_LEN`] bytes
/// without control characters.
pub(crate) fn valid_reason(value: &str) -> bool {
    value.len() <= MAX_REASON_LEN && !value.chars().any(char::is_control)
}

/// What a moderator did, as the history stores and serializes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModerationAction {
    Kick,
    Ban,
    Unban,
    CamBan,
    CamUnban,
    TextMute,
    TextUnmute,
    ReportResolved,
    ReportDismissed,
    MessageRemoved,
}

impl ModerationAction {
    /// Every action, in the order the history vocabulary lists them.
    pub(crate) const ALL: [Self; 10] = [
        Self::Kick,
        Self::Ban,
        Self::Unban,
        Self::CamBan,
        Self::CamUnban,
        Self::TextMute,
        Self::TextUnmute,
        Self::ReportResolved,
        Self::ReportDismissed,
        Self::MessageRemoved,
    ];

    /// The stored name, which is also the serialized one (a test keeps them equal).
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Kick => "kick",
            Self::Ban => "ban",
            Self::Unban => "unban",
            Self::CamBan => "cam_ban",
            Self::CamUnban => "cam_unban",
            Self::TextMute => "text_mute",
            Self::TextUnmute => "text_unmute",
            Self::ReportResolved => "report_resolved",
            Self::ReportDismissed => "report_dismissed",
            Self::MessageRemoved => "message_removed",
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|action| action.as_str() == value)
    }

    pub(crate) fn for_punitive(kind: PunitiveKind, enabled: bool) -> Self {
        match (kind, enabled) {
            (PunitiveKind::CamBanned, true) => Self::CamBan,
            (PunitiveKind::CamBanned, false) => Self::CamUnban,
            (PunitiveKind::Muted, true) => Self::TextMute,
            (PunitiveKind::Muted, false) => Self::TextUnmute,
        }
    }
}

/// Who acted, as the history names them.
#[derive(Clone, Copy)]
pub(crate) struct Actor<'a> {
    pub(crate) id: &'a ParticipantId,
    pub(crate) name: &'a str,
}

/// Whom it concerned; a guest's address is its sanction cohort.
#[derive(Clone)]
pub(crate) struct Target<'a> {
    pub(crate) id: ModerationTargetId,
    pub(crate) name: &'a str,
    pub(crate) authenticated: bool,
    pub(crate) ip: Option<IpAddr>,
}

/// One entry of a room's moderation history. The target's address is for the
/// owner only: it is never serialized with the entry, and `to_json` adds it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModerationEvent {
    pub(crate) event_id: String,
    pub(crate) action: ModerationAction,
    pub(crate) actor_id: ParticipantId,
    pub(crate) actor_name: String,
    pub(crate) target_id: ModerationTargetId,
    pub(crate) target_name: String,
    pub(crate) target_authenticated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) expires_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) report_id: Option<String>,
    pub(crate) created_at: DateTime<Utc>,
    #[serde(skip)]
    pub(crate) target_ip: Option<IpAddr>,
}

impl ModerationEvent {
    pub(crate) fn new(
        action: ModerationAction,
        actor: Actor<'_>,
        target: Target<'_>,
        reason: Option<&str>,
        expires_at: Option<DateTime<Utc>>,
        report_id: Option<Uuid>,
    ) -> Self {
        Self {
            event_id: Uuid::new_v4().to_string(),
            action,
            actor_id: *actor.id,
            actor_name: actor.name.to_owned(),
            target_id: target.id.clone(),
            target_name: target.name.to_owned(),
            target_authenticated: target.authenticated,
            reason: reason.map(str::to_owned),
            expires_at,
            report_id: report_id.map(|id| id.to_string()),
            created_at: Utc::now(),
            target_ip: if target.authenticated {
                target.ip
            } else {
                target.ip.map(crate::net::cohort)
            },
        }
    }
}

/// Append an event inside the caller's transaction. The room keeps its newest
/// `cap` entries. An event that answers a report resolves that report if it is
/// still open, and refuses a report from another room or about someone else.
pub(crate) async fn record_event(
    transaction: &mut PgConnection,
    room_id: &RoomId,
    event: &ModerationEvent,
    cap: usize,
) -> Result<(), RoomError> {
    let report_id = event
        .report_id
        .as_deref()
        .map(Uuid::parse_str)
        .transpose()
        .map_err(|_| sqlx::Error::Protocol("invalid report id".to_string()))?;
    if let Some(report_id) = report_id {
        let target: Option<ParticipantId> = sqlx::query_scalar(
            "SELECT target_participant_id FROM room_reports WHERE room_id = $1 AND id = $2",
        )
        .bind(room_id)
        .bind(report_id)
        .fetch_optional(&mut *transaction)
        .await?;
        let Some(target) = target else {
            return Err(sqlx::Error::RowNotFound.into());
        };
        if ModerationTargetId::from(target) != event.target_id {
            return Err(
                Refusal::Invalid("Open report does not match this participant".into()).into(),
            );
        }
        sqlx::query(
            "UPDATE room_reports SET status = 'resolved', resolved_at = now(), resolved_by = $3
             WHERE room_id = $1 AND id = $2 AND status = 'open'",
        )
        .bind(room_id)
        .bind(report_id)
        .bind(event.actor_id)
        .execute(&mut *transaction)
        .await?;
    }
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM moderation_events WHERE room_id = $1")
            .bind(room_id)
            .fetch_one(&mut *transaction)
            .await?;
    let excess = (count + 1).saturating_sub(cap as i64);
    if excess > 0 {
        sqlx::query(
            "DELETE FROM moderation_events WHERE id IN (
                SELECT id FROM moderation_events WHERE room_id = $1
                ORDER BY created_at, id LIMIT $2)",
        )
        .bind(room_id)
        .bind(excess)
        .execute(&mut *transaction)
        .await?;
    }
    let event_id = Uuid::parse_str(&event.event_id)
        .map_err(|_| sqlx::Error::Protocol("invalid event id".to_string()))?;
    sqlx::query(
        "INSERT INTO moderation_events
            (id, room_id, action, actor_id, actor_name, target_id, target_name,
             target_authenticated, target_ip, reason, expires_at, report_id, created_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9::inet, $10, $11, $12, $13)",
    )
    .bind(event_id)
    .bind(room_id)
    .bind(event.action.as_str())
    .bind(event.actor_id)
    .bind(&event.actor_name)
    .bind(&event.target_id)
    .bind(&event.target_name)
    .bind(event.target_authenticated)
    .bind(event.target_ip.map(|ip| ip.to_string()))
    .bind(&event.reason)
    .bind(event.expires_at)
    .bind(report_id)
    .bind(event.created_at)
    .execute(&mut *transaction)
    .await?;
    Ok(())
}

/// How long moderation data stays: addresses for `address_days`, whole
/// entries, closed reports and expired sanctions for `history_days`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetentionConfig {
    pub address_days: u32,
    pub history_days: u32,
}

const DEFAULT_ADDRESS_RETENTION_DAYS: u32 = 30;
const DEFAULT_HISTORY_RETENTION_DAYS: u32 = 365;
const MAX_RETENTION_DAYS: u32 = 3650;
const RETENTION_INTERVAL: std::time::Duration = std::time::Duration::from_secs(6 * 60 * 60);
const RETENTION_FIRST_DELAY: std::time::Duration = std::time::Duration::from_secs(60);
const RETENTION_BATCH_SIZE: i64 = 1000;
const RETENTION_MAX_BATCHES: usize = 16;
const RETENTION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// A whole number of days from 1 to 3650; unset or empty means the default.
fn parse_days(name: &str, value: Option<&str>, default: u32) -> anyhow::Result<u32> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(default);
    };
    match value.parse::<u32>() {
        Ok(days) if (1..=MAX_RETENTION_DAYS).contains(&days) => Ok(days),
        _ => anyhow::bail!("{name} must be a whole number of days from 1 to {MAX_RETENTION_DAYS}"),
    }
}

impl RetentionConfig {
    /// `MODERATION_ADDRESS_RETENTION_DAYS` (30) and
    /// `MODERATION_HISTORY_RETENTION_DAYS` (365); addresses never outlive entries.
    pub(crate) fn parse(address: Option<&str>, history: Option<&str>) -> anyhow::Result<Self> {
        let address_days = parse_days(
            "MODERATION_ADDRESS_RETENTION_DAYS",
            address,
            DEFAULT_ADDRESS_RETENTION_DAYS,
        )?;
        let history_days = parse_days(
            "MODERATION_HISTORY_RETENTION_DAYS",
            history,
            DEFAULT_HISTORY_RETENTION_DAYS,
        )?;
        anyhow::ensure!(
            address_days <= history_days,
            "MODERATION_ADDRESS_RETENTION_DAYS must not exceed MODERATION_HISTORY_RETENTION_DAYS"
        );
        Ok(Self {
            address_days,
            history_days,
        })
    }
}

/// Rows changed by one bounded batch or an aggregate sweep.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RetentionReport {
    pub addresses_cleared: u64,
    pub sanctions_removed: u64,
    pub entries_removed: u64,
    pub reports_removed: u64,
    pub invites_removed: u64,
}

impl RetentionReport {
    pub(crate) fn total(&self) -> u64 {
        self.addresses_cleared
            + self.sanctions_removed
            + self.entries_removed
            + self.reports_removed
            + self.invites_removed
    }
}

/// Age moderation data out: a target's address goes after `address_days`, an
/// expired sanction's row, a whole history entry and a closed report after
/// `history_days`. Open reports and live sanctions stay whatever their age.
/// Expired invitations and redemption receipts stay for seven additional days.
/// Each statement selects at most 1,000 parent rows and skips locked rows;
/// earlier statements remain committed if a later statement fails or is cancelled.
pub(crate) async fn retire_old(
    pool: &PgPool,
    config: RetentionConfig,
) -> Result<RetentionReport, RoomError> {
    retire_old_batch(pool, config, RETENTION_BATCH_SIZE).await
}

async fn retire_old_batch(
    pool: &PgPool,
    config: RetentionConfig,
    batch_size: i64,
) -> Result<RetentionReport, RoomError> {
    let address_days = i32::try_from(config.address_days).unwrap_or(i32::MAX);
    let history_days = i32::try_from(config.history_days).unwrap_or(i32::MAX);
    let addresses_cleared = sqlx::query(
        "UPDATE moderation_events SET target_ip = NULL
         WHERE id IN (
             SELECT id FROM moderation_events
             WHERE target_ip IS NOT NULL AND created_at < now() - make_interval(days => $1)
             ORDER BY created_at, id LIMIT $2 FOR UPDATE SKIP LOCKED
         )",
    )
    .bind(address_days)
    .bind(batch_size)
    .execute(pool)
    .await?
    .rows_affected();
    let sanctions_removed = sqlx::query(
        "DELETE FROM room_states
         WHERE id IN (
             SELECT id FROM room_states
             WHERE expires_at IS NOT NULL AND expires_at < now() - make_interval(days => $1)
             ORDER BY expires_at, id LIMIT $2 FOR UPDATE SKIP LOCKED
         )",
    )
    .bind(history_days)
    .bind(batch_size)
    .execute(pool)
    .await?
    .rows_affected();
    let entries_removed = sqlx::query(
        "DELETE FROM moderation_events WHERE id IN (
             SELECT id FROM moderation_events WHERE created_at < now() - make_interval(days => $1)
             ORDER BY created_at, id LIMIT $2 FOR UPDATE SKIP LOCKED
         )",
    )
    .bind(history_days)
    .bind(batch_size)
    .execute(pool)
    .await?
    .rows_affected();
    let reports_removed = sqlx::query(
        "DELETE FROM room_reports
         WHERE id IN (
             SELECT id FROM room_reports
             WHERE status <> 'open' AND resolved_at < now() - make_interval(days => $1)
             ORDER BY resolved_at, id LIMIT $2 FOR UPDATE SKIP LOCKED
         )",
    )
    .bind(history_days)
    .bind(batch_size)
    .execute(pool)
    .await?
    .rows_affected();
    let invites_removed = sqlx::query(
        "DELETE FROM invites WHERE code_hash IN (
             SELECT code_hash FROM invites WHERE expires_at < now() - make_interval(days => $1)
             ORDER BY expires_at, code_hash LIMIT $2 FOR UPDATE SKIP LOCKED
         )",
    )
    .bind(super::invites::INVITE_RECEIPT_RETENTION_DAYS)
    .bind(batch_size)
    .execute(pool)
    .await?
    .rows_affected();
    Ok(RetentionReport {
        addresses_cleared,
        sanctions_removed,
        entries_removed,
        reports_removed,
        invites_removed,
    })
}

async fn retention_sweep(
    pool: &PgPool,
    config: RetentionConfig,
) -> Result<RetentionReport, RoomError> {
    let mut total = RetentionReport::default();
    for _ in 0..RETENTION_MAX_BATCHES {
        let batch = retire_old(pool, config).await?;
        total.addresses_cleared += batch.addresses_cleared;
        total.sanctions_removed += batch.sanctions_removed;
        total.entries_removed += batch.entries_removed;
        total.reports_removed += batch.reports_removed;
        total.invites_removed += batch.invites_removed;
        if [
            batch.addresses_cleared,
            batch.sanctions_removed,
            batch.entries_removed,
            batch.reports_removed,
            batch.invites_removed,
        ]
        .into_iter()
        .all(|rows| rows < RETENTION_BATCH_SIZE as u64)
        {
            break;
        }
        tokio::task::yield_now().await;
    }
    Ok(total)
}

/// Sweep every six hours, a minute after startup, each sweep bounded; a failed
/// sweep is retried by the next one. At most 16 batches per table run within
/// the 30-second sweep budget, allowing incremental progress on a large backlog.
pub(crate) fn spawn_retention(
    pool: PgPool,
    config: RetentionConfig,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        tokio::time::sleep(RETENTION_FIRST_DELAY).await;
        let mut interval = tokio::time::interval(RETENTION_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            match tokio::time::timeout(RETENTION_TIMEOUT, retention_sweep(&pool, config)).await {
                Ok(Ok(report)) if report.total() > 0 => {
                    tracing::info!(?report, "Moderation and invitation retention sweep");
                }
                Ok(Ok(_)) => {}
                Ok(Err(error)) => {
                    tracing::warn!(%error, "Moderation and invitation retention sweep failed; the next retries");
                }
                Err(_) => tracing::warn!(
                    "Moderation and invitation retention sweep exceeded its time budget"
                ),
            }
        }
    })
}

/// Write a history entry on its own, for a change that leaves no other row.
pub(crate) async fn persist_event(
    pool: &PgPool,
    room_id: &RoomId,
    event: &ModerationEvent,
) -> Result<(), RoomError> {
    let mut transaction = pool.begin().await?;
    record_event(&mut transaction, room_id, event, MAX_MODERATION_EVENTS).await?;
    transaction.commit().await?;
    Ok(())
}

/// A banned identity as the ban listing reads it.
#[derive(sqlx::FromRow)]
struct BanRow {
    id: Uuid,
    display_name: Option<String>,
    authenticated: bool,
    reason: Option<String>,
    expires_at: Option<DateTime<Utc>>,
}

/// The live bans after `offset`, newest first, at most `limit` of them.
pub(crate) async fn list_bans<'e>(
    executor: impl PgExecutor<'e>,
    room_id: &RoomId,
    offset: i64,
    limit: i64,
) -> Result<Vec<BanEntry>, RoomError> {
    let rows: Vec<BanRow> = sqlx::query_as(
        "SELECT s.id, u.display_name, s.user_id IS NOT NULL AS authenticated, s.reason, s.expires_at
         FROM room_states s LEFT JOIN users u ON u.id = s.user_id
         WHERE s.room_id = $1 AND s.state = 'banned'
           AND (s.expires_at IS NULL OR s.expires_at > now())
         ORDER BY s.created_at DESC, s.id LIMIT $3 OFFSET $2",
    )
    .bind(room_id)
    .bind(offset)
    .bind(limit)
    .fetch_all(executor)
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| BanEntry {
            ban_id: row.id.to_string(),
            display_name: row.display_name.unwrap_or_else(|| "Guest".to_string()),
            authenticated: row.authenticated,
            reason: row.reason,
            expires_at: row.expires_at.map(|expires| expires.to_rfc3339()),
        })
        .collect())
}

/// A report with its latest moderation outcome as the report listing reads it.
#[derive(sqlx::FromRow)]
struct ReportRow {
    id: Uuid,
    reporter_id: ParticipantId,
    reporter_name: String,
    target_participant_id: ParticipantId,
    target_name: String,
    reason: String,
    status: ReportStatus,
    created_at: DateTime<Utc>,
    resolved_at: Option<DateTime<Utc>>,
    outcome_action: Option<String>,
    outcome_at: Option<DateTime<Utc>>,
}

/// The reports after `offset`, newest first, at most `limit` of them, each with
/// the latest moderation outcome that answered it.
pub(crate) async fn list_reports<'e>(
    executor: impl PgExecutor<'e>,
    room_id: &RoomId,
    offset: i64,
    limit: i64,
) -> Result<Vec<ReportEntry>, RoomError> {
    let rows: Vec<ReportRow> = sqlx::query_as(
        "SELECT r.id, r.reporter_id, r.reporter_name, r.target_participant_id, r.target_name,
                r.reason, r.status, r.created_at, r.resolved_at,
                o.action AS outcome_action, o.created_at AS outcome_at
         FROM room_reports r
         LEFT JOIN LATERAL (SELECT e.action, e.created_at FROM moderation_events e
                            WHERE e.report_id = r.id ORDER BY e.created_at DESC, e.id LIMIT 1) o
           ON true
         WHERE r.room_id = $1 ORDER BY r.created_at DESC, r.id LIMIT $3 OFFSET $2",
    )
    .bind(room_id)
    .bind(offset)
    .bind(limit)
    .fetch_all(executor)
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| ReportEntry {
            report_id: row.id.to_string(),
            reporter_id: row.reporter_id,
            reporter_name: row.reporter_name,
            target_participant_id: row.target_participant_id,
            target_name: row.target_name,
            reason: row.reason,
            status: row.status,
            created_at: row.created_at.to_rfc3339(),
            resolved_at: row.resolved_at.map(|resolved| resolved.to_rfc3339()),
            outcome: row
                .outcome_action
                .zip(row.outcome_at)
                .map(|(action, at)| ReportOutcome {
                    action,
                    created_at: at.to_rfc3339(),
                }),
        })
        .collect())
}

/// The newest `limit` entries after `offset`, newest first.
pub(crate) async fn list_events<'e>(
    executor: impl PgExecutor<'e>,
    room_id: &RoomId,
    offset: i64,
    limit: i64,
) -> Result<Vec<ModerationEvent>, RoomError> {
    /// A moderation event as the log listing reads it.
    #[derive(sqlx::FromRow)]
    struct EventRow {
        id: Uuid,
        action: String,
        actor_id: String,
        actor_name: String,
        target_id: String,
        target_name: String,
        target_authenticated: bool,
        target_ip: Option<String>,
        reason: Option<String>,
        expires_at: Option<DateTime<Utc>>,
        report_id: Option<Uuid>,
        created_at: DateTime<Utc>,
    }
    let rows: Vec<EventRow> = sqlx::query_as(
        "SELECT id, action, actor_id, actor_name, target_id, target_name, target_authenticated,
                host(target_ip) AS target_ip, reason, expires_at, report_id, created_at
         FROM moderation_events WHERE room_id = $1
         ORDER BY created_at DESC, id LIMIT $2 OFFSET $3",
    )
    .bind(room_id)
    .bind(limit)
    .bind(offset)
    .fetch_all(executor)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|row| {
            Some(ModerationEvent {
                event_id: row.id.to_string(),
                action: ModerationAction::parse(&row.action)?,
                actor_id: row.actor_id.parse().ok()?,
                actor_name: row.actor_name,
                target_id: row.target_id.parse().ok()?,
                target_name: row.target_name,
                target_authenticated: row.target_authenticated,
                reason: row.reason,
                expires_at: row.expires_at,
                report_id: row.report_id.map(|id| id.to_string()),
                created_at: row.created_at,
                target_ip: row.target_ip.and_then(|ip| ip.parse().ok()),
            })
        })
        .collect())
}

/// Load durable camera/chat sanctions for a joining identity. Registered users
/// match only by UUID; unauthenticated users match by IP so a new guest socket
/// cannot discard a sanction simply by receiving a new participant UUID.
pub(crate) async fn load_punitive_state(
    pool: &PgPool,
    room_id: &RoomId,
    user_id: Option<Uuid>,
    ip: Option<IpAddr>,
    authenticated: bool,
) -> Result<PunitiveState, RoomError> {
    let ip = if authenticated {
        ip
    } else {
        ip.map(crate::net::cohort)
    };
    let (cam_banned, text_muted): (bool, bool) = sqlx::query_as(
        "SELECT
            EXISTS(
                SELECT 1 FROM room_states
                WHERE room_id = $1 AND state = 'cambanned'
                  AND (expires_at IS NULL OR expires_at > now())
                  AND ((user_id IS NOT NULL AND user_id = $2)
                    OR ($3::inet IS NOT NULL AND $4 = false AND user_id IS NULL
                        AND ip_address = $3::inet))
            ),
            EXISTS(
                SELECT 1 FROM room_states
                WHERE room_id = $1 AND state = 'muted'
                  AND (expires_at IS NULL OR expires_at > now())
                  AND ((user_id IS NOT NULL AND user_id = $2)
                    OR ($3::inet IS NOT NULL AND $4 = false AND user_id IS NULL
                        AND ip_address = $3::inet))
            )",
    )
    .bind(room_id)
    .bind(user_id)
    .bind(ip.map(|address| address.to_string()))
    .bind(authenticated)
    .fetch_one(pool)
    .await?;

    Ok(PunitiveState {
        cam_banned,
        text_muted,
    })
}

/// Save or lift a mute or camera ban inside the caller's transaction. Guest
/// rows use the partial unique identity index installed by migration 012 so
/// concurrent moderation cannot create duplicate state for one address cohort.
pub(crate) async fn write_punitive_state(
    connection: &mut PgConnection,
    room_id: &RoomId,
    sanction: Sanction<'_>,
    kind: PunitiveKind,
    enabled: bool,
) -> Result<(), RoomError> {
    let state = kind.as_db_str();
    let address = sanction.identity.address();
    match (sanction.identity.user_id(), enabled) {
        (Some(user_id), true) => {
            sqlx::query(
                "INSERT INTO room_states
                    (room_id, user_id, state, ip_address, reason, applied_by)
                 VALUES ($1, $2, $3, $4::inet, $5, $6)
                 ON CONFLICT (room_id, user_id, state) DO UPDATE
                   SET ip_address = EXCLUDED.ip_address,
                       reason = EXCLUDED.reason,
                       expires_at = NULL,
                       applied_by = EXCLUDED.applied_by",
            )
            .bind(room_id)
            .bind(user_id)
            .bind(state)
            .bind(&address)
            .bind(sanction.reason)
            .bind(sanction.applied_by)
            .execute(&mut *connection)
            .await?;
        }
        (None, true) => {
            sqlx::query(
                "INSERT INTO room_states
                    (room_id, user_id, state, ip_address, reason, applied_by)
                 VALUES ($1, NULL, $2, $3::inet, $4, $5)
                 ON CONFLICT (room_id, ip_address, state)
                   WHERE user_id IS NULL AND ip_address IS NOT NULL
                 DO UPDATE
                   SET reason = EXCLUDED.reason,
                       expires_at = NULL,
                       applied_by = EXCLUDED.applied_by",
            )
            .bind(room_id)
            .bind(state)
            .bind(&address)
            .bind(sanction.reason)
            .bind(sanction.applied_by)
            .execute(&mut *connection)
            .await?;
        }
        (Some(user_id), false) => {
            sqlx::query(
                "DELETE FROM room_states
                 WHERE room_id = $1 AND user_id = $2 AND state = $3",
            )
            .bind(room_id)
            .bind(user_id)
            .bind(state)
            .execute(&mut *connection)
            .await?;
        }
        (None, false) => {
            sqlx::query(
                "DELETE FROM room_states
                 WHERE room_id = $1 AND user_id IS NULL AND state = $2
                   AND ip_address = $3::inet",
            )
            .bind(room_id)
            .bind(state)
            .bind(&address)
            .execute(&mut *connection)
            .await?;
        }
    }
    Ok(())
}

/// Save or lift a mute or camera ban with its history entry in one transaction.
pub(crate) async fn set_punitive_state(
    pool: &PgPool,
    room_id: &RoomId,
    sanction: Sanction<'_>,
    kind: PunitiveKind,
    enabled: bool,
    event: &ModerationEvent,
) -> Result<(), RoomError> {
    let mut transaction = pool.begin().await?;
    write_punitive_state(&mut transaction, room_id, sanction, kind, enabled).await?;
    record_event(&mut transaction, room_id, event, MAX_MODERATION_EVENTS).await?;
    transaction.commit().await?;
    Ok(())
}

/// Save a ban inside the caller's transaction: an account by its id, a guest by
/// its address cohort.
pub(crate) async fn write_ban(
    connection: &mut PgConnection,
    room_id: &RoomId,
    sanction: Sanction<'_>,
    expires_at: Option<DateTime<Utc>>,
) -> Result<(), RoomError> {
    let address = sanction.identity.address();
    if let Some(user_id) = sanction.identity.user_id() {
        sqlx::query(
            "INSERT INTO room_states
                (room_id, user_id, state, ip_address, reason, expires_at, applied_by)
             VALUES ($1, $2, 'banned', $3::inet, $4, $5, $6)
             ON CONFLICT (room_id, user_id, state) DO UPDATE
               SET ip_address = EXCLUDED.ip_address,
                   reason = EXCLUDED.reason,
                   expires_at = EXCLUDED.expires_at,
                   applied_by = EXCLUDED.applied_by",
        )
        .bind(room_id)
        .bind(user_id)
        .bind(&address)
        .bind(sanction.reason)
        .bind(expires_at)
        .bind(sanction.applied_by)
        .execute(&mut *connection)
        .await?;
    } else {
        sqlx::query(
            "INSERT INTO room_states
                (room_id, user_id, state, ip_address, reason, expires_at, applied_by)
             VALUES ($1, NULL, 'banned', $2::inet, $3, $4, $5)
             ON CONFLICT (room_id, ip_address, state)
               WHERE user_id IS NULL AND ip_address IS NOT NULL
             DO UPDATE
               SET reason = EXCLUDED.reason,
                   expires_at = EXCLUDED.expires_at,
                   applied_by = EXCLUDED.applied_by",
        )
        .bind(room_id)
        .bind(&address)
        .bind(sanction.reason)
        .bind(expires_at)
        .bind(sanction.applied_by)
        .execute(&mut *connection)
        .await?;
    }
    Ok(())
}

/// Save a ban with its history entry in one transaction.
pub(crate) async fn persist_ban(
    pool: &PgPool,
    room_id: &RoomId,
    sanction: Sanction<'_>,
    expires_at: Option<DateTime<Utc>>,
    event: &ModerationEvent,
) -> Result<(), RoomError> {
    let mut transaction = pool.begin().await?;
    write_ban(&mut transaction, room_id, sanction, expires_at).await?;
    record_event(&mut transaction, room_id, event, MAX_MODERATION_EVENTS).await?;
    transaction.commit().await?;
    Ok(())
}

/// Check whether a joining participant is banned. Registered users match by
/// user_id (from any IP). IP rows only block unauthenticated connections —
/// shared IPs (CGNAT, tunnels) must not lock out registered users.
pub(crate) async fn is_banned(
    pool: &PgPool,
    room_id: &RoomId,
    user_id: Option<Uuid>,
    ip: Option<IpAddr>,
    authenticated: bool,
) -> Result<bool, RoomError> {
    let ip = if authenticated {
        ip
    } else {
        ip.map(crate::net::cohort)
    };
    let (exists,): (bool,) = sqlx::query_as(
        "SELECT EXISTS(
            SELECT 1 FROM room_states
            WHERE room_id = $1 AND state = 'banned'
              AND (expires_at IS NULL OR expires_at > now())
              AND ( (user_id IS NOT NULL AND user_id = $2)
                 OR ($3::inet IS NOT NULL AND $4 = false AND user_id IS NULL
                     AND ip_address = $3::inet) )
        )",
    )
    .bind(room_id)
    .bind(user_id)
    .bind(ip.map(|i| i.to_string()))
    .bind(authenticated)
    .fetch_one(pool)
    .await?;
    Ok(exists)
}

/// Remove a registered user's persisted ban (guest/IP rows have no stable
/// identity to unban through the UI). Returns true if a ban row was deleted,
/// in which case the history names the account as it is called now.
pub(crate) async fn remove_user_ban(
    pool: &PgPool,
    room_id: &RoomId,
    user_id: Uuid,
    actor: Actor<'_>,
) -> Result<bool, RoomError> {
    let mut transaction = pool.begin().await?;
    let result = sqlx::query(
        "DELETE FROM room_states WHERE room_id = $1 AND state = 'banned' AND user_id = $2",
    )
    .bind(room_id)
    .bind(user_id)
    .execute(&mut *transaction)
    .await?;
    if result.rows_affected() == 0 {
        return Ok(false);
    }
    let name: Option<String> = sqlx::query_scalar("SELECT display_name FROM users WHERE id = $1")
        .bind(user_id)
        .fetch_optional(&mut *transaction)
        .await?;
    let target_id = ParticipantId::from(user_id);
    let event = ModerationEvent::new(
        ModerationAction::Unban,
        actor,
        Target {
            id: target_id.into(),
            name: name.as_deref().unwrap_or("Removed account"),
            authenticated: true,
            ip: None,
        },
        None,
        None,
        None,
    );
    record_event(&mut transaction, room_id, &event, MAX_MODERATION_EVENTS).await?;
    transaction.commit().await?;
    Ok(true)
}

/// The identity a deleted ban row bound.
pub(crate) struct RemovedBan {
    pub(crate) user_id: Option<Uuid>,
    pub(crate) ip: Option<IpAddr>,
}

/// Delete one ban by its row id and record the lifting, inside the caller's
/// transaction. `None` when no live ban row of this room has that id.
pub(crate) async fn remove_ban(
    connection: &mut PgConnection,
    room_id: &RoomId,
    ban_id: Uuid,
    actor: Actor<'_>,
) -> Result<Option<RemovedBan>, RoomError> {
    let row: Option<(Option<Uuid>, Option<String>, Option<String>)> = sqlx::query_as(
        "DELETE FROM room_states s
         WHERE s.room_id = $1 AND s.id = $2 AND s.state = 'banned'
         RETURNING s.user_id, host(s.ip_address),
                   (SELECT display_name FROM users WHERE id = s.user_id)",
    )
    .bind(room_id)
    .bind(ban_id)
    .fetch_optional(&mut *connection)
    .await?;
    let Some((user_id, address, name)) = row else {
        return Ok(None);
    };
    let ip = address.and_then(|address| address.parse::<IpAddr>().ok());
    let target_id = user_id.map_or_else(
        || ModerationTargetId::Ban(ban_id.to_string()),
        |user_id| ModerationTargetId::from(ParticipantId::from(user_id)),
    );
    let event = ModerationEvent::new(
        ModerationAction::Unban,
        actor,
        Target {
            id: target_id,
            name: name.as_deref().unwrap_or(if user_id.is_some() {
                "Removed account"
            } else {
                "Guest"
            }),
            authenticated: user_id.is_some(),
            ip,
        },
        None,
        None,
        None,
    );
    record_event(&mut *connection, room_id, &event, MAX_MODERATION_EVENTS).await?;
    Ok(Some(RemovedBan { user_id, ip }))
}

/// Close an open report with the moderator's decision and record it, inside the
/// caller's transaction; `false` when this room has no open report with that id.
pub(crate) async fn resolve_report(
    connection: &mut PgConnection,
    room_id: &RoomId,
    report_id: Uuid,
    resolution: ReportResolution,
    actor: Actor<'_>,
) -> Result<bool, RoomError> {
    let row: Option<(ParticipantId, String, bool)> = sqlx::query_as(
        "UPDATE room_reports r SET status = $3, resolved_at = now(), resolved_by = $4
         WHERE r.room_id = $1 AND r.id = $2 AND r.status = 'open'
         RETURNING r.target_participant_id, r.target_name,
                   EXISTS(SELECT 1 FROM users u WHERE u.id::text = r.target_participant_id)",
    )
    .bind(room_id)
    .bind(report_id)
    .bind(resolution)
    .bind(actor.id)
    .fetch_optional(&mut *connection)
    .await?;
    let Some((target, target_name, target_authenticated)) = row else {
        return Ok(false);
    };
    let event = ModerationEvent::new(
        resolution.action(),
        actor,
        Target {
            id: target.into(),
            name: &target_name,
            authenticated: target_authenticated,
            ip: None,
        },
        None,
        None,
        Some(report_id),
    );
    record_event(&mut *connection, room_id, &event, MAX_MODERATION_EVENTS).await?;
    Ok(true)
}

/// File a report inside the caller's transaction. The room keeps at most `cap`
/// reports: a closed one gives way to a new one, and a room full of open
/// reports takes no more (`false`).
pub(crate) async fn file_report(
    connection: &mut PgConnection,
    room_id: &RoomId,
    report: &ReportEntry,
    cap: usize,
) -> Result<bool, RoomError> {
    let report_id = Uuid::parse_str(&report.report_id)
        .map_err(|_| sqlx::Error::Protocol("invalid report id".to_string()))?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 71339))")
        .bind(room_id)
        .execute(&mut *connection)
        .await?;
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM room_reports WHERE room_id = $1")
        .bind(room_id)
        .fetch_one(&mut *connection)
        .await?;
    if count >= cap as i64 {
        let removed = sqlx::query(
            "DELETE FROM room_reports WHERE id IN (
                SELECT id FROM room_reports WHERE room_id = $1 AND status <> 'open'
                ORDER BY created_at, id LIMIT 1)",
        )
        .bind(room_id)
        .execute(&mut *connection)
        .await?
        .rows_affected();
        if removed == 0 {
            return Ok(false);
        }
    }
    sqlx::query(
        "INSERT INTO room_reports
            (id, room_id, reporter_id, reporter_name, target_participant_id, target_name, reason)
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(report_id)
    .bind(room_id)
    .bind(report.reporter_id)
    .bind(&report.reporter_name)
    .bind(report.target_participant_id)
    .bind(&report.target_name)
    .bind(&report.reason)
    .execute(&mut *connection)
    .await?;
    Ok(true)
}

/// Check if a participant can produce (not cam-banned, has correct role for moderated rooms)
pub(crate) fn can_produce(
    state: &PunitiveState,
    role: Role,
    moderated: bool,
    kind: MediaKind,
) -> bool {
    // Camera bans apply to the actual media kind, not a client-supplied source
    // label that can be spoofed.
    if state.cam_banned && kind == MediaKind::Video {
        return false;
    }
    if moderated && !role.can_broadcast(true) {
        return false;
    }
    true
}

/// Check if a participant can chat (not text-muted, has correct role)
pub(crate) fn can_chat(state: &PunitiveState, role: Role, moderated: bool) -> bool {
    if state.text_muted {
        return false;
    }
    role.can_chat(moderated)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moderation_actions_store_and_serialize_under_one_name() {
        for action in ModerationAction::ALL {
            assert_eq!(serde_json::to_value(action).unwrap(), action.as_str());
            assert_eq!(ModerationAction::parse(action.as_str()), Some(action));
        }
        assert_eq!(ModerationAction::parse("kicked"), None);
    }
    use crate::ids::fixtures::rid;

    #[test]
    fn a_saved_sanction_binds_an_account_or_a_guest_cohort() {
        use crate::ids::fixtures::pid;
        let account = pid("alice");
        let address: IpAddr = "2001:db8:1:2:3:4:5:6".parse().unwrap();
        assert_eq!(
            SanctionIdentity::of(&account, true, Some(address)).unwrap(),
            SanctionIdentity::User {
                id: account.as_uuid(),
                ip: Some(address)
            }
        );
        assert_eq!(
            SanctionIdentity::of(&account, false, Some(address)).unwrap(),
            SanctionIdentity::Guest(crate::net::cohort(address))
        );
        let refused = SanctionIdentity::of(&account, false, None).unwrap_err();
        assert_eq!(
            refused.to_string(),
            "The participant cannot be identified for a saved sanction"
        );
    }

    #[test]
    fn retention_periods_are_bounded_days_and_addresses_never_outlive_entries() {
        let config = RetentionConfig::parse(None, None).unwrap();
        assert_eq!(
            config,
            RetentionConfig {
                address_days: 30,
                history_days: 365
            }
        );
        let config = RetentionConfig::parse(Some(" 7 "), Some("")).unwrap();
        assert_eq!(
            config,
            RetentionConfig {
                address_days: 7,
                history_days: 365
            }
        );
        assert!(RetentionConfig::parse(Some("0"), None).is_err());
        assert!(RetentionConfig::parse(None, Some("3651")).is_err());
        assert!(RetentionConfig::parse(Some("forever"), None).is_err());
        assert!(RetentionConfig::parse(Some("400"), Some("365")).is_err());
    }

    #[tokio::test]
    #[ignore = "requires TEST_DATABASE_URL pointing to a migrated disposable PostgreSQL database"]
    async fn database_moderation_history_records_sanctions_and_answers_reports() {
        let pool =
            sqlx::PgPool::connect(&std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL"))
                .await
                .unwrap();
        let owner = Uuid::new_v4();
        let member = Uuid::new_v4();
        for (id, name) in [(owner, "Owner"), (member, "Maya")] {
            sqlx::query("INSERT INTO users(id,email,display_name) VALUES($1,$2,$3)")
                .bind(id)
                .bind(format!("{id}@history.invalid"))
                .bind(name)
                .execute(&pool)
                .await
                .unwrap();
        }
        let room_id = rid(&format!("history-{}", Uuid::new_v4()));
        let other_room_id = rid(&format!("history-other-{}", Uuid::new_v4()));
        for id in [&room_id, &other_room_id] {
            sqlx::query("INSERT INTO rooms(id,owner_id,display_name) VALUES($1,$2,$1)")
                .bind(id)
                .bind(owner)
                .execute(&pool)
                .await
                .unwrap();
        }
        let report = Uuid::new_v4();
        let foreign_report = Uuid::new_v4();
        for (id, room) in [(report, &room_id), (foreign_report, &other_room_id)] {
            sqlx::query("INSERT INTO room_reports(id,room_id,reporter_id,reporter_name,target_participant_id,target_name,reason) VALUES($1,$2,$3,'Reporter',$4,'Maya','spam')")
                .bind(id).bind(room).bind(owner.to_string()).bind(member.to_string())
                .execute(&pool).await.unwrap();
        }
        let owner_id = ParticipantId::from(owner);
        let member_id = ParticipantId::from(member);
        let actor = Actor {
            id: &owner_id,
            name: "Owner",
        };
        let target = Target {
            id: member_id.into(),
            name: "Maya",
            authenticated: true,
            ip: None,
        };

        // A ban that answers a report resolves it in the same transaction.
        let ban = ModerationEvent::new(
            ModerationAction::Ban,
            actor,
            target.clone(),
            Some("spam"),
            None,
            Some(report),
        );
        persist_ban(
            &pool,
            &room_id,
            Sanction {
                identity: SanctionIdentity::User {
                    id: member,
                    ip: None,
                },
                reason: Some("spam"),
                applied_by: owner,
            },
            None,
            &ban,
        )
        .await
        .unwrap();
        let (status, resolved_by): (String, Option<String>) =
            sqlx::query_as("SELECT status, resolved_by FROM room_reports WHERE id=$1")
                .bind(report)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(
            (status.as_str(), resolved_by),
            ("resolved", Some(owner_id.to_string()))
        );
        assert!(
            is_banned(&pool, &room_id, Some(member), None, true)
                .await
                .unwrap()
        );

        // A report from another room is refused and left as it was.
        let foreign = ModerationEvent::new(
            ModerationAction::Kick,
            actor,
            target.clone(),
            None,
            None,
            Some(foreign_report),
        );
        assert!(matches!(
            persist_event(&pool, &room_id, &foreign).await,
            Err(RoomError::Database(sqlx::Error::RowNotFound))
        ));
        let (status,): (String,) = sqlx::query_as("SELECT status FROM room_reports WHERE id=$1")
            .bind(foreign_report)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(status, "open");

        // A report about someone else is refused as the caller's mistake.
        let stranger = ParticipantId::random();
        let mismatched = ModerationEvent::new(
            ModerationAction::Kick,
            actor,
            Target {
                id: stranger.into(),
                name: "Someone else",
                authenticated: false,
                ip: None,
            },
            None,
            None,
            Some(report),
        );
        assert!(matches!(
            persist_event(&pool, &room_id, &mismatched).await,
            Err(RoomError::Refused(Refusal::Invalid(_)))
        ));

        // Lifting the ban names the account as it is called now, once.
        assert!(
            remove_user_ban(&pool, &room_id, member, actor)
                .await
                .unwrap()
        );
        assert!(
            !remove_user_ban(&pool, &room_id, member, actor)
                .await
                .unwrap()
        );
        let events = list_events(&pool, &room_id, 0, 10).await.unwrap();
        assert_eq!(
            events.iter().map(|event| event.action).collect::<Vec<_>>(),
            vec![ModerationAction::Unban, ModerationAction::Ban]
        );
        assert_eq!(events[0].target_name, "Maya");
        assert_eq!(
            events[1].report_id.as_deref(),
            Some(report.to_string().as_str())
        );
        assert_eq!(events[1].reason.as_deref(), Some("spam"));
        assert!(
            list_events(&pool, &other_room_id, 0, 10)
                .await
                .unwrap()
                .is_empty()
        );

        // A guest's address is its cohort, and it reaches the owner's listing only.
        let guest_id = ParticipantId::random();
        let guest = ModerationEvent::new(
            ModerationAction::Kick,
            actor,
            Target {
                id: guest_id.into(),
                name: "Guest",
                authenticated: false,
                ip: Some("2001:db8:1:2::9".parse().unwrap()),
            },
            None,
            None,
            None,
        );
        persist_event(&pool, &room_id, &guest).await.unwrap();
        let newest = list_events(&pool, &room_id, 0, 1).await.unwrap().remove(0);
        assert_eq!(newest.target_ip, Some("2001:db8:1:2::".parse().unwrap()));
        let owner_view = crate::signaling::protocol::ModerationEventView::new(newest.clone(), true);
        assert_eq!(owner_view.target_ip.as_deref(), Some("2001:db8:1:2::"));
        let viewer = crate::signaling::protocol::ModerationEventView::new(newest.clone(), false);
        assert!(viewer.target_ip.is_none());
        assert!(
            serde_json::to_value(&viewer)
                .unwrap()
                .get("targetIp")
                .is_none()
        );
        assert!(!serde_json::to_string(&newest).unwrap().contains("2001:db8"));

        // The room keeps only its newest entries.
        let mut transaction = pool.begin().await.unwrap();
        record_event(
            &mut transaction,
            &room_id,
            &ModerationEvent::new(ModerationAction::TextMute, actor, target, None, None, None),
            2,
        )
        .await
        .unwrap();
        transaction.commit().await.unwrap();
        let events = list_events(&pool, &room_id, 0, 10).await.unwrap();
        assert_eq!(
            events.iter().map(|event| event.action).collect::<Vec<_>>(),
            vec![ModerationAction::TextMute, ModerationAction::Kick]
        );

        // Retention: an address goes first, whole entries and closed reports later;
        // an open report stays whatever its age.
        let kick_id = Uuid::parse_str(&events[1].event_id).unwrap();
        sqlx::query(
            "UPDATE moderation_events SET created_at = now() - interval '40 days' WHERE id = $1",
        )
        .bind(kick_id)
        .execute(&pool)
        .await
        .unwrap();
        let config = RetentionConfig {
            address_days: 30,
            history_days: 365,
        };
        let swept = retire_old(&pool, config).await.unwrap();
        assert_eq!(swept.addresses_cleared, 1);
        assert_eq!(swept.entries_removed + swept.reports_removed, 0);
        let events = list_events(&pool, &room_id, 0, 10).await.unwrap();
        assert_eq!(events[1].target_ip, None);
        assert_eq!(events[1].action, ModerationAction::Kick);
        sqlx::query(
            "UPDATE moderation_events SET created_at = now() - interval '400 days' WHERE id = $1",
        )
        .bind(kick_id)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "UPDATE room_reports SET resolved_at = now() - interval '400 days' WHERE id = $1",
        )
        .bind(report)
        .execute(&pool)
        .await
        .unwrap();
        let swept = retire_old(&pool, config).await.unwrap();
        assert_eq!((swept.entries_removed, swept.reports_removed), (1, 1));
        let events = list_events(&pool, &room_id, 0, 10).await.unwrap();
        assert_eq!(
            events.iter().map(|event| event.action).collect::<Vec<_>>(),
            vec![ModerationAction::TextMute]
        );
        let open: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM room_reports WHERE id = $1")
            .bind(foreign_report)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(open, 1);

        // A small batch makes bounded progress past locked invitation rows, and
        // cascades receipts only once their seven-day replay window has ended.
        let codes: Vec<String> = (0..5)
            .map(|_| crate::invite_codes::digest(&Uuid::new_v4().to_string()))
            .collect();
        for (index, code) in codes.iter().enumerate() {
            sqlx::query("INSERT INTO invites(code_hash,kind,room_id,role,created_by,uses_left,expires_at) VALUES($1,'room',$2,2,$3,0,now()-make_interval(days=>$4))")
                .bind(code).bind(&room_id).bind(owner).bind(if index == 4 {1_i32} else {8_i32})
                .execute(&pool).await.unwrap();
            sqlx::query(
                "INSERT INTO invite_redemptions(invite_hash,user_id,granted_role) VALUES($1,$2,2)",
            )
            .bind(code)
            .bind(member)
            .execute(&pool)
            .await
            .unwrap();
        }
        let mut lock = pool.begin().await.unwrap();
        sqlx::query("SELECT code_hash FROM invites WHERE code_hash=$1 FOR UPDATE")
            .bind(&codes[0])
            .execute(&mut *lock)
            .await
            .unwrap();
        let first = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            retire_old_batch(&pool, config, 2),
        )
        .await
        .expect("retention must skip locked rows")
        .unwrap();
        assert_eq!(first.invites_removed, 2);
        lock.rollback().await.unwrap();
        let second = retire_old_batch(&pool, config, 2).await.unwrap();
        assert_eq!(second.invites_removed, 2);
        let retained: Vec<String> =
            sqlx::query_scalar("SELECT code_hash FROM invites WHERE code_hash=ANY($1)")
                .bind(&codes)
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(retained, vec![codes[4].clone()]);
        let receipts: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM invite_redemptions WHERE invite_hash=ANY($1)")
                .bind(&codes)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(receipts, 1);

        for id in [&room_id, &other_room_id] {
            sqlx::query("DELETE FROM rooms WHERE id=$1")
                .bind(id)
                .execute(&pool)
                .await
                .unwrap();
        }
        sqlx::query("DELETE FROM users WHERE id = ANY($1)")
            .bind(vec![owner, member])
            .execute(&pool)
            .await
            .unwrap();
    }
}
