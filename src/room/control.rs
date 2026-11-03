//! Ordered room control without retaining the chat/media state lock over SQL.
//!
//! Lock order is creation (when needed), control, then room state. Membership
//! commits also take control, preserving actor/sender/cohort authorization across
//! persistence. Admission is cancellable; an admitted write owns its task until
//! runtime publication or quarantine. Native cleanup may follow outside control.

use super::{Room, RoomManager, ServerMessage, TokioRwLock, shutting_down, try_send_essential};
use crate::ids::RoomId;
use crate::room::refusal::{Refusal, RoomError};
use crate::sync::Mutex;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::OwnedMutexGuard;
use tracing::{Instrument, warn};

pub(super) const ADMISSION_TIMEOUT: Duration = Duration::from_secs(5);

/// Proof that an owned operation holds its room's control lock.
///
/// The operation's task keeps control for its whole run whatever the operation
/// does with this token: dropping it, or never touching it, cannot release
/// control early. An operation that has published its result and wants later
/// work (native cleanup, notifications) to proceed without excluding other
/// writers calls [`RoomControl::release`] explicitly.
#[derive(Clone)]
pub(crate) struct RoomControl(Arc<Mutex<Option<OwnedMutexGuard<()>>>>);

impl RoomControl {
    /// Wrap control a caller acquired itself through [`lock_room`].
    pub(crate) fn from_guard(guard: OwnedMutexGuard<()>) -> Self {
        Self(Arc::new(Mutex::new(Some(guard))))
    }

