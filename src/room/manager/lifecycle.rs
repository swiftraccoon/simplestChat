//! Persisted room creation and deletion, worker recovery and shutdown.

use super::super::*;

impl RoomManager {
    /// Atomically reserve a database-backed room ID against ad-hoc runtime room
    /// creation. Without sharing the creation lock, an anonymous join could
    /// create an owner-controlled runtime room between the REST endpoint's
    /// existence check and the database insert.
    pub(crate) async fn create_persisted_room(
        &self,
        owner_id: &uuid::Uuid,
        request: &settings::CreateRoomRequest,
        password_hash: Option<&str>,
    ) -> std::result::Result<Option<settings::RoomSettings>, sqlx::Error> {
        if self.drain.is_draining() {
            return Err(sqlx::Error::PoolClosed);
        }
        let _creation_guard =
            tokio::time::timeout(ROOM_CREATION_TIMEOUT, self.room_creation_lock.lock())
                .await
                .map_err(|_| room_creation_timeout("waiting for room creation"))?;
        if self.drain.is_draining() {
            return Err(sqlx::Error::PoolClosed);
        }
        if self.deleting_rooms.read().contains_key(request.id.as_str()) {
            return Ok(None);
        }
        if self.rooms.read().contains_key(request.id.as_str()) {
            return Ok(None);
        }
        let pool = self.db_pool.as_ref().ok_or(sqlx::Error::PoolClosed)?;
        tokio::time::timeout(
            ROOM_CREATION_TIMEOUT,
            settings::create_room(
                pool,
                owner_id,
                request,
                password_hash,
                self.max_persisted_rooms,
            ),
        )
        .await
        .map_err(|_| room_creation_timeout("writing the database row"))?
        .map(Some)
    }

