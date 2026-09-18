//! Ordered room control without retaining the chat/media state lock over SQL.
//!
//! Lock order is creation (when needed), control, then room state. Membership
//! commits also take control, preserving actor/sender/cohort authorization across
//! persistence. Admission is cancellable; an admitted write owns its task until
//! runtime publication or quarantine. Native cleanup may follow outside control.

use super::{Room, RoomManager, ServerMessage, TokioRwLock, try_send_essential};
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::OwnedMutexGuard;
use tracing::{Instrument, warn};

const ADMISSION_TIMEOUT: Duration = Duration::from_secs(5);
pub(super) const PERSISTENCE_TIMEOUT: Duration = Duration::from_secs(15);
const QUARANTINE_CLEANUP_TIMEOUT: Duration = Duration::from_secs(5);

/// Obtain control without retaining the state lock while waiting. Lifecycle and
/// membership callers must not abandon cleanup merely because a writer is busy.
pub(super) async fn lock_room(room: &Arc<TokioRwLock<Room>>) -> OwnedMutexGuard<()> {
    let control = room.read().await.control.clone();
    control.lock_owned().await
}

/// A missing backing row is also unsafe: the runtime may describe a generation
/// that no longer exists. Do not keep serving its cached authorization policy.
pub(super) fn persistence_is_indeterminate(error: &sqlx::Error) -> bool {
    match error {
        sqlx::Error::Database(error) => !postgres_rejection_is_definite(error.code().as_deref()),
        sqlx::Error::PoolTimedOut | sqlx::Error::PoolClosed | sqlx::Error::InvalidArgument(_) => {
            false
        }
        _ => true,
    }
}

/// PostgreSQL also sends SQLSTATEs for connection failure and unknown statement
/// completion. Receiving a database error does not by itself prove rollback.
/// See [PostgreSQL SQLSTATE codes](https://www.postgresql.org/docs/current/errcodes-appendix.html).
pub(super) fn postgres_rejection_is_definite(code: Option<&str>) -> bool {
    let Some(code) = code else {
        return false;
    };
    code.len() == 5
        && (code.starts_with("22") // Invalid data.
        || code.starts_with("23") // Integrity constraint rejected the write.
        || code.starts_with("42") // Syntax/access-rule rejection.
        || matches!(code, "40000" | "40001" | "40002" | "40P01" | "55P03" | "57014"))
}

impl RoomManager {
    /// Admit one owned control operation against the exact runtime incarnation.
    /// Request cancellation while queued does no work; cancellation after spawn
    /// cannot strand a successful SQL write before its runtime publication.
    pub(super) async fn run_room_control<T, F, W>(
        &self,
        room_id: &str,
        action: F,
    ) -> anyhow::Result<T>
    where
        T: Send + 'static,
        F: FnOnce(Self, String, OwnedMutexGuard<()>) -> W + Send + 'static,
        W: Future<Output = anyhow::Result<T>> + Send + 'static,
    {
        let room = self.get_room(room_id)?;
        let control = tokio::time::timeout(ADMISSION_TIMEOUT, lock_room(&room))
            .await
            .map_err(|_| anyhow::anyhow!("Room control is busy; try again"))?;
        {
            let state = room.read().await;
            state.ensure_live()?;
            anyhow::ensure!(!self.drain.is_draining(), "Server shutting down");
            anyhow::ensure!(
                self.rooms
                    .read()
                    .unwrap_or_else(|error| error.into_inner())
                    .get(room_id)
                    .is_some_and(|current| Arc::ptr_eq(current, &room)),
                "Room is no longer active"
            );
        }
        let manager = self.clone();
        let room_id = room_id.to_owned();
        // Preserve log correlation explicitly. Task-local diagnostic timers do
        // not inherit a request scope that may end before this owned work does.
        tokio::spawn(action(manager, room_id, control).in_current_span())
            .await
            .map_err(|error| {
                warn!(%error, "Owned room control task failed");
                anyhow::anyhow!("Room control operation failed")
            })?
    }

