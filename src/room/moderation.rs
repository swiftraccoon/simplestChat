#![forbid(unsafe_code)]

use crate::room::roles::Role;
use chrono::{DateTime, Utc};
use mediasoup::prelude::MediaKind;
use serde::Serialize;
use sqlx::{PgConnection, PgPool};
use std::net::IpAddr;
use uuid::Uuid;

/// Guest identities have no stable account key. IPv6 clients normally control
/// many interface identifiers inside one delegated /64, so exact-address
/// moderation would let a banned or muted guest reconnect immediately. Keep
/// IPv4 exact while using the same IPv6 /64 identity already used by abuse
/// limits elsewhere in the server.
pub(crate) fn canonical_guest_ip(address: IpAddr) -> IpAddr {
    match address {
        IpAddr::V4(_) => address,
        IpAddr::V6(address) => {
            if let Some(address) = address.to_ipv4_mapped() {
                return IpAddr::V4(address);
            }
            let segments = address.segments();
            IpAddr::V6(std::net::Ipv6Addr::new(
                segments[0],
                segments[1],
                segments[2],
                segments[3],
                0,
                0,
                0,
                0,
            ))
        }
    }
}

/// In-memory punitive state for a participant
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PunitiveState {
    pub cam_banned: bool,
    pub text_muted: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PunitiveKind {
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

/// Durable history entries kept per room; the oldest go when a new one arrives.
pub(crate) const MAX_MODERATION_EVENTS: usize = 1000;

/// What a moderator did, as the history stores and serializes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ModerationAction {
    Kick,
    Ban,
    Unban,
    CamBan,
    CamUnban,
    TextMute,
    TextUnmute,
    ReportResolved,
    ReportDismissed,
}

impl ModerationAction {
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
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        [
            Self::Kick,
            Self::Ban,
            Self::Unban,
            Self::CamBan,
            Self::CamUnban,
            Self::TextMute,
            Self::TextUnmute,
            Self::ReportResolved,
            Self::ReportDismissed,
        ]
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
    pub(crate) id: &'a str,
    pub(crate) name: &'a str,
}

/// Whom it concerned; a guest's address is its sanction cohort.
#[derive(Clone, Copy)]
pub(crate) struct Target<'a> {
    pub(crate) id: &'a str,
    pub(crate) name: &'a str,
    pub(crate) authenticated: bool,
    pub(crate) ip: Option<IpAddr>,
}

/// One entry of a room's moderation history. The target's address is for the
/// owner only: it is never serialized with the entry, and `to_json` adds it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ModerationEvent {
    pub(crate) event_id: String,
    pub(crate) action: ModerationAction,
    pub(crate) actor_id: String,
    pub(crate) actor_name: String,
    pub(crate) target_id: String,
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
            actor_id: actor.id.to_owned(),
            actor_name: actor.name.to_owned(),
            target_id: target.id.to_owned(),
            target_name: target.name.to_owned(),
            target_authenticated: target.authenticated,
            reason: reason.map(str::to_owned),
            expires_at,
            report_id: report_id.map(|id| id.to_string()),
            created_at: Utc::now(),
            target_ip: if target.authenticated {
                target.ip
            } else {
                target.ip.map(canonical_guest_ip)
            },
        }
    }

    /// The entry as a reader of the given standing sees it.
    pub(crate) fn to_json(&self, owner: bool) -> serde_json::Value {
        let mut value = serde_json::to_value(self).unwrap_or_default();
        if owner
            && let Some(ip) = self.target_ip
            && let Some(object) = value.as_object_mut()
        {
            object.insert(
                "targetIp".to_owned(),
                serde_json::Value::String(ip.to_string()),
            );
        }
        value
    }
}