    /// Delete a persisted room without leaving an in-memory zombie. Ownership
    /// is checked before any runtime tombstone is installed, so unauthorized
    /// callers cannot temporarily disrupt a live room. The creation mutex
    /// serializes the reservation, tombstone, and conditional DB delete against
    /// creation; the reservation then protects map eviction and media teardown
    /// after the mutex is released.
    pub(crate) async fn delete_persisted_room(
        &self,
        room_id: &RoomId,
        requester_id: &uuid::Uuid,
    ) -> std::result::Result<DeleteRoomResult, sqlx::Error> {
        let pool = self.db_pool.clone().ok_or(sqlx::Error::PoolClosed)?;
        let room_id = room_id.clone();
        let requester_id = *requester_id;
        let rooms = self.rooms.clone();
        let deleting_rooms = self.deleting_rooms.clone();
        let creation_lock = self.room_creation_lock.clone();
        let media_server = self.media_server.clone();

        // Run the mutation in its own task so cancellation of the HTTP request
        // cannot strand a committed delete halfway through runtime teardown.
        let deletion = tokio::spawn(async move {
            // Capture an immutable row generation before taking the local
            // creation lock. The conditional DELETE below revalidates both
            // ownership and generation, so a delayed task cannot delete a
            // same-ID replacement created after this read.
            let generation = tokio::time::timeout(
                ROOM_DELETE_TIMEOUT,
                sqlx::query_as::<_, (uuid::Uuid, chrono::DateTime<chrono::Utc>)>(
                    "SELECT owner_id, created_at FROM rooms WHERE id = $1",
                )
                .bind(&room_id)
                .fetch_optional(&pool),
            )
            .await
            .map_err(|_| room_delete_timeout("checking ownership"))??;
            let Some((owner_id, created_at)) = generation else {
                return Ok(DeleteRoomResult::NotFound);
            };
            if owner_id != requester_id {
                return Ok(DeleteRoomResult::Forbidden);
            }

            let creation_guard = tokio::time::timeout(ROOM_DELETE_TIMEOUT, creation_lock.lock())
                .await
                .map_err(|_| room_delete_timeout("waiting for room creation"))?;
            let runtime_room = rooms.read().get(&room_id).cloned();
            // Control precedes the deletion reservation: an earlier writer may
            // quarantine this incarnation while we wait for its SQL to finish.
            let control_guard = match runtime_room.as_ref() {
                Some(room) => Some(
                    tokio::time::timeout(ROOM_DELETE_TIMEOUT, control::lock_room(room))
                        .await
                        .map_err(|_| room_delete_timeout("waiting for room control"))?,
                ),
                None => None,
            };
            let deletion_token = uuid::Uuid::new_v4();
            {
                let mut deleting = deleting_rooms.write();
                if deleting.contains_key(&room_id) {
                    return Ok(DeleteRoomResult::NotFound);
                }
                deleting.insert(room_id.clone(), deletion_token);
            }

            if let Some(runtime_room) = runtime_room.as_ref() {
                let mut room = match tokio::time::timeout(ROOM_DELETE_TIMEOUT, runtime_room.write())
                    .await
                {
                    Ok(room) => room,
                    Err(_) => {
                        release_deletion_reservation(&deleting_rooms, &room_id, deletion_token);
                        return Err(room_delete_timeout("waiting to tombstone the runtime room"));
                    }
                };
                room.deleting = true;
                room.policy_revision = room.policy_revision.wrapping_add(1);
            }

            let deleted = tokio::time::timeout(
                ROOM_DELETE_TIMEOUT,
                sqlx::query(
                    "DELETE FROM rooms \
                     WHERE id = $1 AND owner_id = $2 AND created_at = $3",
                )
                .bind(&room_id)
                .bind(requester_id)
                .bind(created_at)
                .execute(&pool),
            )
            .await;
            let deleted = match deleted {
                Ok(Ok(result)) => result.rows_affected() == 1,
                Ok(Err(error)) => {
                    drop(creation_guard);
                    if !control::persistence_is_indeterminate(&error) {
                        // PostgreSQL rejected the statement (or no connection
                        // was acquired), so no commit is possible.
                        restore_failed_deletion(
                            runtime_room,
                            deleting_rooms,
                            room_id,
                            deletion_token,
                        )
                        .await;
                    } else {
                        // I/O/protocol failures can make an autocommit outcome
                        // unknowable. Keep the room reserved and tombstoned
                        // rather than reviving a runtime room whose DB row may
                        // already be gone.
                        warn!(%room_id, %error, "Room delete outcome is indeterminate; retaining tombstone");
                    }
                    return Err(error);
                }
                Err(_) => {
                    drop(creation_guard);
                    // Dropping a timed-out query future cannot prove that
                    // PostgreSQL did not commit. Fail closed until process
                    // reconciliation/restart rather than creating a zombie.
                    warn!(
                        %room_id,
                        "Room delete timed out; retaining tombstone because commit status is unknown"
                    );
                    return Err(room_delete_timeout("deleting the database row"));
                }
            };
            if !deleted {
                drop(creation_guard);
                restore_failed_deletion(runtime_room, deleting_rooms, room_id, deletion_token)
                    .await;
                return Ok(DeleteRoomResult::NotFound);
            }

            // The reservation blocks same-ID creation. Release the global
            // creation mutex before waiting for a busy room or mediasoup IPC.
            drop(creation_guard);

            let participant_sessions = if let Some(runtime_room) = runtime_room.as_ref() {
                let mut room =
                    match tokio::time::timeout(ROOM_DELETE_TIMEOUT, runtime_room.write()).await {
                        Ok(room) => room,
                        Err(_) => {
                            // The database row is already gone. Keep the runtime
                            // tombstoned and the ID reserved rather than leak a
                            // permanently waiting teardown task.
                            warn!(
                                %room_id,
                                "Timed out draining deleted room; retaining room-ID reservation"
                            );
                            return Ok(DeleteRoomResult::Deleted);
                        }
                    };
                room.broadcast_all(&ServerMessage::RoomClosed {
                    reason: "Room was deleted by its owner".to_string(),
                });
                if let Ok(message) = serde_json::to_string(&ServerMessage::RoomClosed {
                    reason: "Room was deleted by its owner".to_string(),
                }) {
                    let message = crate::OutboundJson::from(message);
                    for entry in room.lobby.values() {
                        let _ = try_send_essential(&room.metrics, &entry.sender, message.clone());
                    }
                }
                let participant_sessions: Vec<(ParticipantId, uuid::Uuid)> = room
                    .participants
                    .values()
                    .map(|participant| (participant.id, participant.media_session_id))
                    .collect();
                room.participants.clear();
                room.lobby.clear();
                room.producer_to_participant.clear();
                participant_sessions
            } else {
                Vec::new()
            };

            if let Some(runtime_room) = runtime_room.as_ref() {
                let mut rooms = rooms.write();
                if rooms
                    .get(&room_id)
                    .is_some_and(|candidate| Arc::ptr_eq(candidate, runtime_room))
                {
                    rooms.remove(&room_id);
                }
            }
            drop(control_guard);

            let participant_cleanup = participant_sessions.into_iter().map(
                |(participant_id, media_session_id)| {
                    let media_server = media_server.clone();
                    let room_id = room_id.clone();
                    async move {
                        let media_participant_id = RoomManager::media_participant_id(
                            &room_id,
                            &participant_id,
                            media_session_id,
                        );
                        if let Err(error) = media_server
                            .transport_manager()
                            .remove_participant(&media_participant_id)
                            .await
                        {
                            warn!(%room_id, %participant_id, %error, "Failed to tear down deleted-room participant media");
                        }
                    }
                },
            );
            if tokio::time::timeout(
                ROOM_DELETE_TIMEOUT,
                futures_util::future::join_all(participant_cleanup),
            )
            .await
            .is_err()
            {
                warn!(
                    %room_id,
                    "Timed out tearing down deleted-room participant media"
                );
            }
            let router_teardown_succeeded = if runtime_room.is_some() {
                match tokio::time::timeout(
                    ROOM_DELETE_TIMEOUT,
                    media_server.remove_router(room_id.as_str()),
                )
                .await
                {
                    Ok(Ok(())) => true,
                    Ok(Err(error)) => {
                        // The DB row is gone, but releasing the ID while the old
                        // mediasoup router remains could make a same-ID room
                        // recreation fail or attach to stale infrastructure.
                        // Fail closed until restart/reconciliation can prove the
                        // router is gone.
                        warn!(%room_id, %error, "Failed to tear down deleted-room router; retaining room-ID reservation");
                        false
                    }
                    Err(_) => {
                        warn!(
                            %room_id,
                            "Timed out tearing down deleted-room router; retaining room-ID reservation"
                        );
                        false
                    }
                }
            } else {
                true
            };

            release_deletion_reservation_after_router_teardown(
                &deleting_rooms,
                &room_id,
                deletion_token,
                router_teardown_succeeded,
            );
            Ok(DeleteRoomResult::Deleted)
        });

        deletion
            .await
            .map_err(|error| sqlx::Error::Protocol(format!("room deletion task failed: {error}")))?
    }

