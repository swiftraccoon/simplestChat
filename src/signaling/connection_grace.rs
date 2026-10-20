//! Reconnect grace: a room member whose socket closed is retained for thirty
//! seconds under its reconnect token, revalidated while it waits when it is
//! an account, and removed by its own timer when nothing reconnected.

use super::*;
use crate::shutdown::DrainSignal;
use crate::sync::RwLock;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::broadcast;

/// Grace period entry for a disconnected participant
pub(super) struct GraceEntry {
    pub(super) reconnect_token: String,
    pub(super) authenticated_subject: Option<String>,
    pub(super) authenticated_version: Option<i64>,
    /// Media limiter state for this exact room-session incarnation.
    pub(super) media_rate_state: MediaSessionRateState,
    /// Sender channel that owned the participant session when it disconnected.
    /// `same_channel` is the immutable connection-incarnation check.
    pub(super) sender: mpsc::Sender<crate::OutboundJson>,
    pub(super) timer: tokio::task::JoinHandle<()>,
}

type GraceEntries = HashMap<(RoomId, ParticipantId, String), GraceEntry>;

/// Shared map of participants in grace period (disconnected but not yet removed)
#[derive(Clone)]
pub(crate) struct GracePeriodMap {
    pub(super) inner: Arc<RwLock<GraceEntries>>,
    max_entries: usize,
    closed: Arc<AtomicBool>,
}

impl Default for GracePeriodMap {
    fn default() -> Self {
        Self::new()
    }
}

impl GracePeriodMap {
    pub(crate) fn new() -> Self {
        Self::with_capacity(10_000)
    }

