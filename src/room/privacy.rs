//! Publish current account privacy into active memberships after an owned save.
//!
//! Publication reads durable state only after acquiring each room's control
//! gate, so a delayed older HTTP completion cannot overwrite a newer policy.
#![forbid(unsafe_code)]

use super::{Room, RoomManager, TokioRwLock, control};
use crate::auth::account::{ChatPreferences, load_chat_preferences};
use crate::ids::ParticipantId;
use futures_util::{StreamExt, stream};
use sqlx::PgPool;
use std::{future::Future, sync::Arc, time::Duration};
use uuid::Uuid;

const SYNC_TIMEOUT: Duration = Duration::from_secs(15);
const SYNC_CONCURRENCY: usize = 8;

async fn refresh_live_room<F, W, C>(
    room: &Arc<TokioRwLock<Room>>,
    account: Uuid,
    current: C,
    read: F,
) -> anyhow::Result<()>
where
    F: FnOnce() -> W,
    W: Future<Output = Result<ChatPreferences, sqlx::Error>>,
    C: Fn() -> bool,
{
    let _control = tokio::time::timeout(control::ADMISSION_TIMEOUT, control::lock_room(room))
        .await
        .map_err(|_| anyhow::anyhow!("Privacy synchronization is waiting on a busy room"))?;
    let account_id = ParticipantId::from(account);
    {
        let state = room.read().await;
        if !current() {
            return Ok(());
        }
        state.ensure_live()?;
        if !state
            .participants
            .values()
            .any(|person| person.authenticated && person.id == account_id)
        {
            return Ok(());
        }
    }
    // Never accept the initiating request's snapshot here. Concurrent account
    // writes commit independently of a room; the room gate orders fresh reads.
    let preferences = tokio::time::timeout(control::PERSISTENCE_TIMEOUT, read())
        .await
        .map_err(|_| anyhow::anyhow!("Current account privacy could not be read"))??;
    let mut state = room.write().await;
    if !current() {
        return Ok(());
    }
    state.ensure_live()?;
    for person in state.participants.values_mut() {
        if person.authenticated && person.id == account_id {
            person.social.apply_account_preferences(&preferences);
        }
    }
    Ok(())
}

impl RoomManager {
    /// Caller holds the destination room's control gate, without its state lock.
    /// The room is already globally registered before this read starts, so a
    /// concurrent save either publishes after insertion or this read sees it.
    pub(super) async fn membership_chat_preferences(
        &self,
        authenticated: bool,
        participant_id: &ParticipantId,
    ) -> anyhow::Result<Option<ChatPreferences>> {
        if !authenticated {
            return Ok(None);
        }
        let account = participant_id.as_uuid();
        let pool = self
            .db_pool
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Account privacy could not be verified"))?;
        let preferences = tokio::time::timeout(
            control::PERSISTENCE_TIMEOUT,
            load_chat_preferences(pool, account),
        )
        .await
        .map_err(|_| anyhow::anyhow!("Account privacy could not be verified"))?
        .map_err(|_| anyhow::anyhow!("Account privacy could not be verified"))?;
        Ok(Some(preferences))
    }

    /// Called by the owned preference-save task after COMMIT. Unreadable or
    /// timed-out rooms retain their existing policy and make the save response
    /// explicitly report incomplete synchronization rather than claim success.
    pub(crate) async fn refresh_account_preferences(&self, account: Uuid) -> anyhow::Result<()> {
        let pool: PgPool = self
            .db_pool
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Account privacy requires a database"))?
            .clone();
        let account_id = ParticipantId::from(account);
        let rooms: Vec<_> = self
            .rooms
            .read()
            .iter()
            .filter(|(_, room)| {
                room.try_read().map_or(true, |state| {
                    state
                        .participants
                        .values()
                        .any(|person| person.authenticated && person.id == account_id)
                })
            })
            .map(|(id, room)| (id.clone(), room.clone()))
            .collect();
        let synchronize = async {
            let mut work = stream::iter(rooms)
                .map(|(id, room)| {
                    let pool = pool.clone();
                    async move {
                        refresh_live_room(
                            &room,
                            account,
                            || {
                                self.rooms
                                    .read()
                                    .get(&id)
                                    .is_some_and(|live| Arc::ptr_eq(live, &room))
                            },
                            || load_chat_preferences(&pool, account),
                        )
                        .await
                    }
                })
                .buffer_unordered(SYNC_CONCURRENCY);
            let mut failed = 0;
            while let Some(result) = work.next().await {
                if result.is_err() {
                    failed += 1;
                }
            }
            anyhow::ensure!(
                failed == 0,
                "Account privacy did not synchronize to {failed} rooms"
            );
            Ok(())
        };
        let result = tokio::time::timeout(SYNC_TIMEOUT, synchronize)
            .await
            .map_err(|_| anyhow::anyhow!("Account privacy synchronization deadline exceeded"))?;
        if result.is_err() {
            tracing::warn!("Saved account privacy could not be synchronized to every active room");
        }
        result
    }
}

#[cfg(test)]
#[path = "privacy_tests.rs"]
mod tests;