    /// Starts the task that turns media worker deaths into recovery: the
    /// worker is recreated so capacity returns, and every room whose router
    /// lived on it is closed with a temporary restart notice so its members
    /// rejoin onto live capacity. The task holds only a weak handle and ends
    /// with the manager.
    pub(crate) fn spawn_worker_recovery(self: &Arc<Self>) {
        let Some(mut deaths) = self.media_server.worker_manager().take_death_events() else {
            return;
        };
        let manager = Arc::downgrade(self);
        tokio::spawn(async move {
            while let Some(worker_id) = deaths.recv().await {
                let Some(manager) = manager.upgrade() else {
                    break;
                };
                manager.recover_from_worker_death(worker_id).await;
            }
        });
    }

    /// Recovery for one dead worker; see [`Self::spawn_worker_recovery`].
    pub(crate) async fn recover_from_worker_death(&self, dead_worker: mediasoup::worker::WorkerId) {
        self.metrics.inc_media_worker_death();
        let rooms = self
            .media_server
            .router_manager()
            .rooms_on_worker(dead_worker)
            .await;
        // Viewers placed on the dead worker lost their receive transports; the
        // server refuses their ICE restarts, the client then rebuilds its
        // transports, and `place_viewer` puts the new one on a live worker.
        // Their rooms stay open.
        let viewer_routers = self
            .media_server
            .router_manager()
            .drop_viewer_routers_on_worker(dead_worker)
            .await;
        warn!(
            %dead_worker,
            affected_rooms = rooms.len(),
            viewer_routers,
            "Media worker died; recreating it and asking its rooms to rejoin"
        );
        // Capacity first: when this was the only worker, rejoins need it back.
        let replacement = self
            .media_server
            .worker_manager()
            .recreate_worker(dead_worker)
            .await;
        self.metrics.inc_media_worker_recovery(replacement.is_ok());
        if let Err(error) = replacement {
            error!(
                %dead_worker,
                %error,
                "Failed to recreate a dead media worker; capacity stays reduced until restart"
            );
        }
        for room_id in rooms {
            let Ok(room_id) = RoomId::new(room_id) else {
                continue;
            };
            if let Err(error) = self
                .close_room_for_rejoin(&room_id, dead_worker, "Media worker restarted; rejoining")
                .await
            {
                warn!(%room_id, %error, "Failed to close a room whose media worker died");
            }
        }
    }

