#![forbid(unsafe_code)]
//! One-use WebSocket upgrade capabilities. Bearer JWTs stay in authenticated
//! HTTP requests and established signaling frames, never handshake protocols.

use super::{
    account, jwt, routes,
    types::{AuthError, Claims},
};
use crate::signaling::SignalingServer;
use axum::{Json, extract::State, http::HeaderMap};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use lru::LruCache;
use rand::{TryRng, rngs::SysRng};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    num::NonZeroUsize,
    sync::Mutex,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const MAX_PENDING: usize = 10_000;
const TICKET_TTL_SECONDS: u64 = 30;

struct Ticket {
    claims: Claims,
    deadline: Instant,
}

/// Bounded pending upgrades for this process. Successful consumption removes
/// the capability atomically before any database await. No raw secret is stored.
pub(crate) struct TicketStore {
    entries: Mutex<LruCache<[u8; 32], Ticket>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TicketRequest {}

#[derive(Serialize)]
pub(crate) struct TicketResponse {
    ticket: String,
    expires_in: u64,
}

fn epoch_seconds() -> Result<u64, AuthError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .map_err(|_| AuthError::ServiceBusy)
}

fn key(ticket: &str) -> [u8; 32] {
    Sha256::digest(ticket.as_bytes()).into()
}

pub(crate) fn valid_shape(ticket: &str) -> bool {
    ticket.len() == 43
        && URL_SAFE_NO_PAD
            .decode(ticket)
            .is_ok_and(|bytes| bytes.len() == 32)
}

impl TicketStore {
    pub(crate) fn new() -> Self {
        Self::with_capacity(MAX_PENDING)
    }

    fn with_capacity(capacity: usize) -> Self {
        Self {
            entries: Mutex::new(LruCache::new(
                NonZeroUsize::new(capacity).expect("nonzero ticket capacity"),
            )),
        }
    }

    fn issue(&self, claims: Claims) -> Result<TicketResponse, AuthError> {
        self.issue_at(claims, Instant::now(), epoch_seconds()?)
    }

    fn issue_at(
        &self,
        claims: Claims,
        now: Instant,
        epoch: u64,
    ) -> Result<TicketResponse, AuthError> {
        let expires_in = (claims.exp as u64)
            .saturating_sub(epoch)
            .min(TICKET_TTL_SECONDS);
        if expires_in == 0 {
            return Err(AuthError::TokenExpired);
        }
        let mut bytes = [0_u8; 32];
        SysRng
            .try_fill_bytes(&mut bytes)
            .map_err(|_| AuthError::ServiceBusy)?;
        let ticket = URL_SAFE_NO_PAD.encode(bytes);
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        // Lifetimes can differ near JWT expiry, so retire every expired entry
        // only at capacity. Normal issuance and consumption remain O(1).
        if entries.len() == entries.cap().get() {
            let expired: Vec<_> = entries
                .iter()
                .filter(|(_, entry)| entry.deadline <= now)
                .map(|(key, _)| *key)
                .collect();
            for key in expired {
                entries.pop(&key);
            }
        }
        if entries.len() == entries.cap().get() {
            return Err(AuthError::ServiceBusy);
        }
        entries.put(
            key(&ticket),
            Ticket {
                claims,
                deadline: now + Duration::from_secs(expires_in),
            },
        );
        Ok(TicketResponse { ticket, expires_in })
    }

    fn take_at(&self, token: &str, now: Instant, epoch: u64) -> Result<Ticket, AuthError> {
        if !valid_shape(token) {
            return Err(AuthError::InvalidToken);
        }
        let ticket = self
            .entries
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .pop(&key(token))
            .ok_or(AuthError::InvalidToken)?;
        if ticket.deadline <= now || ticket.claims.exp as u64 <= epoch {
            return Err(AuthError::InvalidToken);
        }
        Ok(ticket)
    }

    /// Validate session/account revocation again at use, not only at issuance.
    /// An invalid or failed attempt still spends the one-use capability.
    pub(crate) async fn redeem(
        &self,
        pool: &sqlx::PgPool,
        token: &str,
    ) -> Result<Claims, AuthError> {
        let ticket = self.take_at(token, Instant::now(), epoch_seconds()?)?;
        jwt::validate_current_claims(pool, &ticket.claims).await?;
        if ticket.deadline <= Instant::now() || ticket.claims.exp as u64 <= epoch_seconds()? {
            return Err(AuthError::InvalidToken);
        }
        Ok(ticket.claims)
    }
}

