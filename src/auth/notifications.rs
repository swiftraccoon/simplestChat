//! Account-wide alert policy. Permission and subscription remain device choices;
//! muting never changes message delivery, inbox membership, or unread cursors.
use super::{
    account::authenticated_principal,
    routes,
    sessions::lock_current_session,
    types::{AuthError, Principal},
};
use crate::{signaling::SignalingServer, sync::RwLock};
use axum::{
    Json,
    extract::{Path, State},
    http::HeaderMap,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{PgConnection, PgPool, Postgres, Transaction};
use std::{
    collections::HashSet,
    time::{Duration, Instant},
};
use uuid::Uuid;

const MAX_CONVERSATIONS: i64 = 100;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct QuietHours {
    start_minute: i64,
    end_minute: i64,
    time_zone: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct NotificationPolicy {
    private_messages: bool,
    mentions: bool,
    quiet_hours: Option<QuietHours>,
}

fn present<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

/// Omitted fields preserve the latest committed value. Only explicit null
/// clears quiet hours; a null boolean is never a missing preference.
#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct NotificationPatch {
    #[serde(deserialize_with = "present")]
    private_messages: Option<bool>,
    #[serde(deserialize_with = "present")]
    mentions: Option<bool>,
    #[serde(deserialize_with = "present")]
    quiet_hours: Option<Option<QuietHours>>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ConversationPolicy {
    muted: bool,
    snoozed_until: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ConversationPreference {
    peer_id: Uuid,
    muted: bool,
    snoozed_until: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NotificationPreferences {
    #[serde(flatten)]
    policy: NotificationPolicy,
    conversations: Vec<ConversationPreference>,
}

#[derive(sqlx::FromRow)]
struct StoredPolicy {
    private_messages: bool,
    mentions: bool,
    quiet_start: Option<i64>,
    quiet_end: Option<i64>,
    quiet_timezone: Option<String>,
}

async fn preferences(pool: &PgPool, user: Uuid) -> Result<NotificationPreferences, AuthError> {
    let policy: Option<StoredPolicy> = sqlx::query_as(
        "SELECT private_messages,mentions,quiet_start,quiet_end,quiet_timezone
         FROM account_notification_preferences WHERE user_id=$1",
    )
    .bind(user)
    .fetch_optional(pool)
    .await
    .map_err(routes::database_error)?;
    let policy = decode_policy(policy);
    let conversations = sqlx::query_as(
        "SELECT peer_id,muted,CASE WHEN snoozed_until>clock_timestamp() THEN snoozed_until ELSE NULL END AS snoozed_until
         FROM conversation_notification_preferences WHERE user_id=$1
         AND (muted OR snoozed_until>clock_timestamp()) ORDER BY peer_id LIMIT $2",
    ).bind(user).bind(MAX_CONVERSATIONS).fetch_all(pool).await.map_err(routes::database_error)?;
    Ok(NotificationPreferences {
        policy,
        conversations,
    })
}

fn decode_policy(policy: Option<StoredPolicy>) -> NotificationPolicy {
    match policy {
        Some(stored) => NotificationPolicy {
            private_messages: stored.private_messages,
            mentions: stored.mentions,
            quiet_hours: stored
                .quiet_start
                .zip(stored.quiet_end)
                .zip(stored.quiet_timezone)
                .map(|((start_minute, end_minute), time_zone)| QuietHours {
                    start_minute,
                    end_minute,
                    time_zone,
                }),
        },
        None => NotificationPolicy {
            private_messages: true,
            mentions: true,
            quiet_hours: None,
        },
    }
}

fn validate_quiet_hours(quiet: &QuietHours) -> Result<(), AuthError> {
    if !(0..1440).contains(&quiet.start_minute)
        || !(0..1440).contains(&quiet.end_minute)
        || quiet.start_minute == quiet.end_minute
    {
        return Err(AuthError::InvalidInput(
            "Quiet hours need different start and end times",
        ));
    }
    if quiet.time_zone.is_empty()
        || quiet.time_zone.len() > 64
        || !quiet
            .time_zone
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'/' | b'_' | b'-' | b'+'))
    {
        return Err(AuthError::InvalidInput("Choose a valid IANA time zone"));
    }
    Ok(())
}

/// How long a copy of PostgreSQL's time zone names answers lookups; the next
/// lookup after that reads the names again.
const TIME_ZONE_REFRESH: Duration = Duration::from_secs(3600);

/// PostgreSQL's time zone names, which quiet hours must name. Saving a policy
/// checks this copy, read again at most once an hour, instead of asking the
/// database on every save.
#[derive(Default)]
pub(crate) struct TimeZoneNames {
    names: RwLock<Option<(Instant, HashSet<String>)>>,
    /// Held from the re-check of a stale copy until the new one is stored, so
    /// callers that find the copy stale together run one query. Callers that
    /// find it fresh never take it.
    refresh: tokio::sync::Mutex<()>,
}

impl TimeZoneNames {
    /// Whether `name` is exactly one of `pg_timezone_names`' names, compared
    /// byte for byte as the database compared them. A copy older than
    /// `TIME_ZONE_REFRESH` is read again on `connection`, the one the caller
    /// already holds, so a lookup never asks the pool for another. One read
    /// runs at a time; callers that waited for it answer from its copy. A
    /// failed read is a database error and keeps the old copy, which the next
    /// lookup reads again.
    pub(crate) async fn contains(
        &self,
        connection: &mut PgConnection,
        name: &str,
    ) -> Result<bool, AuthError> {
        self.contains_at(connection, name, Instant::now()).await
    }

    async fn contains_at(
        &self,
        connection: &mut PgConnection,
        name: &str,
        now: Instant,
    ) -> Result<bool, AuthError> {
        if let Some(known) = self.cached(name, now) {
            return Ok(known);
        }
        let _refresh = self.refresh.lock().await;
        // A caller that held the refresh lock may have stored a fresh copy.
        if let Some(known) = self.cached(name, now) {
            return Ok(known);
        }
        let names: HashSet<String> =
            sqlx::query_scalar::<_, String>("SELECT name FROM pg_timezone_names")
                .fetch_all(&mut *connection)
                .await
                .map_err(routes::database_error)?
                .into_iter()
                .collect();
        let known = names.contains(name);
        *self.names.write() = Some((now, names));
        Ok(known)
    }

    /// The answer from a copy younger than `TIME_ZONE_REFRESH` at `now`, if
    /// there is one. The read lock is released when this returns, so it is
    /// never held across a query.
    fn cached(&self, name: &str, now: Instant) -> Option<bool> {
        let cached = self.names.read();
        let (refreshed_at, names) = cached.as_ref()?;
        (now.saturating_duration_since(*refreshed_at) < TIME_ZONE_REFRESH)
            .then(|| names.contains(name))
    }
}

async fn save_policy(
    pool: &PgPool,
    zones: &TimeZoneNames,
    principal: &Principal,
    policy: &NotificationPolicy,
) -> Result<Uuid, AuthError> {
    principal.ensure_unexpired()?;
    let mut tx = pool.begin().await.map_err(routes::database_error)?;
    let (user, _) = lock_current_session(&mut tx, principal).await?;
    current_policy_session(&mut tx, principal, user).await?;
    write_policy(&mut tx, zones, user, policy).await?;
    current_policy_session(&mut tx, principal, user).await?;
    tx.commit().await.map_err(routes::database_error)?;
    Ok(user)
}

async fn current_policy_session(
    tx: &mut Transaction<'_, Postgres>,
    principal: &Principal,
    user: Uuid,
) -> Result<(), AuthError> {
    let live: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT FROM sessions WHERE id=$1 AND user_id=$2 AND expires_at>clock_timestamp())")
        .bind(principal.sid).bind(user).fetch_one(&mut **tx).await.map_err(routes::database_error)?;
    if !live {
        return Err(AuthError::InvalidToken);
    }
    principal.ensure_unexpired()
}

async fn patch_policy(
    pool: &PgPool,
    zones: &TimeZoneNames,
    principal: &Principal,
    patch: NotificationPatch,
) -> Result<Uuid, AuthError> {
    if patch.private_messages.is_none() && patch.mentions.is_none() && patch.quiet_hours.is_none() {
        return Err(AuthError::InvalidInput(
            "Choose a notification setting to change",
        ));
    }
    principal.ensure_unexpired()?;
    let mut tx = pool.begin().await.map_err(routes::database_error)?;
    // The account lock serializes creation of an absent policy as well as all
    // PUT/PATCH writers; the retained row is read only after these locks wait.
    let (user, _) = lock_current_session(&mut tx, principal).await?;
    current_policy_session(&mut tx, principal, user).await?;
    let stored: Option<StoredPolicy> = sqlx::query_as(
        "SELECT private_messages,mentions,quiet_start,quiet_end,quiet_timezone
         FROM account_notification_preferences WHERE user_id=$1 FOR UPDATE",
    )
    .bind(user)
    .fetch_optional(&mut *tx)
    .await
    .map_err(routes::database_error)?;
    let mut policy = decode_policy(stored);
    if let Some(value) = patch.private_messages {
        policy.private_messages = value;
    }
    if let Some(value) = patch.mentions {
        policy.mentions = value;
    }
    if let Some(value) = patch.quiet_hours {
        policy.quiet_hours = value;
    }
    write_policy(&mut tx, zones, user, &policy).await?;
    current_policy_session(&mut tx, principal, user).await?;
    tx.commit().await.map_err(routes::database_error)?;
    Ok(user)
}

async fn write_policy(
    tx: &mut Transaction<'_, Postgres>,
    zones: &TimeZoneNames,
    user: Uuid,
    policy: &NotificationPolicy,
) -> Result<(), AuthError> {
    if let Some(quiet) = &policy.quiet_hours {
        validate_quiet_hours(quiet)?;
    }
    // The lookup reads on the transaction's own connection, so a save never
    // needs a second pool connection.
    if let Some(quiet) = &policy.quiet_hours
        && !zones.contains(tx, &quiet.time_zone).await?
    {
        return Err(AuthError::InvalidInput("Choose a valid IANA time zone"));
    }
    sqlx::query("INSERT INTO account_notification_preferences(user_id,private_messages,mentions,quiet_start,quiet_end,quiet_timezone)
        VALUES($1,$2,$3,$4,$5,$6) ON CONFLICT(user_id) DO UPDATE SET private_messages=EXCLUDED.private_messages,
        mentions=EXCLUDED.mentions,quiet_start=EXCLUDED.quiet_start,quiet_end=EXCLUDED.quiet_end,quiet_timezone=EXCLUDED.quiet_timezone")
        .bind(user).bind(policy.private_messages).bind(policy.mentions)
        .bind(policy.quiet_hours.as_ref().map(|q|q.start_minute))
        .bind(policy.quiet_hours.as_ref().map(|q|q.end_minute))
        .bind(policy.quiet_hours.as_ref().map(|q|q.time_zone.as_str()))
        .execute(&mut **tx).await.map_err(routes::database_error)?;
    Ok(())
}

fn validate_snooze(policy: &ConversationPolicy, now: DateTime<Utc>) -> Result<(), AuthError> {
    if policy
        .snoozed_until
        .is_some_and(|until| until <= now || until > now + chrono::Duration::days(30))
    {
        return Err(AuthError::InvalidInput(
            "Snooze must end within the next 30 days",
        ));
    }
    Ok(())
}

async fn reserve_conversation(
    tx: &mut Transaction<'_, Postgres>,
    user: Uuid,
    peer: Uuid,
) -> Result<(), AuthError> {
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users WHERE id=$1)")
        .bind(peer)
        .fetch_one(&mut **tx)
        .await
        .map_err(routes::database_error)?;
    if !exists || user == peer {
        return Err(AuthError::InvalidInput("Choose another account"));
    }
    sqlx::query("DELETE FROM conversation_notification_preferences WHERE user_id=$1 AND NOT muted AND snoozed_until<=clock_timestamp()")
        .bind(user).execute(&mut **tx).await.map_err(routes::database_error)?;
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM conversation_notification_preferences WHERE user_id=$1 AND peer_id<>$2")
        .bind(user).bind(peer).fetch_one(&mut **tx).await.map_err(routes::database_error)?;
    if count >= MAX_CONVERSATIONS {
        return Err(AuthError::InvalidInput(
            "Notification overrides are limited to 100 conversations",
        ));
    }
    Ok(())
}

async fn save_conversation(
    pool: &PgPool,
    principal: &Principal,
    peer: Uuid,
    policy: &ConversationPolicy,
) -> Result<Uuid, AuthError> {
    validate_snooze(policy, Utc::now())?;
    let mut tx = pool.begin().await.map_err(routes::database_error)?;
    let (user, _) = lock_current_session(&mut tx, principal).await?;
    if !policy.muted && policy.snoozed_until.is_none() {
        sqlx::query(
            "DELETE FROM conversation_notification_preferences WHERE user_id=$1 AND peer_id=$2",
        )
        .bind(user)
        .bind(peer)
        .execute(&mut *tx)
        .await
        .map_err(routes::database_error)?;
    } else {
        reserve_conversation(&mut tx, user, peer).await?;
        sqlx::query("INSERT INTO conversation_notification_preferences(user_id,peer_id,muted,snoozed_until) VALUES($1,$2,$3,$4)
            ON CONFLICT(user_id,peer_id) DO UPDATE SET muted=EXCLUDED.muted,snoozed_until=EXCLUDED.snoozed_until")
            .bind(user).bind(peer).bind(policy.muted).bind(policy.snoozed_until)
            .execute(&mut *tx).await.map_err(routes::database_error)?;
    }
    tx.commit().await.map_err(routes::database_error)?;
    Ok(user)
}

pub(crate) async fn get_preferences(
    State(server): State<SignalingServer>,
    headers: HeaderMap,
) -> Result<(HeaderMap, Json<NotificationPreferences>), AuthError> {
    let _permit = routes::acquire_account_request(&server)?;
    let principal = authenticated_principal(&server, &headers).await?;
    let user = principal.user.as_uuid();
    let result = preferences(server.db_pool().ok_or(AuthError::NotConfigured)?, user).await?;
    Ok((routes::no_store_headers(), Json(result)))
}

pub(crate) async fn put_preferences(
    State(server): State<SignalingServer>,
    headers: HeaderMap,
    Json(policy): Json<NotificationPolicy>,
) -> Result<(HeaderMap, Json<NotificationPreferences>), AuthError> {
    let permit = routes::acquire_account_request(&server)?;
    let principal = authenticated_principal(&server, &headers).await?;
    let result = super::updates::owned(permit, async move {
        let pool = server.db_pool().ok_or(AuthError::NotConfigured)?;
        let user = save_policy(pool, server.time_zone_names(), &principal, &policy).await?;
        server.room_manager().account_updates().changed(
            user,
            super::updates::Scope::Notifications,
            None,
        );
        preferences(pool, user).await
    })
    .await?;
    Ok((routes::no_store_headers(), Json(result)))
}

pub(crate) async fn patch_preferences(
    State(server): State<SignalingServer>,
    headers: HeaderMap,
    Json(patch): Json<NotificationPatch>,
) -> Result<(HeaderMap, Json<NotificationPreferences>), AuthError> {
    let permit = routes::acquire_account_request(&server)?;
    let principal = authenticated_principal(&server, &headers).await?;
    let result = super::updates::owned(permit, async move {
        let pool = server.db_pool().ok_or(AuthError::NotConfigured)?;
        let user = patch_policy(pool, server.time_zone_names(), &principal, patch).await?;
        server.room_manager().account_updates().changed(
            user,
            super::updates::Scope::Notifications,
            None,
        );
        preferences(pool, user).await
    })
    .await?;
    Ok((routes::no_store_headers(), Json(result)))
}

pub(crate) async fn put_conversation(
    State(server): State<SignalingServer>,
    headers: HeaderMap,
    Path(peer): Path<Uuid>,
    Json(policy): Json<ConversationPolicy>,
) -> Result<(HeaderMap, Json<NotificationPreferences>), AuthError> {
    let permit = routes::acquire_account_request(&server)?;
    let principal = authenticated_principal(&server, &headers).await?;
    let result = super::updates::owned(permit, async move {
        let pool = server.db_pool().ok_or(AuthError::NotConfigured)?;
        let user = save_conversation(pool, &principal, peer, &policy).await?;
        server.room_manager().account_updates().changed(
            user,
            super::updates::Scope::Notifications,
            Some(peer),
        );
        preferences(pool, user).await
    })
    .await?;
    Ok((routes::no_store_headers(), Json(result)))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn notification_patch_distinguishes_omitted_quiet_hours_from_explicit_clear() {
        let patch: NotificationPatch =
            serde_json::from_value(serde_json::json!({"mentions":false})).unwrap();
        assert_eq!(patch.mentions, Some(false));
        assert!(patch.private_messages.is_none());
        assert!(patch.quiet_hours.is_none());
        let patch: NotificationPatch =
            serde_json::from_value(serde_json::json!({"quietHours":null})).unwrap();
        assert!(matches!(patch.quiet_hours, Some(None)));
        for invalid in [
            serde_json::json!({"privateMessages":null}),
            serde_json::json!({"mentions":null}),
            serde_json::json!({"unexpected":true}),
            serde_json::json!({"quietHours":{"startMinute":0}}),
        ] {
            assert!(serde_json::from_value::<NotificationPatch>(invalid).is_err());
        }
    }

    #[test]
    fn quiet_hours_require_bounded_different_times_and_named_zone() {
        let mut quiet = QuietHours {
            start_minute: 1320,
            end_minute: 420,
            time_zone: "America/New_York".into(),
        };
        assert!(validate_quiet_hours(&quiet).is_ok());
        quiet.end_minute = 1320;
        assert!(validate_quiet_hours(&quiet).is_err());
        quiet.end_minute = 1440;
        assert!(validate_quiet_hours(&quiet).is_err());
        quiet.end_minute = 420;
        quiet.time_zone = "PST8PDT,M3.2.0,M11.1.0".into();
        assert!(validate_quiet_hours(&quiet).is_err());
        quiet.time_zone = "Etc/GMT+5".into();
        assert!(validate_quiet_hours(&quiet).is_ok());
    }
    #[test]
    fn snoozes_expire_and_cannot_reserve_unbounded_time() {
        let now = Utc::now();
        for delta in [1, 3600, 30 * 86400] {
            assert!(
                validate_snooze(
                    &ConversationPolicy {
                        muted: false,
                        snoozed_until: Some(now + chrono::Duration::seconds(delta))
                    },
                    now
                )
                .is_ok()
            );
        }
        for delta in [-1, 0, 30 * 86400 + 1] {
            assert!(
                validate_snooze(
                    &ConversationPolicy {
                        muted: false,
                        snoozed_until: Some(now + chrono::Duration::seconds(delta))
                    },
                    now
                )
                .is_err()
            );
        }
    }

    /// A pool of `connections` on the disposable database, which must be a
    /// loopback database whose name ends in `_test`. A request for a connection
    /// beyond those fails after five seconds instead of the default timeout.
    async fn database_pool(connections: u32) -> PgPool {
        use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
        use std::str::FromStr;
        let url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL");
        let options = PgConnectOptions::from_str(&url).unwrap();
        assert!(
            matches!(options.get_host(), "127.0.0.1" | "::1" | "localhost")
                && options
                    .get_database()
                    .is_some_and(|name| name.ends_with("_test"))
        );
        PgPoolOptions::new()
            .max_connections(connections)
            .acquire_timeout(Duration::from_secs(5))
            .connect_with(options)
            .await
            .unwrap()
    }

    /// A new account on the disposable database.
    async fn account(pool: &PgPool) -> Uuid {
        sqlx::query_scalar(
            "INSERT INTO users(email,display_name) VALUES($1,'Notification policy tester') RETURNING id",
        )
        .bind(format!("{}@notifications.invalid", Uuid::new_v4()))
        .fetch_one(pool)
        .await
        .unwrap()
    }

    /// The principal of a fresh token for a new live session of `user`.
    async fn signed_in(pool: &PgPool, user: Uuid) -> Principal {
        let refresh = super::super::session::generate_refresh_token().unwrap();
        let sid = super::super::session::create_session(pool, &user, &refresh)
            .await
            .unwrap();
        super::super::access_tokens::fixtures::principal(pool, sid).await
    }

    /// Private messages off, mentions on, quiet from 22:00 to 07:00 in New York.
    fn quiet_policy() -> NotificationPolicy {
        NotificationPolicy {
            private_messages: false,
            mentions: true,
            quiet_hours: Some(QuietHours {
                start_minute: 1320,
                end_minute: 420,
                time_zone: "America/New_York".into(),
            }),
        }
    }

    /// A transaction whose first statement failed, so any later query on it
    /// fails too: a lookup that answers on it ran no query.
    async fn aborted(pool: &PgPool) -> Transaction<'static, Postgres> {
        let mut transaction = pool.begin().await.unwrap();
        let refused = sqlx::query("SELECT 1/0")
            .execute(&mut *transaction)
            .await
            .unwrap_err();
        assert!(matches!(refused, sqlx::Error::Database(_)));
        transaction
    }

    /// The first lookup reads PostgreSQL's names, and for the rest of the hour
    /// the copy answers alone: lookups on an aborted transaction, where any
    /// query fails, still answer. An hour on, the next lookup reads the names
    /// again.
    #[tokio::test]
    #[ignore = "requires a migrated disposable TEST_DATABASE_URL"]
    async fn time_zone_names_are_cached_for_an_hour() {
        let pool = database_pool(2).await;
        let mut live = pool.acquire().await.unwrap();
        let mut dead = aborted(&pool).await;
        let zones = TimeZoneNames::default();
        let start = Instant::now();
        assert!(
            zones
                .contains_at(&mut live, "America/New_York", start)
                .await
                .unwrap()
        );
        let within = start + TIME_ZONE_REFRESH - Duration::from_secs(1);
        assert!(
            zones
                .contains_at(&mut dead, "America/New_York", within)
                .await
                .unwrap()
        );
        assert!(
            !zones
                .contains_at(&mut dead, "Unknown/Nowhere", within)
                .await
                .unwrap()
        );
        assert!(
            !zones
                .contains_at(&mut dead, "america/new_york", within)
                .await
                .unwrap(),
            "names compare exactly, as the database compared them"
        );
        let expired = start + TIME_ZONE_REFRESH;
        assert!(
            zones
                .contains_at(&mut dead, "America/New_York", expired)
                .await
                .is_err(),
            "an hour on, a lookup reads the names again"
        );
        assert!(
            zones
                .contains_at(&mut live, "Etc/GMT+5", expired)
                .await
                .unwrap()
        );
        assert!(
            zones
                .contains_at(
                    &mut dead,
                    "America/New_York",
                    expired + Duration::from_secs(1)
                )
                .await
                .unwrap(),
            "the refresh starts a new hour"
        );
        dead.rollback().await.unwrap();
        drop(live);
        pool.close().await;
    }

    /// Two lookups that find the copy stale at the same time read the names
    /// once: the second waits for the first one's read and answers from its
    /// copy, on an aborted transaction where a read of its own would fail.
    #[tokio::test]
    #[ignore = "requires a migrated disposable TEST_DATABASE_URL"]
    async fn concurrent_stale_time_zone_lookups_read_the_names_once() {
        let pool = database_pool(2).await;
        let mut live = pool.acquire().await.unwrap();
        let mut dead = aborted(&pool).await;
        let zones = TimeZoneNames::default();
        let now = Instant::now();
        let (first, second) = {
            let mut first = std::pin::pin!(zones.contains_at(&mut live, "America/New_York", now));
            // #[tokio::test] runs a current-thread runtime, so nothing drives
            // the I/O between this poll and the join's first poll: the first
            // lookup cannot finish its read early, and it still holds the
            // refresh lock when the second one starts.
            assert!(
                futures_util::poll!(&mut first).is_pending(),
                "the first lookup is reading the names"
            );
            tokio::join!(first, zones.contains_at(&mut dead, "Etc/GMT+5", now))
        };
        assert!(first.unwrap());
        assert!(
            second.unwrap(),
            "the second lookup answered from the first one's copy"
        );
        dead.rollback().await.unwrap();
        drop(live);
        pool.close().await;
    }

    /// A policy save checks its zone on the connection its transaction holds:
    /// on a pool of one connection, with no copy of the names yet, the save
    /// that reads them still completes. A lookup that asked the pool for a
    /// second connection would wait for the transaction's own until the
    /// acquire timeout failed it.
    #[tokio::test]
    #[ignore = "requires a migrated disposable TEST_DATABASE_URL"]
    async fn policy_saves_read_time_zones_on_their_own_connection() {
        let pool = database_pool(1).await;
        let user = account(&pool).await;
        let principal = signed_in(&pool, user).await;
        save_policy(
            &pool,
            &TimeZoneNames::default(),
            &principal,
            &quiet_policy(),
        )
        .await
        .unwrap();
        let saved = preferences(&pool, user).await.unwrap().policy.quiet_hours;
        assert_eq!(
            saved.map(|quiet| quiet.time_zone).as_deref(),
            Some("America/New_York")
        );
        sqlx::query("DELETE FROM users WHERE id=$1")
            .bind(user)
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;
    }

    #[tokio::test]
    #[ignore = "requires a migrated disposable TEST_DATABASE_URL"]
    async fn database_notification_preferences_preserve_ownership_and_reject_revoked_writes() {
        let pool = database_pool(2).await;
        let zones = TimeZoneNames::default();
        let users = vec![account(&pool).await, account(&pool).await];
        let user = users[0];
        let peer = users[1];
        let principal = signed_in(&pool, user).await;
        let mut policy = quiet_policy();
        save_policy(&pool, &zones, &principal, &policy)
            .await
            .unwrap();
        assert!(
            !preferences(&pool, user)
                .await
                .unwrap()
                .policy
                .private_messages
        );
        assert!(
            preferences(&pool, peer)
                .await
                .unwrap()
                .policy
                .private_messages
        );
        policy.quiet_hours.as_mut().unwrap().time_zone = "Unknown/Nowhere".into();
        assert!(
            save_policy(&pool, &zones, &principal, &policy)
                .await
                .is_err()
        );
        let mute = ConversationPolicy {
            muted: true,
            snoozed_until: None,
        };
        save_conversation(&pool, &principal, peer, &mute)
            .await
            .unwrap();
        assert_eq!(
            preferences(&pool, user).await.unwrap().conversations.len(),
            1
        );
        assert!(
            preferences(&pool, peer)
                .await
                .unwrap()
                .conversations
                .is_empty()
        );
        assert!(
            save_conversation(&pool, &principal, user, &mute)
                .await
                .is_err()
        );
        save_conversation(
            &pool,
            &principal,
            peer,
            &ConversationPolicy {
                muted: false,
                snoozed_until: None,
            },
        )
        .await
        .unwrap();
        assert!(
            preferences(&pool, user)
                .await
                .unwrap()
                .conversations
                .is_empty()
        );
        sqlx::query("DELETE FROM sessions WHERE id=$1")
            .bind(principal.sid)
            .execute(&pool)
            .await
            .unwrap();
        assert!(matches!(
            save_conversation(&pool, &principal, peer, &mute).await,
            Err(AuthError::InvalidToken)
        ));
        policy.quiet_hours = None;
        assert!(matches!(
            save_policy(&pool, &zones, &principal, &policy).await,
            Err(AuthError::InvalidToken)
        ));
        sqlx::query("DELETE FROM users WHERE id=ANY($1)")
            .bind(users)
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;
    }
}