    /// Removes a live room's runtime and tells its members to rejoin. Unlike
    /// deletion this keeps any persisted row. Its room ID stays reserved until
    /// old media teardown finishes, so delayed cleanup cannot remove a rejoin's
    /// replacement router.
    pub(in crate::room) async fn close_room_for_rejoin(
        &self,
        room_id: &RoomId,
        expected_worker: mediasoup::worker::WorkerId,
        reason: &str,
    ) -> Result<()> {
        let Ok(room_lock) = self.get_room(room_id) else {
            return Ok(());
        };
        let creation_guard =
            tokio::time::timeout(ROOM_CREATION_TIMEOUT, self.room_creation_lock.lock())
                .await
                .map_err(|_| anyhow::anyhow!("Room creation is busy"))?;
        if !self
            .rooms
            .read()
            .get(room_id)
            .is_some_and(|current| Arc::ptr_eq(current, &room_lock))
        {
            return Ok(());
        }
        // Worker recreation awaited after the original affected-room snapshot.
        // A room replaced in the meantime belongs to a different worker generation.
        if self
            .media_server
            .router_manager()
            .get_worker_id(room_id.as_str())
            .await
            .ok()
            != Some(expected_worker)
        {
            return Ok(());
        }
        let control_guard =
            tokio::time::timeout(control::ADMISSION_TIMEOUT, control::lock_room(&room_lock))
                .await
                .map_err(|_| anyhow::anyhow!("Room control is busy"))?;
        let participant_sessions = {
            let mut room = tokio::time::timeout(ROOM_DELETE_TIMEOUT, room_lock.write())
                .await
                .map_err(|_| anyhow::anyhow!("Room state is busy"))?;
            let notice = ServerMessage::ServerRestarting {
                reason: reason.to_string(),
            };
            room.broadcast_all(&notice);
            if let Ok(message) = serde_json::to_string(&notice) {
                let message = crate::OutboundJson::from(message);
                for entry in room.lobby.values() {
                    let _ = try_send_essential(&room.metrics, &entry.sender, message.clone());
                }
            }
            let sessions: Vec<(ParticipantId, uuid::Uuid)> = room
                .participants
                .values()
                .map(|participant| (participant.id, participant.media_session_id))
                .collect();
            room.participants.clear();
            room.lobby.clear();
            room.producer_to_participant.clear();
            room.active_speaker_observer = None;
            room.audio_level_observer = None;
            room.deleting = true;
            room.policy_revision = room.policy_revision.wrapping_add(1);
            sessions
        };
        {
            let mut rooms = self.rooms.write();
            if rooms
                .get(room_id)
                .is_some_and(|candidate| Arc::ptr_eq(candidate, &room_lock))
            {
                rooms.remove(room_id);
            }
        }
        let deletion_token = uuid::Uuid::new_v4();
        self.deleting_rooms
            .write()
            .insert(room_id.to_owned(), deletion_token);
        drop(control_guard);
        drop(creation_guard);
        for (participant_id, media_session_id) in participant_sessions {
            let media_participant_id =
                Self::media_participant_id(room_id, &participant_id, media_session_id);
            if let Err(error) = self
                .media_server
                .transport_manager()
                .remove_participant(&media_participant_id)
                .await
            {
                debug!(%room_id, %participant_id, %error, "Media for a rejoining participant was already gone");
            }
        }
        let teardown = tokio::time::timeout(
            ROOM_DELETE_TIMEOUT,
            self.media_server.remove_router(room_id.as_str()),
        )
        .await;
        let succeeded = matches!(&teardown, Ok(Ok(())));
        release_deletion_reservation_after_router_teardown(
            &self.deleting_rooms,
            room_id,
            deletion_token,
            succeeded,
        );
        anyhow::ensure!(
            succeeded,
            "Recovery router cleanup incomplete; room ID remains reserved"
        );
        Ok(())
    }

