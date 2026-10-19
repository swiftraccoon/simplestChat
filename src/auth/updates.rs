//! Content-free account invalidations. Every connection starts with a resync;
//! durable HTTP APIs remain authoritative and no mutation is replayed here.
use super::{
    account, jwt,
    types::{AuthError, Claims},
};
use crate::{shutdown::DrainSignal, signaling::SignalingServer};
use axum::{
    extract::State,
    http::{HeaderMap, HeaderValue},
    response::{
        Sse,
        sse::{Event, KeepAlive},
    },
};
use futures_util::stream::{self, Stream};
use serde::Serialize;
use std::{
    collections::HashMap,
    convert::Infallible,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{sync::broadcast, time::Instant};
use uuid::Uuid;

const ACCOUNT_STREAMS: usize = 8;
const QUEUE: usize = 64;
const REVALIDATE: Duration = Duration::from_secs(5);
const LIFETIME: Duration = Duration::from_secs(5 * 60);

/// Retain admission through a bounded commit and its synchronous invalidation,
/// even when the HTTP response is abandoned while COMMIT is in flight.
pub(crate) async fn owned<T, F>(
    permit: tokio::sync::OwnedSemaphorePermit,
    work: F,
) -> Result<T, AuthError>
where
    T: Send + 'static,
    F: std::future::Future<Output = Result<T, AuthError>> + Send + 'static,
{
    tokio::spawn(async move {
        let _permit = permit;
        tokio::time::timeout(Duration::from_secs(15), work)
            .await
            .map_err(|_| AuthError::ServiceBusy)?
    })
    .await
    .map_err(|_| AuthError::ServiceBusy)?
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Scope {
    Inbox,
    Contacts,
    Preferences,
    Notifications,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub(crate) enum Update {
    Ready,
    Change {
        scope: Scope,
        #[serde(rename = "peerId", skip_serializing_if = "Option::is_none")]
        peer_id: Option<Uuid>,
    },
}

#[derive(Clone, Copy, Debug)]
enum Notice {
    Change(Update),
    Revalidate,
}

struct Account {
    sender: broadcast::Sender<Notice>,
    subscribers: usize,
    generation: Arc<AtomicU64>,
}

struct StateMap {
    accounts: HashMap<Uuid, Account>,
    total: usize,
}

struct Inner {
    state: Mutex<StateMap>,
    capacity: usize,
}

#[derive(Clone)]
pub(crate) struct Hub {
    inner: Arc<Inner>,
}

impl Hub {
    pub(crate) fn configured() -> anyhow::Result<Self> {
        Ok(Self::with_capacity(crate::configuration::read_usize(
            "MAX_CONNECTIONS",
            10_000,
            1,
            1_000_000,
        )?))
    }

    fn with_capacity(capacity: usize) -> Self {
        Self {
            inner: Arc::new(Inner {
                state: Mutex::new(StateMap {
                    accounts: HashMap::new(),
                    total: 0,
                }),
                capacity,
            }),
        }
    }

    fn subscribe(&self, account: Uuid) -> Result<Subscription, AuthError> {
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if state.total >= self.inner.capacity {
            return Err(AuthError::ServiceBusy);
        }
        let entry = state.accounts.entry(account).or_insert_with(|| Account {
            sender: broadcast::channel(QUEUE).0,
            subscribers: 0,
            generation: Arc::new(AtomicU64::new(0)),
        });
        if entry.subscribers >= ACCOUNT_STREAMS {
            return Err(AuthError::RateLimited);
        }
        let receiver = entry.sender.subscribe();
        let generation = entry.generation.clone();
        entry.subscribers += 1;
        state.total += 1;
        Ok(Subscription {
            receiver,
            generation,
            _lease: Lease {
                inner: self.inner.clone(),
                account,
            },
        })
    }

    fn send(&self, account: Uuid, notice: Notice) {
        let sender = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .accounts
            .get(&account)
            .map(|entry| entry.sender.clone());
        if let Some(sender) = sender {
            let _ = sender.send(notice);
        }
    }

    pub(crate) fn changed(&self, account: Uuid, scope: Scope, peer_id: Option<Uuid>) {
        self.send(account, Notice::Change(Update::Change { scope, peer_id }));
    }

    pub(crate) fn pair(&self, account: Uuid, peer: Uuid, scope: Scope) {
        self.changed(account, scope, Some(peer));
        self.changed(peer, scope, Some(account));
    }

    pub(crate) fn private_message(&self, message: &crate::signaling::protocol::ChatEntry) {
        if message.account_conversation != Some(true) {
            return;
        }
        if let (Ok(account), Some(Ok(peer))) = (
            message.participant_id.parse(),
            message.recipient_id.as_deref().map(str::parse),
        ) {
            self.pair(account, peer, Scope::Inbox);
        }
    }

    pub(crate) fn revalidate(&self, account: Uuid) {
        let state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(entry) = state.accounts.get(&account) {
            entry.generation.fetch_add(1, Ordering::SeqCst);
            let _ = entry.sender.send(Notice::Revalidate);
        }
    }
}

struct Lease {
    inner: Arc<Inner>,
    account: Uuid,
}

impl Drop for Lease {
    fn drop(&mut self) {
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        state.total -= 1;
        if let Some(entry) = state.accounts.get_mut(&self.account) {
            entry.subscribers -= 1;
            if entry.subscribers == 0 {
                state.accounts.remove(&self.account);
            }
        }
    }
}

struct Subscription {
    receiver: broadcast::Receiver<Notice>,
    generation: Arc<AtomicU64>,
    _lease: Lease,
}

struct Connection {
    subscription: Subscription,
    claims: Claims,
    pool: sqlx::PgPool,
    drain: DrainSignal,
    next_validation: Instant,
    deadline: Instant,
    validated_generation: u64,
    ready: bool,
}

impl Connection {
    async fn valid(&mut self) -> bool {
        if self.drain.is_draining()
            || Instant::now() >= self.deadline
            || account::ensure_unexpired_claim(&self.claims).is_err()
        {
            return false;
        }
        let generation = self.subscription.generation.load(Ordering::SeqCst);
        if Instant::now() >= self.next_validation || generation != self.validated_generation {
            if !matches!(
                tokio::time::timeout_at(
                    self.deadline.min(Instant::now() + REVALIDATE),
                    jwt::validate_current_claims(&self.pool, &self.claims)
                )
                .await,
                Ok(Ok(()))
            ) {
                return false;
            }
            self.next_validation = Instant::now() + REVALIDATE;
            self.validated_generation = generation;
        }
        account::ensure_unexpired_claim(&self.claims).is_ok()
            && !self.drain.is_draining()
            && Instant::now() < self.deadline
            && self.subscription.generation.load(Ordering::SeqCst) == self.validated_generation
    }

    async fn next(&mut self) -> Option<Update> {
        loop {
            if !self.valid().await {
                return None;
            }
            if self.ready {
                self.ready = false;
                return Some(Update::Ready);
            }
            let remaining = i64::try_from(self.claims.exp)
                .ok()
                .and_then(|seconds| chrono::DateTime::from_timestamp(seconds, 0))
                .and_then(|expiry| (expiry - chrono::Utc::now()).to_std().ok())
                .unwrap_or_default();
            let expiry = tokio::time::sleep(remaining);
            tokio::select! {
                biased;
                _ = self.drain.wait() => return None,
                _ = tokio::time::sleep_until(self.deadline) => return None,
                _ = expiry => return None,
                _ = tokio::time::sleep_until(self.next_validation) => {},
                notice = self.subscription.receiver.recv() => match notice {
                    Ok(Notice::Change(update)) => {
                        if self.valid().await { return Some(update); }
                        return None;
                    },
                    Ok(Notice::Revalidate) => self.next_validation = Instant::now(),
                    Err(_) => return None, // Lag requires a new ready + HTTP resync.
                },
            }
        }
    }
}

/// Authentication is bounded before headers. The response body owns only its
/// dedicated stream lease; it cannot occupy an ordinary auth request permit.
pub(crate) async fn stream(
    State(server): State<SignalingServer>,
    headers: HeaderMap,
) -> Result<
    (
        HeaderMap,
        Sse<impl Stream<Item = Result<Event, Infallible>>>,
    ),
    AuthError,
> {
    let _admission = super::routes::acquire_account_request(&server)?;
    let claims = tokio::time::timeout(REVALIDATE, account::authenticated_claims(&server, &headers))
        .await
        .map_err(|_| AuthError::ServiceBusy)??;
    let account = claims.sub.parse().map_err(|_| AuthError::InvalidToken)?;
    let rooms = server.room_manager();
    let subscription = rooms.account_updates().subscribe(account)?;
    let connection = Connection {
        subscription,
        claims,
        pool: server.db_pool().ok_or(AuthError::NotConfigured)?.clone(),
        drain: rooms.drain_signal(),
        next_validation: Instant::now(),
        deadline: Instant::now() + LIFETIME,
        validated_generation: 0,
        ready: true,
    };
    let events = stream::unfold(connection, |mut connection| async move {
        let update = connection.next().await?;
        let name = if matches!(update, Update::Ready) {
            "ready"
        } else {
            "change"
        };
        let event = Event::default().event(name).json_data(update).ok()?;
        Some((Ok(event), connection))
    });
    let mut headers = super::routes::no_store_headers();
    headers.insert("x-accel-buffering", HeaderValue::from_static("no"));
    Ok((
        headers,
        Sse::new(events).keep_alive(
            KeepAlive::new()
                .interval(Duration::from_secs(15))
                .text("keep-alive"),
        ),
    ))
}

#[cfg(test)]
#[path = "updates_tests.rs"]
mod tests;