/// Append an event inside the caller's transaction. The room keeps its newest
/// `cap` entries. An event that answers a report resolves that report if it is
/// still open, and refuses a report from another room.
pub(crate) async fn record_event(
    transaction: &mut PgConnection,
    room_id: &str,
    event: &ModerationEvent,
    cap: usize,
) -> Result<(), sqlx::Error> {
    let report_id = event
        .report_id
        .as_deref()
        .map(Uuid::parse_str)
        .transpose()
        .map_err(|_| sqlx::Error::Protocol("invalid report id".to_string()))?;
    if let Some(report_id) = report_id {
        let status: Option<String> =
            sqlx::query_scalar("SELECT status FROM room_reports WHERE room_id = $1 AND id = $2")
                .bind(room_id)
                .bind(report_id)
                .fetch_optional(&mut *transaction)
                .await?;
        if status.is_none() {
            return Err(sqlx::Error::RowNotFound);
        }
        sqlx::query(
            "UPDATE room_reports SET status = 'resolved', resolved_at = now(), resolved_by = $3
             WHERE room_id = $1 AND id = $2 AND status = 'open'",
        )
        .bind(room_id)
        .bind(report_id)
        .bind(&event.actor_id)
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
    .bind(&event.actor_id)
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
    pub fn from_env() -> anyhow::Result<Self> {
        fn read(name: &str) -> anyhow::Result<Option<String>> {
            match std::env::var(name) {
                Ok(value) => Ok(Some(value)),
                Err(std::env::VarError::NotPresent) => Ok(None),
                Err(std::env::VarError::NotUnicode(_)) => {
                    anyhow::bail!("{name} must be valid UTF-8")
                }
            }
        }
        let address = read("MODERATION_ADDRESS_RETENTION_DAYS")?;
        let history = read("MODERATION_HISTORY_RETENTION_DAYS")?;
        Self::parse(address.as_deref(), history.as_deref())
    }

    fn parse(address: Option<&str>, history: Option<&str>) -> anyhow::Result<Self> {
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
pub struct RetentionReport {
    pub addresses_cleared: u64,
    pub sanctions_removed: u64,
    pub entries_removed: u64,
    pub reports_removed: u64,
    pub invites_removed: u64,
}

impl RetentionReport {
    pub fn total(&self) -> u64 {
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
pub async fn retire_old(
    pool: &PgPool,
    config: RetentionConfig,
) -> Result<RetentionReport, sqlx::Error> {
    retire_old_batch(pool, config, RETENTION_BATCH_SIZE).await
}

async fn retire_old_batch(
    pool: &PgPool,
    config: RetentionConfig,
    batch_size: i64,
) -> Result<RetentionReport, sqlx::Error> {
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
) -> Result<RetentionReport, sqlx::Error> {
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
pub fn spawn_retention(pool: PgPool, config: RetentionConfig) -> tokio::task::JoinHandle<()> {
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
    room_id: &str,
    event: &ModerationEvent,
) -> Result<(), sqlx::Error> {
    let mut transaction = pool.begin().await?;
    record_event(&mut transaction, room_id, event, MAX_MODERATION_EVENTS).await?;
    transaction.commit().await?;
    Ok(())
}

/// The newest `limit` entries after `offset`, newest first.
pub(crate) async fn list_events(
    pool: &PgPool,
    room_id: &str,
    offset: i64,
    limit: i64,
) -> Result<Vec<ModerationEvent>, sqlx::Error> {
    type EventRow = (
        Uuid,
        String,
        String,
        String,
        String,
        String,
        bool,
        Option<String>,
        Option<String>,
        Option<DateTime<Utc>>,
        Option<Uuid>,
        DateTime<Utc>,
    );
    let rows: Vec<EventRow> = sqlx::query_as(
        "SELECT id, action, actor_id, actor_name, target_id, target_name, target_authenticated,
                host(target_ip), reason, expires_at, report_id, created_at
         FROM moderation_events WHERE room_id = $1
         ORDER BY created_at DESC, id LIMIT $2 OFFSET $3",
    )
    .bind(room_id)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(
            |(
                id,
                action,
                actor_id,
                actor_name,
                target_id,
                target_name,
                target_authenticated,
                target_ip,
                reason,
                expires_at,
                report_id,
                created_at,
            )| {
                Some(ModerationEvent {
                    event_id: id.to_string(),
                    action: ModerationAction::parse(&action)?,
                    actor_id,
                    actor_name,
                    target_id,
                    target_name,
                    target_authenticated,
                    reason,
                    expires_at,
                    report_id: report_id.map(|id| id.to_string()),
                    created_at,
                    target_ip: target_ip.and_then(|ip| ip.parse().ok()),
                })
            },
        )
        .collect())
}

/// Load durable camera/chat sanctions for a joining identity. Registered users
/// match only by UUID; unauthenticated users match by IP so a new guest socket
/// cannot discard a sanction simply by receiving a new participant UUID.
pub async fn load_punitive_state(
    pool: &PgPool,
    room_id: &str,
    user_id: Option<Uuid>,
    ip: Option<IpAddr>,
    authenticated: bool,
) -> Result<PunitiveState, sqlx::Error> {
    let ip = if authenticated {
        ip
    } else {
        ip.map(canonical_guest_ip)
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

/// Persist or remove a camera/chat sanction. Guest rows use the partial unique
/// identity index installed by migration 012 so concurrent moderation cannot
/// create duplicate state for one canonical IP cohort.
#[expect(
    clippy::too_many_arguments,
    reason = "sanction persistence keeps actor, target identity, and mutation fields explicit"
)]
pub(crate) async fn set_punitive_state(
    pool: &PgPool,
    room_id: &str,
    user_id: Option<Uuid>,
    ip: Option<IpAddr>,
    kind: PunitiveKind,
    enabled: bool,
    reason: Option<&str>,
    applied_by: Uuid,
    event: &ModerationEvent,
) -> Result<(), sqlx::Error> {
    let state = kind.as_db_str();
    let ip = if user_id.is_some() {
        ip
    } else {
        ip.map(canonical_guest_ip)
    };
    let ip = ip.map(|address| address.to_string());
    let mut transaction = pool.begin().await?;

    if enabled {
        if let Some(user_id) = user_id {
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
            .bind(&ip)
            .bind(reason)
            .bind(applied_by)
            .execute(&mut *transaction)
            .await?;
        } else if let Some(ip) = &ip {
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
            .bind(ip)
            .bind(reason)
            .bind(applied_by)
            .execute(&mut *transaction)
            .await?;
        } else {
            return Err(sqlx::Error::Protocol(
                "punitive state has no durable identity".to_string(),
            ));
        }
    } else if let Some(user_id) = user_id {
        sqlx::query(
            "DELETE FROM room_states
             WHERE room_id = $1 AND user_id = $2 AND state = $3",
        )
        .bind(room_id)
        .bind(user_id)
        .bind(state)
        .execute(&mut *transaction)
        .await?;
    } else if let Some(ip) = &ip {
        sqlx::query(
            "DELETE FROM room_states
             WHERE room_id = $1 AND user_id IS NULL AND state = $2
               AND ip_address = $3::inet",
        )
        .bind(room_id)
        .bind(state)
        .bind(ip)
        .execute(&mut *transaction)
        .await?;
    } else {
        return Err(sqlx::Error::Protocol(
            "punitive state has no durable identity".to_string(),
        ));
    }

    record_event(&mut transaction, room_id, event, MAX_MODERATION_EVENTS).await?;
    transaction.commit().await?;
    Ok(())
}

/// Persist a ban to room_states. Registered targets are keyed by user_id;
/// guests are keyed by their canonical IP cohort.
#[expect(
    clippy::too_many_arguments,
    reason = "a durable ban keeps identity, sanction fields and its history entry explicit"
)]
pub(crate) async fn persist_ban(
    pool: &PgPool,
    room_id: &str,
    user_id: Option<Uuid>,
    ip: Option<IpAddr>,
    reason: Option<&str>,
    expires_at: Option<chrono::DateTime<chrono::Utc>>,
    applied_by: Uuid,
    event: &ModerationEvent,
) -> Result<(), sqlx::Error> {
    let ip = if user_id.is_some() {
        ip
    } else {
        ip.map(canonical_guest_ip)
    };
    let ip = ip.map(|address| address.to_string());
    let mut transaction = pool.begin().await?;
    if let Some(user_id) = user_id {
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
        .bind(&ip)
        .bind(reason)
        .bind(expires_at)
        .bind(applied_by)
        .execute(&mut *transaction)
        .await?;
    } else if let Some(ip) = &ip {
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
        .bind(ip)
        .bind(reason)
        .bind(expires_at)
        .bind(applied_by)
        .execute(&mut *transaction)
        .await?;
    } else {
        return Err(sqlx::Error::InvalidArgument(
            "ban has no durable identity".to_string(),
        ));
    }
    record_event(&mut transaction, room_id, event, MAX_MODERATION_EVENTS).await?;
    transaction.commit().await?;
    Ok(())
}

/// Check whether a joining participant is banned. Registered users match by
/// user_id (from any IP). IP rows only block unauthenticated connections —
/// shared IPs (CGNAT, tunnels) must not lock out registered users.
pub async fn is_banned(
    pool: &PgPool,
    room_id: &str,
    user_id: Option<Uuid>,
    ip: Option<IpAddr>,
    authenticated: bool,
) -> Result<bool, sqlx::Error> {
    let ip = if authenticated {
        ip
    } else {
        ip.map(canonical_guest_ip)
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
    room_id: &str,
    user_id: Uuid,
    actor: Actor<'_>,
) -> Result<bool, sqlx::Error> {
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
    let target_id = user_id.to_string();
    let event = ModerationEvent::new(
        ModerationAction::Unban,
        actor,
        Target {
            id: &target_id,
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

/// Check if a participant can produce (not cam-banned, has correct role for moderated rooms)
pub fn can_produce(state: &PunitiveState, role: Role, moderated: bool, kind: MediaKind) -> bool {
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
pub fn can_chat(state: &PunitiveState, role: Role, moderated: bool) -> bool {
    if state.text_muted {
        return false;
    }
    role.can_chat(moderated)
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn guest_ipv6_identity_is_scoped_to_64_bit_prefix() {
        let first: IpAddr = "2001:db8:1:2::1".parse().unwrap();
        let rotated: IpAddr = "2001:db8:1:2:ffff:ffff:ffff:42".parse().unwrap();
        let other_prefix: IpAddr = "2001:db8:1:3::1".parse().unwrap();

        assert_eq!(canonical_guest_ip(first), canonical_guest_ip(rotated));
        assert_ne!(canonical_guest_ip(first), canonical_guest_ip(other_prefix));
    }

    #[test]
    fn guest_ipv4_identity_remains_exact() {
        let first: IpAddr = "192.0.2.10".parse().unwrap();
        let other: IpAddr = "192.0.2.11".parse().unwrap();
        let mapped_first: IpAddr = "::ffff:192.0.2.10".parse().unwrap();
        let mapped_other: IpAddr = "::ffff:192.0.2.11".parse().unwrap();

        assert_eq!(canonical_guest_ip(first), first);
        assert_ne!(canonical_guest_ip(first), canonical_guest_ip(other));
        assert_eq!(canonical_guest_ip(mapped_first), first);
        assert_ne!(
            canonical_guest_ip(mapped_first),
            canonical_guest_ip(mapped_other)
        );
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
        let room_id = format!("history-{}", Uuid::new_v4());
        let other_room_id = format!("history-other-{}", Uuid::new_v4());
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
        let owner_id = owner.to_string();
        let member_id = member.to_string();
        let actor = Actor {
            id: &owner_id,
            name: "Owner",
        };
        let target = Target {
            id: &member_id,
            name: "Maya",
            authenticated: true,
            ip: None,
        };

        // A ban that answers a report resolves it in the same transaction.
        let ban = ModerationEvent::new(
            ModerationAction::Ban,
            actor,
            target,
            Some("spam"),
            None,
            Some(report),
        );
        persist_ban(
            &pool,
            &room_id,
            Some(member),
            None,
            Some("spam"),
            None,
            owner,
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
            (status.as_str(), resolved_by.as_deref()),
            ("resolved", Some(owner_id.as_str()))
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
            target,
            None,
            None,
            Some(foreign_report),
        );
        assert!(matches!(
            persist_event(&pool, &room_id, &foreign).await,
            Err(sqlx::Error::RowNotFound)
        ));
        let (status,): (String,) = sqlx::query_as("SELECT status FROM room_reports WHERE id=$1")
            .bind(foreign_report)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(status, "open");

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
        let guest_id = Uuid::new_v4().to_string();
        let guest = ModerationEvent::new(
            ModerationAction::Kick,
            actor,
            Target {
                id: &guest_id,
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
        assert_eq!(newest.to_json(true)["targetIp"], "2001:db8:1:2::");
        assert!(newest.to_json(false).get("targetIp").is_none());
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
            sqlx::query("INSERT INTO invites(code_hash,kind,room_id,role,created_by,uses_left,expires_at) VALUES($1,'room',$2,1,$3,0,now()-make_interval(days=>$4))")
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
