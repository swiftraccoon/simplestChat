#![forbid(unsafe_code)]

use crate::room::roles::Role;
use mediasoup::prelude::MediaKind;
use sqlx::PgPool;
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
pub async fn set_punitive_state(
    pool: &PgPool,
    room_id: &str,
    user_id: Option<Uuid>,
    ip: Option<IpAddr>,
    kind: PunitiveKind,
    enabled: bool,
    reason: Option<&str>,
    applied_by: Uuid,
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

    transaction.commit().await?;
    Ok(())
}

/// Persist a ban to room_states. Registered targets are keyed by user_id;
/// guests are keyed by their canonical IP cohort.
pub async fn persist_ban(
    pool: &PgPool,
    room_id: &str,
    user_id: Option<Uuid>,
    ip: Option<IpAddr>,
    reason: Option<&str>,
    expires_at: Option<chrono::DateTime<chrono::Utc>>,
    applied_by: Uuid,
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
/// identity to unban through the UI). Returns true if a ban row was deleted.
pub async fn remove_user_ban(
    pool: &PgPool,
    room_id: &str,
    user_id: Uuid,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query(
        "DELETE FROM room_states WHERE room_id = $1 AND state = 'banned' AND user_id = $2",
    )
    .bind(room_id)
    .bind(user_id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
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
}