    /// Bound the complete SQL phase, including acquisition and COMMIT. Callers
    /// hold control, not room state, and publish success before releasing control.
    /// Multi-statement writes must supply one transaction as this future.
    pub(super) async fn persist_room<T>(
        &self,
        room_id: &str,
        room: &Arc<TokioRwLock<Room>>,
        persistence: impl Future<Output = Result<T, sqlx::Error>>,
    ) -> Result<T, sqlx::Error> {
        self.persist_room_until(
            room_id,
            room,
            tokio::time::Instant::now() + PERSISTENCE_TIMEOUT,
            persistence,
        )
        .await
    }

    /// Absolute-deadline boundary kept separate for deterministic timer tests.
    pub(super) async fn persist_room_until<T>(
        &self,
        room_id: &str,
        room: &Arc<TokioRwLock<Room>>,
        deadline: tokio::time::Instant,
        persistence: impl Future<Output = Result<T, sqlx::Error>>,
    ) -> Result<T, sqlx::Error> {
        let result = tokio::time::timeout_at(deadline, persistence)
            .await
            .unwrap_or_else(|_| {
                Err(sqlx::Error::Protocol(
                    "Room persistence deadline exceeded; commit status is unknown".into(),
                ))
            });
        if let Err(error) = &result
            && persistence_is_indeterminate(error)
        {
            self.quarantine_room(room_id, room).await;
        }
        result
    }

    /// Never revive uncertain policy or reuse its room ID automatically. The
    /// database row is left untouched; restart reloads its durable state. State
    /// publication is mandatory, not dropped by an end-to-end request timeout.
    async fn quarantine_room(&self, room_id: &str, room_lock: &Arc<TokioRwLock<Room>>) {
        let sessions = {
            let mut room = room_lock.write().await;
            let mapped = self
                .rooms
                .read()
                .unwrap_or_else(|error| error.into_inner())
                .get(room_id)
                .is_some_and(|current| Arc::ptr_eq(current, room_lock));
            if !mapped || room.deleting {
                return;
            }
            room.deleting = true;
            room.policy_revision = room.policy_revision.wrapping_add(1);
            self.deleting_rooms
                .write()
                .unwrap_or_else(|error| error.into_inner())
                .entry(room_id.to_owned())
                .or_insert_with(uuid::Uuid::new_v4);
            let notice = ServerMessage::RoomClosed {
                reason: "Room unavailable while its saved state is uncertain".into(),
            };
            room.broadcast_all(&notice);
            if let Ok(notice) = serde_json::to_string(&notice) {
                let notice = Arc::new(notice);
                for entry in room.lobby.values() {
                    let _ = try_send_essential(&room.metrics, &entry.sender, notice.clone());
                }
            }
            let sessions: Vec<_> = room
                .participants
                .values()
                .map(|participant| {
                    Self::media_participant_id(
                        room_id,
                        &participant.id,
                        participant.media_session_id,
                    )
                })
                .collect();
            room.participants.clear();
            room.lobby.clear();
            room.producer_to_participant.clear();
            room.active_speaker_observer = None;
            room.audio_level_observer = None;
            sessions
        };
        warn!(
            room_id,
            "Room persistence outcome uncertain; retaining runtime tombstone and room-ID reservation until restart"
        );
        let media = self.media_server.clone();
        let room_id = room_id.to_owned();
        tokio::spawn(
            async move {
                use futures_util::StreamExt;
                let cleanup = futures_util::stream::iter(sessions).for_each_concurrent(16, |id| {
                    let media = media.clone();
                    async move {
                        if let Err(error) = media.transport_manager().remove_participant(&id).await
                        {
                            warn!(%error, "Quarantined room participant media cleanup failed");
                        }
                    }
                });
                if tokio::time::timeout(QUARANTINE_CLEANUP_TIMEOUT, cleanup)
                    .await
                    .is_err()
                {
                    warn!(room_id, "Quarantined room participant cleanup incomplete");
                }
                match tokio::time::timeout(
                    QUARANTINE_CLEANUP_TIMEOUT,
                    media.remove_router(&room_id),
                )
                .await
                {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => {
                        warn!(room_id, %error, "Quarantined room router cleanup failed")
                    }
                    Err(_) => warn!(room_id, "Quarantined room router cleanup incomplete"),
                }
                // Intentionally retain the reservation even after successful cleanup.
            }
            .in_current_span(),
        );
    }
}