    /// Close admission and clear authoritative membership within eight seconds.
    /// The coordinator must then call [`MediaServer::shutdown`] to release all
    /// transports, routers, and workers, even if membership cleanup times out.
    /// No persisted rooms or accounts are deleted.
    pub(crate) async fn shutdown(&self) -> Result<()> {
        self.shutdown_with_budget(std::time::Duration::from_secs(8))
            .await
    }

    pub(in crate::room) async fn shutdown_with_budget(
        &self,
        budget: std::time::Duration,
    ) -> Result<()> {
        use futures_util::StreamExt;
        use std::sync::atomic::AtomicUsize;
        self.drain.begin_draining();
        let remaining = AtomicUsize::new(self.rooms.read().len());
        let acquired_creation = AtomicBool::new(false);
        let cleanup = async {
            // Wait for an earlier room creation to finish or roll back. Holding
            // this lock prevents any late publication after draining the map.
            let _creation = self.room_creation_lock.lock().await;
            acquired_creation.store(true, Ordering::Relaxed);
            let all_rooms: Vec<_> = self.rooms.write().drain().collect();
            remaining.store(all_rooms.len(), Ordering::Relaxed);
            futures_util::stream::iter(all_rooms)
                .for_each_concurrent(16, |(room_id, room_lock)| {
                    let remaining = &remaining;
                    async move {
                        let _control = control::lock_room(&room_lock).await;
                        let mut room = room_lock.write().await;
                        let participants = room.participants.len();
                        let lobby = room.lobby.len();
                        room.deleting = true;
                        room.participants.clear();
                        room.lobby.clear();
                        room.producer_to_participant.clear();
                        room.active_speaker_observer = None;
                        room.audio_level_observer = None;
                        remaining.fetch_sub(1, Ordering::Relaxed);
                        info!(%room_id, participants, lobby, "Room membership drained");
                    }
                })
                .await;
            Ok::<(), anyhow::Error>(())
        };
        let result = crate::shutdown::run_stage("rooms", budget, cleanup).await;
        if result.is_err() {
            warn!(
                remaining_rooms = remaining.load(Ordering::Relaxed),
                acquired_creation_lock = acquired_creation.load(Ordering::Relaxed),
                "Room drain incomplete; global media cleanup will still run"
            );
        }
        result
    }
}