    /// End the exclusion now, before the owned task finishes.
    pub(crate) fn release(&self) {
        drop(self.0.lock().take());
    }
}
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
pub(super) fn persistence_is_indeterminate(error: &RoomError) -> bool {
    match error {
        RoomError::Refused(_) | RoomError::PasswordRequired => false,
        RoomError::Database(sqlx::Error::Database(error)) => {
            !postgres_rejection_is_definite(error.code().as_deref())
        }
        RoomError::Database(
            sqlx::Error::PoolTimedOut | sqlx::Error::PoolClosed | sqlx::Error::InvalidArgument(_),
        ) => false,
        RoomError::Database(_) => true,
        // Persistence futures fail only with refusals and database errors; any
        // other failure inside one leaves its commit unproven, so fail closed.
        RoomError::Media(_) | RoomError::Internal(_) => true,
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

/// A change whose save its requester waits for. A room-state save runs
/// through [`RoomManager::persist_room`], which quarantines its room when the
/// commit is unproven; the account's saves (`ChatLook`, `ReadMarker`) keep the
/// account's own state and quarantine no room.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Save {
    /// A mute, a camera ban or the removal of either.
    Sanction,
    /// A ban; it may resolve a report.
    Ban,
    /// The removal of an account's saved ban.
    Unban,
    /// A saved role.
    Role,
    /// The room settings.
    Settings,
    /// The room topic.
    Topic,
    /// The history entry a kick writes first; it may resolve a report.
    Kick,
    /// The account's chat look.
    ChatLook,
    /// The account's read marker in a room's saved history.
    ReadMarker,
}

impl Save {
    /// What the requester hears when this save definitely did not apply, and
    /// when it may have committed.
    fn texts(self) -> (&'static str, &'static str) {
        match self {
            Self::Sanction => (
                "Moderation state could not be saved; try again",
                "Moderation state may not have been saved",
            ),
            Self::Ban => (
                "Ban could not be saved; try again",
                "Ban may not have been saved",
            ),
            Self::Unban => (
                "Ban removal could not be saved; try again",
                "Ban removal may not have been saved",
            ),
            Self::Role => (
                "Role could not be saved; try again",
                "Role may not have been saved",
            ),
            Self::Settings => (
                "Room settings could not be saved; try again",
                "Room settings may not have been saved",
            ),
            Self::Topic => (
                "Room topic could not be saved; try again",
                "Room topic may not have been saved",
            ),
            Self::Kick => (
                "Moderation history could not be saved; try again",
                "Moderation history may not have been saved",
            ),
            Self::ChatLook => (
                "Your chat look could not be saved; try again",
                "Your chat look may not have been saved",
            ),
            Self::ReadMarker => (
                "Read marker could not be saved; try again",
                "Read marker may not have been saved",
            ),
        }
    }

    /// What the requester is told when this save failed. A refusal stands as
    /// it is (a report from another room among them). A failure that leaves
    /// the commit unproven ([`persistence_is_indeterminate`]) answers
    /// `timed_out`, because the change may have applied; a room-state save's
    /// room has then been quarantined, an account save's has not. A definite
    /// database failure answers `unavailable` and any other definite failure
    /// `request_failed`. Every failure but a refusal is logged with the room's
    /// log handle and its detail.
    pub(super) fn failure(self, room_id: &RoomId, error: RoomError) -> RoomError {
        match error {
            refused @ RoomError::Refused(_) => refused,
            error => {
                warn!(room = %room_id.log_handle(), save = ?self, %error, "Save failed");
                let (failed, uncertain) = self.texts();
                if persistence_is_indeterminate(&error) {
                    Refusal::TimedOut(uncertain.into()).into()
                } else if matches!(error, RoomError::Database(_)) {
                    Refusal::Unavailable(failed.into()).into()
                } else {
                    Refusal::Internal(failed.into()).into()
                }
            }
        }
    }
}

impl RoomManager {
    /// Admit one owned control operation against the exact runtime incarnation.
    /// Request cancellation while queued does no work; cancellation after spawn
    /// cannot strand a successful SQL write before its runtime publication.
    pub(super) async fn run_room_control<T, F, W>(
        &self,
        room_id: &RoomId,
        action: F,
    ) -> Result<T, RoomError>
    where
        T: Send + 'static,
        F: FnOnce(Self, RoomId, RoomControl) -> W + Send + 'static,
        W: Future<Output = Result<T, RoomError>> + Send + 'static,
    {
        let room = self.get_room(room_id)?;
        let control = tokio::time::timeout(ADMISSION_TIMEOUT, lock_room(&room))
            .await
            .map_err(|_| Refusal::Unavailable("Room control is busy; try again".into()))?;
        {
            let state = room.read().await;
            state.ensure_live()?;
            if self.drain.is_draining() {
                return Err(shutting_down());
            }
            if !self
                .rooms
                .read()
                .get(room_id)
                .is_some_and(|current| Arc::ptr_eq(current, &room))
            {
                return Err(Refusal::NotFound("Room is no longer active".into()).into());
            }
        }
        let manager = self.clone();
        let room_id = room_id.clone();
        // The task, not the operation, owns control: a closure that ignores its
        // token (or drops it on an early error path) still excludes other
        // writers until the owned work has finished.
        let control = RoomControl::from_guard(control);
        let held = control.clone();
        let operation = action(manager, room_id, control);
        // Preserve log correlation explicitly. Task-local diagnostic timers do
        // not inherit a request scope that may end before this owned work does.
        tokio::spawn(
            async move {
                let _held = held;
                operation.await
            }
            .in_current_span(),
        )
        .await
        .map_err(|error| {
            warn!(%error, "Owned room control task failed");
            RoomError::Internal(anyhow::anyhow!("Room control operation failed"))
        })?
    }

    /// Bound the complete SQL phase, including acquisition and COMMIT. Callers
    /// hold control, not room state, and publish success before releasing control.
    /// Multi-statement writes must supply one transaction as this future.
    pub(super) async fn persist_room<T>(
        &self,
        room_id: &RoomId,
        room: &Arc<TokioRwLock<Room>>,
        persistence: impl Future<Output = Result<T, RoomError>>,
    ) -> Result<T, RoomError> {
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
        room_id: &RoomId,
        room: &Arc<TokioRwLock<Room>>,
        deadline: tokio::time::Instant,
        persistence: impl Future<Output = Result<T, RoomError>>,
    ) -> Result<T, RoomError> {
        let result = tokio::time::timeout_at(deadline, persistence)
            .await
            .unwrap_or_else(|_| {
                Err(sqlx::Error::Protocol(
                    "Room persistence deadline exceeded; commit status is unknown".into(),
                )
                .into())
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
    async fn quarantine_room(&self, room_id: &RoomId, room_lock: &Arc<TokioRwLock<Room>>) {
        let sessions = {
            let mut room = room_lock.write().await;
            let mapped = self
                .rooms
                .read()
                .get(room_id)
                .is_some_and(|current| Arc::ptr_eq(current, room_lock));
            if !mapped || room.deleting {
                return;
            }
            room.deleting = true;
            room.policy_revision = room.policy_revision.wrapping_add(1);
            self.deleting_rooms
                .write()
                .entry(room_id.to_owned())
                .or_insert_with(uuid::Uuid::new_v4);
            let notice = ServerMessage::RoomClosed {
                reason: "Room unavailable while its saved state is uncertain".into(),
            };
            room.broadcast_all(&notice);
            if let Ok(notice) = serde_json::to_string(&notice) {
                let notice = crate::OutboundJson::from(notice);
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
            room = %room_id.log_handle(),
            "Room persistence outcome uncertain; retaining runtime tombstone and room-ID reservation until restart"
        );
        let media = self.media_server.clone();
        let room_id = room_id.clone();
        tokio::spawn(
            async move {
                use futures_util::StreamExt;
                let cleanup = futures_util::stream::iter(sessions).for_each_concurrent(
                    16,
                    |media_participant_id| {
                        let media = media.clone();
                        async move {
                            if let Err(error) = media
                                .transport_manager()
                                .remove_participant(&media_participant_id)
                                .await
                            {
                                warn!(%error, "Quarantined room participant media cleanup failed");
                            }
                        }
                    },
                );
                if tokio::time::timeout(QUARANTINE_CLEANUP_TIMEOUT, cleanup)
                    .await
                    .is_err()
                {
                    warn!(
                        room = %room_id.log_handle(),
                        "Quarantined room participant cleanup incomplete"
                    );
                }
                match tokio::time::timeout(
                    QUARANTINE_CLEANUP_TIMEOUT,
                    media.remove_router(room_id.as_str()),
                )
                .await
                {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => {
                        warn!(
                            room = %room_id.log_handle(),
                            %error,
                            "Quarantined room router cleanup failed"
                        )
                    }
                    Err(_) => warn!(
                        room = %room_id.log_handle(),
                        "Quarantined room router cleanup incomplete"
                    ),
                }
                // Intentionally retain the reservation even after successful cleanup.
            }
            .in_current_span(),
        );
    }
}
