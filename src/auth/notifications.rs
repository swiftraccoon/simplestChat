//! Account-wide alert policy. Permission and subscription remain device choices;
//! muting never changes message delivery, inbox membership, or unread cursors.
use super::{
    account::authenticated_claims,
    routes,
    sessions::lock_current_session,
    types::{AuthError, Claims},
};
use crate::signaling::SignalingServer;
use axum::{
    Json,
    extract::{Path, State},
    http::HeaderMap,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

const MAX_CONVERSATIONS: i64 = 100;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QuietHours {
    start_minute: i64,
    end_minute: i64,
    time_zone: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NotificationPolicy {
    private_messages: bool,
    mentions: bool,
    quiet_hours: Option<QuietHours>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConversationPolicy {
    muted: bool,
    snoozed_until: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct ConversationPreference {
    peer_id: Uuid,
    muted: bool,
    snoozed_until: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NotificationPreferences {
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
    let policy = match policy {
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
    };
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

async fn save_policy(
    pool: &PgPool,
    claims: &Claims,
    policy: &NotificationPolicy,
) -> Result<Uuid, AuthError> {
    if let Some(quiet) = &policy.quiet_hours {
        validate_quiet_hours(quiet)?;
    }
    let mut tx = pool.begin().await.map_err(routes::database_error)?;
    let (user, _) = lock_current_session(&mut tx, claims).await?;
    if let Some(quiet) = &policy.quiet_hours {
        let recognized: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_timezone_names WHERE name=$1)")
                .bind(&quiet.time_zone)
                .fetch_one(&mut *tx)
                .await
                .map_err(routes::database_error)?;
        if !recognized {
            return Err(AuthError::InvalidInput("Choose a valid IANA time zone"));
        }
    }
    sqlx::query("INSERT INTO account_notification_preferences(user_id,private_messages,mentions,quiet_start,quiet_end,quiet_timezone)
        VALUES($1,$2,$3,$4,$5,$6) ON CONFLICT(user_id) DO UPDATE SET private_messages=EXCLUDED.private_messages,
        mentions=EXCLUDED.mentions,quiet_start=EXCLUDED.quiet_start,quiet_end=EXCLUDED.quiet_end,quiet_timezone=EXCLUDED.quiet_timezone")
        .bind(user).bind(policy.private_messages).bind(policy.mentions)
        .bind(policy.quiet_hours.as_ref().map(|q|q.start_minute))
        .bind(policy.quiet_hours.as_ref().map(|q|q.end_minute))
        .bind(policy.quiet_hours.as_ref().map(|q|q.time_zone.as_str()))
        .execute(&mut *tx).await.map_err(routes::database_error)?;
    tx.commit().await.map_err(routes::database_error)?;
    Ok(user)
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
    claims: &Claims,
    peer: Uuid,
    policy: &ConversationPolicy,
) -> Result<Uuid, AuthError> {
    validate_snooze(policy, Utc::now())?;
    let mut tx = pool.begin().await.map_err(routes::database_error)?;
    let (user, _) = lock_current_session(&mut tx, claims).await?;
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

pub async fn get_preferences(
    State(server): State<SignalingServer>,
    headers: HeaderMap,
) -> Result<(HeaderMap, Json<NotificationPreferences>), AuthError> {
    let _permit = routes::acquire_auth_request(&server)?;
    let claims = authenticated_claims(&server, &headers).await?;
    let user = Uuid::parse_str(&claims.sub).map_err(|_| AuthError::InvalidToken)?;
    let result = preferences(server.db_pool().ok_or(AuthError::NotConfigured)?, user).await?;
    Ok((routes::no_store_headers(), Json(result)))
}

pub async fn put_preferences(
    State(server): State<SignalingServer>,
    headers: HeaderMap,
    Json(policy): Json<NotificationPolicy>,
) -> Result<(HeaderMap, Json<NotificationPreferences>), AuthError> {
    let _permit = routes::acquire_auth_request(&server)?;
    let claims = authenticated_claims(&server, &headers).await?;
    let pool = server.db_pool().ok_or(AuthError::NotConfigured)?;
    let user = save_policy(pool, &claims, &policy).await?;
    Ok((
        routes::no_store_headers(),
        Json(preferences(pool, user).await?),
    ))
}

pub async fn put_conversation(
    State(server): State<SignalingServer>,
    headers: HeaderMap,
    Path(peer): Path<Uuid>,
    Json(policy): Json<ConversationPolicy>,
) -> Result<(HeaderMap, Json<NotificationPreferences>), AuthError> {
    let _permit = routes::acquire_auth_request(&server)?;
    let claims = authenticated_claims(&server, &headers).await?;
    let pool = server.db_pool().ok_or(AuthError::NotConfigured)?;
    let user = save_conversation(pool, &claims, peer, &policy).await?;
    Ok((
        routes::no_store_headers(),
        Json(preferences(pool, user).await?),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
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

    #[tokio::test]
    #[ignore = "requires a migrated disposable TEST_DATABASE_URL"]
    async fn database_notification_preferences_preserve_ownership_and_reject_revoked_writes() {
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
        let pool = PgPoolOptions::new()
            .max_connections(2)
            .connect_with(options)
            .await
            .unwrap();
        let mut users = Vec::new();
        for _ in 0..2 {
            let user: Uuid=sqlx::query_scalar("INSERT INTO users(email,display_name) VALUES($1,'Notification policy tester') RETURNING id")
                .bind(format!("{}@notifications.invalid",Uuid::new_v4())).fetch_one(&pool).await.unwrap();
            users.push(user);
        }
        let user = users[0];
        let peer = users[1];
        let refresh = super::super::session::generate_refresh_token().unwrap();
        let sid = super::super::session::create_session(&pool, &user, &refresh)
            .await
            .unwrap();
        let claims = Claims {
            sub: user.to_string(),
            name: "tester".into(),
            iss: "simplestChat".into(),
            aud: "simplestChat".into(),
            exp: (Utc::now().timestamp() + 3600) as usize,
            auth_version: 0,
            sid,
        };
        let mut policy = NotificationPolicy {
            private_messages: false,
            mentions: true,
            quiet_hours: Some(QuietHours {
                start_minute: 1320,
                end_minute: 420,
                time_zone: "America/New_York".into(),
            }),
        };
        save_policy(&pool, &claims, &policy).await.unwrap();
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
        assert!(save_policy(&pool, &claims, &policy).await.is_err());
        let mute = ConversationPolicy {
            muted: true,
            snoozed_until: None,
        };
        save_conversation(&pool, &claims, peer, &mute)
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
            save_conversation(&pool, &claims, user, &mute)
                .await
                .is_err()
        );
        save_conversation(
            &pool,
            &claims,
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
            .bind(sid)
            .execute(&pool)
            .await
            .unwrap();
        assert!(matches!(
            save_conversation(&pool, &claims, peer, &mute).await,
            Err(AuthError::InvalidToken)
        ));
        policy.quiet_hours = None;
        assert!(matches!(
            save_policy(&pool, &claims, &policy).await,
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