    pub(crate) fn with_capacity(max_entries: usize) -> Self {
        Self {
            inner: Arc::new(RwLock::new(HashMap::new())),
            max_entries: max_entries.max(1),
            closed: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Stop only this map's owned timers. Global room drain removes retained
    /// memberships; closing also rejects timers created concurrently afterwards.
    pub(crate) fn close(&self) -> usize {
        let mut map = self.inner.write();
        self.closed.store(true, Ordering::Release);
        let count = map.len();
        for (_, entry) in map.drain() {
            entry.timer.abort();
        }
        count
    }

    pub(crate) fn take_revoked(
        &self,
        user_id: &str,
        minimum_version: i64,
    ) -> Vec<(RoomId, ParticipantId, mpsc::Sender<crate::OutboundJson>)> {
        let mut map = self.inner.write();
        let mut removed = Vec::new();
        map.retain(|(room_id, participant_id, _), entry| {
            if entry.authenticated_subject.as_deref() == Some(user_id)
                && entry
                    .authenticated_version
                    .is_none_or(|version| version < minimum_version)
            {
                entry.timer.abort();
                removed.push((room_id.clone(), *participant_id, entry.sender.clone()));
                false
            } else {
                true
            }
        });
        removed
    }

    /// Retain a disconnected session without evicting another reconnectable
    /// participant. On capacity exhaustion the new timer is aborted and the
    /// caller must immediately remove that participant's room/media state.
    pub(super) fn insert(
        &self,
        room_id: RoomId,
        participant_id: ParticipantId,
        entry: GraceEntry,
    ) -> bool {
        let mut map = self.inner.write();
        let key = (room_id, participant_id, entry.reconnect_token.clone());
        if self.closed.load(Ordering::Acquire)
            || (map.len() >= self.max_entries && !map.contains_key(&key))
        {
            entry.timer.abort();
            return false;
        }
        // Separate connection incarnations for the same stable UUID may briefly
        // coexist after kick/rejoin. Key by the per-session reconnect token so
        // a stale disconnect cannot abort a replacement session's grace timer.
        if let Some(old) = map.remove(&key) {
            old.timer.abort();
        }
        map.insert(key, entry);
        true
    }

    /// A retained timer may start only after its matching map entry exists.
    pub(super) fn insert_activated(
        &self,
        room_id: RoomId,
        participant_id: ParticipantId,
        entry: GraceEntry,
        activation: tokio::sync::oneshot::Sender<()>,
    ) -> bool {
        let retained = self.insert(room_id, participant_id, entry);
        if retained {
            let _ = activation.send(());
        }
        retained
    }

    pub(super) fn remove_if_token_matches(
        &self,
        room_id: &RoomId,
        participant_id: &ParticipantId,
        reconnect_token: &str,
        authenticated_subject: Option<&str>,
    ) -> Option<GraceEntry> {
        let mut map = self.inner.write();
        let key = (
            room_id.clone(),
            *participant_id,
            reconnect_token.to_string(),
        );
        if map
            .get(&key)
            .is_some_and(|entry| entry.authenticated_subject.as_deref() == authenticated_subject)
        {
            map.remove(&key)
        } else {
            None
        }
    }
}

/// What a disconnected room member hands to its reconnect grace.
pub(super) struct DisconnectedMember {
    pub(super) room_id: RoomId,
    pub(super) participant_id: ParticipantId,
    pub(super) reconnect_token: String,
    pub(super) media_rate_state: MediaSessionRateState,
    pub(super) sender: mpsc::Sender<crate::OutboundJson>,
    pub(super) disconnected_at: Instant,
    /// Owned copies: the session's check in flight cannot be shared across an await.
    pub(super) claims: Option<Claims>,
    pub(super) continuity: CredentialContinuity,
    pub(super) credential_deadline: Option<Instant>,
}

/// Retain a disconnected member for a reconnect within the grace period, or
/// remove it at once when the map is full. The owned timer keeps checking an
/// account's credentials while it waits and removes the member when the
/// period ends with this exact grace entry still in place.
pub(super) async fn retain_for_reconnect(
    member: DisconnectedMember,
    grace_periods: &GracePeriodMap,
    room_manager: &Arc<RoomManager>,
    db_pool: Option<sqlx::PgPool>,
    mut auth_revocations: broadcast::Receiver<(String, i64)>,
    drain: &DrainSignal,
) {
    let DisconnectedMember {
        room_id,
        participant_id,
        reconnect_token,
        media_rate_state,
        sender,
        disconnected_at,
        claims,
        continuity,
        credential_deadline,
    } = member;
    let grace_map = grace_periods.clone();
    let rm = room_manager.clone();
    let pid = participant_id;
    let rid = room_id.clone();
    let timer_token = reconnect_token.clone();
    let authenticated_subject = claims.as_ref().map(|claims| claims.sub.clone());
    let authenticated_version = claims.as_ref().map(|claims| claims.auth_version);
    let timer_subject = authenticated_subject.clone();
    let timer_sender = sender.clone();
    let timer_claims = claims;
    let timer_pool = db_pool;
    let mut timer_continuity = continuity;
    let timer_drain = drain.clone();
    let grace_deadline = disconnected_at + Duration::from_secs(30);
    let grace_deadline =
        credential_deadline.map_or(grace_deadline, |expiry| expiry.min(grace_deadline));
    let (activate_timer, activation) = tokio::sync::oneshot::channel::<()>();

    let timer = tokio::spawn(async move {
        // Registration must precede even an already-expired timer's
        // cleanup. Failed insertion drops activation and aborts the task.
        if activation.await.is_err() {
            return;
        }
        let mut next_check = Instant::now() + AUTH_REVALIDATE_INTERVAL;
        loop {
            let deadline = timer_continuity
                .deadline()
                .map_or(grace_deadline, |limit| limit.min(grace_deadline));
            tokio::select! {
                biased;
                _ = timer_drain.wait() => return,
                _ = tokio::time::sleep_until(deadline.into()) => break,
                _ = tokio::time::sleep_until(next_check.into()), if timer_claims.is_some() => {
                    if let Some(claims) = timer_claims.as_ref() {
                        match revalidate_account(timer_pool.as_ref(), claims, &mut timer_continuity,
                            &mut auth_revocations, &timer_drain, grace_deadline).await {
                            Some(true) => {},
                            Some(false) => break,
                            None => return,
                        }
                    }
                    next_check = Instant::now() + AUTH_REVALIDATE_INTERVAL;
                },
                notice = auth_revocations.recv(), if timer_claims.is_some() => {
                    match notice {
                        Ok((subject, version)) if timer_claims.as_ref().is_some_and(|claims|
                            claims.sub == subject && claims.auth_version < version) => break,
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                            timer_continuity.require_revalidation();
                            next_check = Instant::now();
                        },
                        _ => {},
                    }
                }
            }
        }
        // Only the timer that still owns this exact grace entry may
        // remove the participant. A reconnect or replacement timer can
        // win the race at the deadline without being torn down by an
        // older task.
        if grace_map
            .remove_if_token_matches(&rid, &pid, &timer_token, timer_subject.as_deref())
            .is_some()
        {
            info!(
                "Grace period expired for participant {} in room {}",
                pid, rid
            );
            if tracing::enabled!(target: "simplestchat::lifecycle", tracing::Level::DEBUG) {
                let lifecycle_id = pid.as_uuid();
                debug!(
                    target: "simplestchat::lifecycle",
                    event = "grace_cleanup_started",
                    participant_id = %lifecycle_id,
                    "lifecycle"
                );
            }
            if let Err(e) = rm
                .remove_participant_for_sender(&rid, &pid, &timer_sender)
                .await
            {
                error!("Error removing participant after grace period: {}", e);
            }
        }
    });

    let retained_for_reconnect = grace_periods.insert_activated(
        room_id.clone(),
        participant_id,
        GraceEntry {
            reconnect_token,
            authenticated_subject,
            authenticated_version,
            media_rate_state,
            sender: sender.clone(),
            timer,
        },
        activate_timer,
    );
    if retained_for_reconnect
        && tracing::enabled!(target: "simplestchat::lifecycle", tracing::Level::DEBUG)
    {
        let lifecycle_id = participant_id.as_uuid();
        debug!(
            target: "simplestchat::lifecycle",
            event = "grace_started",
            participant_id = %lifecycle_id,
            "lifecycle"
        );
    }
    if !retained_for_reconnect {
        warn!(
            %room_id,
            %participant_id,
            "Grace-session capacity reached; cleaning up disconnected participant immediately"
        );
        if let Err(error) = room_manager
            .remove_participant_for_sender(&room_id, &participant_id, &sender)
            .await
        {
            debug!(
                %room_id,
                %participant_id,
                %error,
                "Immediate grace-cap cleanup found no current participant session"
            );
        }
    }
}

/// Resume a retained session on a new socket when its reconnect token matches.
pub(super) async fn handle_reconnect(
    participant_id: &ParticipantId,
    room_id: &RoomId,
    client_token: &str,
    authenticated_subject: Option<&str>,
    grace_periods: &GracePeriodMap,
    room_manager: &Arc<RoomManager>,
    new_sender: &mpsc::Sender<crate::OutboundJson>,
) -> Option<MediaSessionRateState> {
    if client_token.parse::<Uuid>().is_err() {
        return None;
    }
    // Check if participant is in grace period
    if let Some(entry) = grace_periods.remove_if_token_matches(
        room_id,
        participant_id,
        client_token,
        authenticated_subject,
    ) {
        // Cancel the cleanup timer
        entry.timer.abort();

        // Rebind the sender in the room's participant list
        match room_manager
            .rebind_participant_sender(
                room_id,
                participant_id,
                authenticated_subject,
                &entry.sender,
                new_sender.clone(),
            )
            .await
        {
            Ok(true) => {
                info!(
                    "Participant {} reconnected to room {}",
                    participant_id, room_id
                );
                Some(entry.media_rate_state)
            }
            Ok(false) => {
                warn!(
                    "Participant {} not found in room {} during reconnect",
                    participant_id, room_id
                );
                let _ = room_manager
                    .remove_participant_for_sender(room_id, participant_id, &entry.sender)
                    .await;
                None
            }
            Err(e) => {
                error!("Error during reconnect for {}: {}", participant_id, e);
                let _ = room_manager
                    .remove_participant_for_sender(room_id, participant_id, &entry.sender)
                    .await;
                None
            }
        }
    } else {
        debug!("No grace period found for participant {}", participant_id);
        None
    }
}