/// POST /api/auth/ws-ticket, Authorization: Bearer, JSON {}.
/// A ticket grants one upgrade for the already-authenticated session, never a
/// new login. HTTP origin/rate/body guards and operation admission wrap issuance.
pub(crate) async fn issue(
    State(server): State<SignalingServer>,
    headers: HeaderMap,
    Json(_request): Json<TicketRequest>,
) -> Result<(HeaderMap, Json<TicketResponse>), AuthError> {
    let _permit = routes::acquire_auth_request(&server)?;
    let claims = account::authenticated_claims(&server, &headers).await?;
    if !server.allow_auth_principal(&claims.sub) {
        return Err(AuthError::RateLimited);
    }
    let ticket = server.websocket_tickets().issue(claims)?;
    Ok((routes::no_store_headers(), Json(ticket)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claims(epoch: u64) -> Claims {
        Claims {
            sub: uuid::Uuid::new_v4().to_string(),
            name: "Ticket test".into(),
            iss: "simplestchat".into(),
            aud: "simplestchat".into(),
            exp: (epoch + 900) as usize,
            auth_version: 0,
            sid: uuid::Uuid::new_v4(),
        }
    }

    #[test]
    fn tickets_are_one_use_bounded_and_expire_without_extending_the_jwt() {
        let store = TicketStore::with_capacity(2);
        let now = Instant::now();
        let identity = claims(1000);
        let first = store.issue_at(identity.clone(), now, 1000).unwrap();
        assert!(valid_shape(&first.ticket));
        assert_eq!(first.expires_in, 30);
        let mut near_expiry = identity.clone();
        near_expiry.exp = 1004;
        let second = store.issue_at(near_expiry, now, 1000).unwrap();
        assert_eq!(second.expires_in, 4);
        assert!(matches!(
            store.issue_at(identity.clone(), now, 1000),
            Err(AuthError::ServiceBusy)
        ));
        assert!(
            store
                .take_at(&second.ticket, now + Duration::from_secs(4), 1004)
                .is_err()
        );
        let third = store.issue_at(identity.clone(), now, 1000).unwrap();
        let used = store.take_at(&first.ticket, now, 1000).unwrap();
        assert_eq!(used.claims.sub, identity.sub);
        assert_eq!(used.claims.sid, identity.sid);
        assert!(store.take_at(&first.ticket, now, 1000).is_err());
        assert!(
            store
                .take_at(&third.ticket, now + Duration::from_secs(30), 1030)
                .is_err()
        );
    }

    #[test]
    fn capacity_reclaims_expired_tickets_and_concurrent_consumers_have_one_winner() {
        let store = std::sync::Arc::new(TicketStore::with_capacity(1));
        let now = Instant::now();
        store.issue_at(claims(1000), now, 1000).unwrap();
        let ticket = store
            .issue_at(claims(1030), now + Duration::from_secs(30), 1030)
            .unwrap();
        let winners = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..8)
                .map(|_| {
                    scope.spawn(|| {
                        store
                            .take_at(&ticket.ticket, now + Duration::from_secs(30), 1030)
                            .is_ok()
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .filter(|won| *won)
                .count()
        });
        assert_eq!(winners, 1);
        assert!(store.entries.lock().unwrap().is_empty());
    }

    #[tokio::test]
    #[ignore = "requires TEST_DATABASE_URL pointing to a migrated disposable PostgreSQL database"]
    async fn database_tickets_recheck_logout_account_version_and_session_binding() {
        use super::super::session;
        let pool =
            sqlx::PgPool::connect(&std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL"))
                .await
                .unwrap();
        let user = uuid::Uuid::new_v4();
        sqlx::query("INSERT INTO users(id,email,display_name) VALUES($1,$2,'Ticket test')")
            .bind(user)
            .bind(format!("{user}@tickets.invalid"))
            .execute(&pool)
            .await
            .unwrap();
        let store = TicketStore::new();
        let mut identities = Vec::new();
        for _ in 0..2 {
            let refresh = session::generate_refresh_token().unwrap();
            let mut transaction = pool.begin().await.unwrap();
            let sid = session::create_session_with(&mut transaction, &user, &refresh)
                .await
                .unwrap();
            transaction.commit().await.unwrap();
            let mut identity = claims(epoch_seconds().unwrap());
            identity.sub = user.to_string();
            identity.sid = sid;
            identities.push((identity, refresh));
        }
        let first = store.issue(identities[0].0.clone()).unwrap();
        let second = store.issue(identities[1].0.clone()).unwrap();
        session::delete_session_by_token(&pool, &identities[0].1.raw)
            .await
            .unwrap();
        assert!(
            store.redeem(&pool, &first.ticket).await.is_err(),
            "logout after issuance invalidates ticket"
        );
        assert_eq!(
            store.redeem(&pool, &second.ticket).await.unwrap().sid,
            identities[1].0.sid
        );
        assert!(
            store.redeem(&pool, &second.ticket).await.is_err(),
            "one-use even with live session"
        );
        let changed = store.issue(identities[1].0.clone()).unwrap();
        sqlx::query("UPDATE users SET auth_version=auth_version+1 WHERE id=$1")
            .bind(user)
            .execute(&pool)
            .await
            .unwrap();
        assert!(
            store.redeem(&pool, &changed.ticket).await.is_err(),
            "credential revocation after issuance invalidates ticket"
        );
        sqlx::query("DELETE FROM users WHERE id=$1")
            .bind(user)
            .execute(&pool)
            .await
            .unwrap();
    }
}
