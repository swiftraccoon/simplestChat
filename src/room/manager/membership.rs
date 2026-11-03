//! Joining and leaving: admission reservations, password checks, the dormant-room
//! precheck, participant records, sender rebinding and departure broadcasts.

use super::super::*;

/// The most producers one audio-level report names; a zero would fail the
/// build, since a constant is evaluated at compile time.
const AUDIO_LEVEL_ENTRIES: std::num::NonZeroU16 = std::num::NonZeroU16::new(10).unwrap();

impl RoomManager {
    pub(in crate::room) fn failed_join_cleanup(&self) -> FailedJoinCleanup {
        FailedJoinCleanup {
            rooms: self.rooms.clone(),
            deleting_rooms: self.deleting_rooms.clone(),
            room_creation_lock: self.room_creation_lock.clone(),
            media_server: self.media_server.clone(),
        }
    }

    pub(in crate::room) async fn reserve_existing_room_join(
        &self,
        room_id: &RoomId,
        room_lock: Arc<TokioRwLock<Room>>,
    ) -> Result<PendingRoomJoin, RoomError> {
        let mut room = measure_result(
            Stage::RoomLockWait,
            tokio::time::timeout(ROOM_CREATION_TIMEOUT, room_lock.write()),
        )
        .await
        .map_err(|_| Refusal::Unavailable("Room is busy; try again".into()))?;
        let admission = self.drain.admit().map_err(|_| shutting_down())?;
        room.ensure_live()?;
        room.pending_joins = room
            .pending_joins
            .checked_add(1)
            .ok_or_else(|| Refusal::Unavailable("Room join capacity has been reached".into()))?;
        drop(admission);
        drop(room);
        Ok(PendingRoomJoin {
            room_id: room_id.clone(),
            room: room_lock,
            cleanup: self.failed_join_cleanup(),
            active: true,
        })
    }

    /// Verifies a supplied room password under the account passwords' NFC policy,
    /// on the blocking pool under the password lane's permit; "Incorrect room
    /// password" when it does not match.
    pub(in crate::room) async fn verify_room_password(
        &self,
        room_id: &RoomId,
        supplied: String,
        hash: String,
    ) -> Result<(), RoomError> {
        let permit = measure_result(Stage::RoomPasswordPermitWait, async {
            tokio::time::timeout(
                PASSWORD_VERIFY_QUEUE_TIMEOUT,
                self.password_verify_work.clone().acquire_owned(),
            )
            .await
            .map_err(|_| Refusal::Unavailable("Password verification is busy; try again".into()))?
            .map_err(|_| Refusal::Unavailable("Password verification is unavailable".into()))
        })
        .await?;
        let verified = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            crate::auth::password::verify_account_password(&supplied, &hash).unwrap_or(false)
        })
        .await
        .map_err(|error| {
            warn!(room = %room_id.log_handle(), %error, "Password verification task failed");
            Refusal::Internal("Password verification failed".into())
        })?;
        if !verified {
            return Err(Refusal::Forbidden("Incorrect room password".into()).into());
        }
        Ok(())
    }

    /// A persisted room that is not live costs a router to join. Everything the
    /// live admission below would refuse from durable state alone (a closed
    /// guest or registration policy, a ban, a missing or wrong password) is
    /// refused here first, so an unauthorized attempt never builds media state
    /// or holds the creation lock. Ad-hoc and live rooms pass straight through;
    /// the live checks still run on the room as created. Returns the hash a
    /// supplied password was proved against, so the live gate can skip a second
    /// Argon2 run when the hash is unchanged.
    pub(in crate::room) async fn dormant_room_precheck(
        &self,
        room_id: &RoomId,
        user_uuid: Option<uuid::Uuid>,
        client_ip: Option<std::net::IpAddr>,
        authenticated: bool,
        password: Option<&str>,
    ) -> Result<Option<String>, RoomError> {
        let Some(pool) = &self.db_pool else {
            return Ok(None);
        };
        if self.rooms.read().contains_key(room_id) {
            return Ok(None);
        }
        let loaded = tokio::time::timeout(
            ROOM_CREATION_TIMEOUT,
            measure_result(Stage::RoomPolicyLookup, settings::load_room(pool, room_id)),
        )
        .await
        .map_err(|_| Refusal::Unavailable("Room data is temporarily unavailable".into()))?
        .map_err(|error| {
            warn!(room = %room_id.log_handle(), %error, "Failed to load room");
            Refusal::Unavailable("Room data is temporarily unavailable".into())
        })?;
        let Some((room_settings, password_hash)) = loaded else {
            return Ok(None);
        };
        // The same order as the live path: a ban answers before policy.
        match measure_result(
            Stage::RoomPolicyLookup,
            moderation::is_banned(pool, room_id, user_uuid, client_ip, authenticated),
        )
        .await
        {
            Ok(true) => {
                return Err(Refusal::Forbidden("You are banned from this room".into()).into());
            }
            Ok(false) => {}
            Err(error) => {
                warn!(room = %room_id.log_handle(), %error, "Ban check failed");
                return Err(
                    Refusal::Unavailable("Room access could not be verified".into()).into(),
                );
            }
        }
        if room_settings.require_registration && !authenticated {
            return Err(Refusal::Forbidden("This room requires registration".into()).into());
        }
        if !room_settings.guests_allowed && !authenticated {
            return Err(Refusal::Forbidden("Guests are not allowed in this room".into()).into());
        }
        if !room_settings.password_protected {
            return Ok(None);
        }
        let role = measure_result(
            Stage::RoomPolicyLookup,
            roles::resolve_role(
                pool,
                room_id,
                user_uuid.as_ref(),
                &room_settings.owner_id.as_uuid(),
                authenticated,
            ),
        )
        .await
        .map_err(|error| {
            warn!(room = %room_id.log_handle(), %error, "Role lookup failed");
            Refusal::Unavailable("Room access could not be verified".into())
        })?;
        if role >= roles::Role::Admin {
            return Ok(None);
        }
        let hash = password_hash
            .ok_or_else(|| Refusal::Unavailable("Room password is unavailable".into()))?;
        let supplied = password.ok_or(RoomError::PasswordRequired)?.to_owned();
        self.verify_room_password(room_id, supplied, hash.clone())
            .await?;
        Ok(Some(hash))
    }

    /// Gets or creates a room, creating a router if needed
    pub(in crate::room) async fn get_or_create_room(
        &self,
        room_id: &RoomId,
    ) -> Result<PendingRoomJoin, RoomError> {
        if self.drain.is_draining() {
            return Err(shutting_down());
        }
        if self.deleting_rooms.read().contains_key(room_id) {
            return Err(Refusal::Unavailable("Room is being deleted".into()).into());
        }
        // Fast path: room exists (brief outer read lock)
        let existing_room = {
            let rooms = self.rooms.read();
            let existing = rooms.get(room_id).cloned();
            if existing.is_none() && rooms.len() >= self.max_rooms {
                return Err(Refusal::Unavailable("Room capacity has been reached".into()).into());
            }
            existing
        };
        if let Some(room) = existing_room {
            return self.reserve_existing_room_join(room_id, room).await;
        }

        // Serialize creation: a concurrent creator may be mid-way between
        // create_router and inserting into the rooms map.
        let _creation_guard = measure_result(
            Stage::RoomCreationLockWait,
            tokio::time::timeout(ROOM_CREATION_TIMEOUT, self.room_creation_lock.lock()),
        )
        .await
        .map_err(|_| Refusal::Unavailable("Room creation is busy; try again".into()))?;

        if self.drain.is_draining() {
            return Err(shutting_down());
        }

        if self.deleting_rooms.read().contains_key(room_id) {
            return Err(Refusal::Unavailable("Room is being deleted".into()).into());
        }
        if self.identity_updates.read().contains(room_id) {
            return Err(
                Refusal::Unavailable("Room settings are being updated; try again".into()).into(),
            );
        }

        // Re-check now that we hold the creation lock
        let existing_room = {
            let rooms = self.rooms.read();
            let existing = rooms.get(room_id).cloned();
            if existing.is_none() && rooms.len() >= self.max_rooms {
                return Err(Refusal::Unavailable("Room capacity has been reached".into()).into());
            }
            existing
        };
        if let Some(room) = existing_room {
            drop(_creation_guard);
            return self.reserve_existing_room_join(room_id, room).await;
        }

        // Load security-relevant state before creating any runtime room. A DB
        // outage must not turn a protected persisted room into an ad-hoc room.
        let (room_settings, password_hash) = if let Some(pool) = &self.db_pool {
            let loaded = tokio::time::timeout(
                ROOM_CREATION_TIMEOUT,
                measure_result(Stage::RoomPolicyLookup, settings::load_room(pool, room_id)),
            )
            .await
            .map_err(|_| Refusal::Unavailable("Room data is temporarily unavailable".into()))?
            .map_err(|error| {
                warn!(room = %room_id.log_handle(), %error, "Failed to load room");
                Refusal::Unavailable("Room data is temporarily unavailable".into())
            })?;
            loaded.map_or((None, None), |(settings, hash)| (Some(settings), hash))
        } else {
            (None, None)
        };
        let persisted = room_settings.is_some();
        if !persisted && !self.allow_ad_hoc_rooms {
            return Err(Refusal::NotFound("Ad-hoc room creation is disabled".into()).into());
        }

        // Slow path: router/observer setup remains serialized so duplicate
        // same-ID creators and the live-room cap stay atomic. Bound the entire
        // media phase; on timeout, best-effort removal prevents a half-created
        // router from poisoning the room ID after the lock is released.
        info!("Creating new room: {}", room_id.log_handle());
        let media_setup = async {
            self.media_server.create_router(room_id.to_string()).await?;
            let router = self
                .media_server
                .router_manager()
                .get_router(room_id.as_str())
                .await?;

            let active_speaker_observer = match router
                .create_active_speaker_observer(ActiveSpeakerObserverOptions::default())
                .await
            {
                Ok(observer) => Some(observer),
                Err(error) => {
                    warn!(
                        room = %room_id.log_handle(),
                        %error,
                        "Failed to create active speaker observer"
                    );
                    None
                }
            };

            let mut options = AudioLevelObserverOptions::default();
            options.max_entries = AUDIO_LEVEL_ENTRIES;
            options.threshold = -50;
            options.interval = 800;
            let audio_level_observer = match router.create_audio_level_observer(options).await {
                Ok(observer) => Some(observer),
                Err(error) => {
                    warn!(
                        room = %room_id.log_handle(),
                        %error,
                        "Failed to create audio level observer"
                    );
                    None
                }
            };
            Ok::<_, RoomError>((active_speaker_observer, audio_level_observer))
        };
        let (active_speaker_observer, audio_level_observer) = match tokio::time::timeout(
            ROOM_CREATION_TIMEOUT,
            measure_result(Stage::RoomMediaSetup, media_setup),
        )
        .await
        {
            Ok(Ok(setup)) => setup,
            Ok(Err(error)) => {
                let _ = tokio::time::timeout(
                    ROOM_DELETE_TIMEOUT,
                    self.media_server.remove_router(room_id.as_str()),
                )
                .await;
                return Err(error);
            }
            Err(_) => {
                warn!(
                    room = %room_id.log_handle(),
                    "Room media setup timed out; rolling back router"
                );
                let _ = tokio::time::timeout(
                    ROOM_DELETE_TIMEOUT,
                    self.media_server.remove_router(room_id.as_str()),
                )
                .await;
                return Err(Refusal::Unavailable(
                    "Room media service is temporarily unavailable".into(),
                )
                .into());
            }
        };
        self.metrics.inc_rooms_created();

        // Create bounded channel for observer events (observer events are ephemeral UI hints)
        let (observer_tx, observer_rx) = tokio::sync::mpsc::channel::<ObserverEvent>(16);

        // Set up callbacks (use try_send — dropping stale events is fine).
        // .detach() is required: dropping the returned HandlerId unregisters the
        // callback, silently killing active-speaker/audio-level events.
        if let Some(obs) = &active_speaker_observer {
            let tx = observer_tx.clone();
            obs.on_dominant_speaker(move |speaker| {
                let _ = tx.try_send(ObserverEvent::ActiveSpeaker {
                    producer_id: speaker.producer.id(),
                });
            })
            .detach();
        }

        if let Some(obs) = &audio_level_observer {
            let tx = observer_tx.clone();
            obs.on_volumes(move |volumes| {
                let entries: Vec<_> = volumes
                    .iter()
                    .map(|v| (v.producer.id(), v.volume))
                    .collect();
                let _ = tx.try_send(ObserverEvent::AudioLevels { volumes: entries });
            })
            .detach();
            // The worker reports volumes only while someone exceeds the
            // threshold and announces silence once when the last speaker
            // stops; without it clients keep the final speaker highlighted.
            let tx = observer_tx.clone();
            obs.on_silence(move || {
                let _ = tx.try_send(ObserverEvent::Silence);
            })
            .detach();
        }
        drop(observer_tx); // Only clones in callbacks remain

        // Publish the initialized room while the creation mutex is still held.
        let room_arc = {
            let mut rooms = self.rooms.write();
            // Every runtime-room insertion is serialized by
            // `room_creation_lock`, which is still held here.
            debug_assert!(!rooms.contains_key(room_id));
            let mut created = Room::new_with_observers(
                room_id.clone(),
                room_settings,
                persisted,
                password_hash,
                active_speaker_observer,
                audio_level_observer,
                self.metrics.clone(),
            );
            created.broadcaster_ceiling = self.max_broadcasters_per_room;
            let new_room = Arc::new(TokioRwLock::new(created));
            if let Ok(_admission) = self.drain.admit() {
                rooms.insert(room_id.clone(), new_room.clone());
                Some(new_room)
            } else {
                None
            }
        };
        let Some(room_arc) = room_arc else {
            // A router created before drain must never become a new runtime
            // room afterwards. Global media shutdown is the fallback if this
            // bounded rollback cannot complete.
            if !matches!(
                tokio::time::timeout(
                    ROOM_CREATION_TIMEOUT,
                    self.media_server.remove_router(room_id.as_str()),
                )
                .await,
                Ok(Ok(()))
            ) {
                warn!(room = %room_id.log_handle(), "Draining room creation rollback incomplete");
            }
            return Err(shutting_down());
        };

        // Spawn background task with Weak reference (only for newly created rooms)
        let weak_room = Arc::downgrade(&room_arc);
        tokio::spawn(Self::observer_broadcast_task(observer_rx, weak_room));

        Ok(PendingRoomJoin {
            room_id: room_id.clone(),
            room: room_arc,
            cleanup: self.failed_join_cleanup(),
            active: true,
        })
    }

    /// Background task that reads observer events and broadcasts to room participants.
    /// Uses a Weak reference so the task exits when the room is dropped.
    pub(in crate::room) async fn observer_broadcast_task(
        mut rx: tokio::sync::mpsc::Receiver<ObserverEvent>,
        weak_room: Weak<TokioRwLock<Room>>,
    ) {
        while let Some(event) = rx.recv().await {
            let room_arc = match weak_room.upgrade() {
                Some(r) => r,
                None => break, // Room is gone
            };

            let room = room_arc.read().await;
            match event {
                ObserverEvent::ActiveSpeaker { producer_id } => {
                    if let Some(participant_id) = room.producer_to_participant.get(&producer_id) {
                        room.broadcast_all(&ServerMessage::ActiveSpeaker {
                            participant_id: *participant_id,
                        });
                    }
                }
                ObserverEvent::AudioLevels { volumes } => {
                    let levels: Vec<AudioLevelEntry> = volumes
                        .iter()
                        .filter_map(|(pid, vol)| {
                            room.producer_to_participant.get(pid).map(|participant_id| {
                                AudioLevelEntry {
                                    participant_id: *participant_id,
                                    volume: *vol,
                                }
                            })
                        })
                        .collect();
                    if !levels.is_empty() {
                        room.broadcast_all(&ServerMessage::AudioLevels { levels });
                    }
                }
                ObserverEvent::Silence => {
                    room.broadcast_all(&ServerMessage::AudioLevels { levels: Vec::new() });
                }
            }
        }
    }

    /// Adds a participant to a room (creates room if needed)
    ///
    /// If the room has `lobby_enabled` and the participant's role would be < Moderator
    /// (and they are not the first participant), they are placed in the lobby instead.
    ///
    /// # Errors
    /// Returns an error if media server operations fail
    #[expect(
        clippy::too_many_arguments,
        reason = "join boundary owns participant identity, channel, and admission state"
    )]
    pub(crate) async fn add_participant(
        &self,
        room_id: &RoomId,
        participant_id: ParticipantId,
        participant_name: String,
        sender: mpsc::Sender<crate::OutboundJson>,
        authenticated: bool,
        in_lobby_flag: Arc<AtomicBool>,
        password: Option<&str>,
        reconnect_token: &str,
        client_ip: Option<std::net::IpAddr>,
        chat_style: Option<ChatStyle>,
    ) -> Result<JoinResult, RoomError> {
        let client_ip = if authenticated {
            client_ip
        } else {
            client_ip.map(crate::net::cohort)
        };
        if let Some(address) = client_ip {
            let address = crate::net::cohort(address);
            let now = std::time::Instant::now();
            if !self.join_attempts_by_ip.allow(address, now)
                || !self
                    .join_attempts_by_room_ip
                    .allow((room_id.to_string(), address), now)
            {
                return Err(
                    Refusal::RateLimited("Room join attempts are rate limited".into()).into(),
                );
            }
        }

        if self.saturated() {
            self.metrics.inc_join_refused_saturated();
            return Err(Refusal::Unavailable(
                "The server is at capacity right now; please try again in a moment".into(),
            )
            .into());
        }
        // A room's router lives on one worker thread; when that thread is
        // saturated the quota may still show headroom, so the refusal is per
        // room. Reconnects and lobby admissions do not pass through here.
        if let Ok(worker_id) = self
            .media_server
            .router_manager()
            .get_worker_id(room_id.as_str())
            .await
            && self
                .media_server
                .worker_manager()
                .worker_saturated(worker_id)
        {
            self.metrics.inc_join_refused_saturated();
            return Err(Refusal::Unavailable(
                "This room's media worker is at capacity right now; please try again in a moment"
                    .into(),
            )
            .into());
        }

        let user_uuid = if authenticated {
            Some(participant_id.as_uuid())
        } else {
            None
        };
        // A dormant persisted room is admitted from its durable record first,
        // so a refused attempt never builds a router.
        let prechecked_hash = self
            .dormant_room_precheck(room_id, user_uuid, client_ip, authenticated, password)
            .await?;

        let mut pending_join = self.get_or_create_room(room_id).await?;
        let room_lock = pending_join.room();

        // A membership retained for reconnect grace belongs to a socket that
        // has already closed. The same identity arriving on a fresh socket (a
        // page reload or network switch has no reconnect token) replaces it
        // rather than being told it still has an active session. The stale
        // socket's grace timer later finds a different sender and does nothing.
        // The pending join reservation keeps the room from being evicted when
        // the stale membership was its last occupant.
        let stale_sender = {
            let room = room_lock.read().await;
            room.participants
                .get(&participant_id)
                .filter(|participant| participant.sender.is_closed())
                .map(|participant| participant.sender.clone())
        };
        if let Some(stale_sender) = stale_sender {
            info!(
                room = %room_id.log_handle(),
                %participant_id,
                "Replacing a retained membership whose socket has closed"
            );
            self.remove_participant_for_sender(room_id, &participant_id, &stale_sender)
                .await?;
        }

        // Brief read lock to check conditions for role resolution
        let (
            lobby_enabled,
            invite_only,
            settings_owner_id,
            password_protected,
            password_hash,
            persisted,
            policy_revision,
        ) = {
            // A persisted policy write may hold control across SQL for up to
            // PERSISTENCE_TIMEOUT; a joiner waits no longer than other control
            // callers rather than stalling its socket for that whole hold.
            let _control =
                tokio::time::timeout(control::ADMISSION_TIMEOUT, control::lock_room(&room_lock))
                    .await
                    .map_err(|_| Refusal::Unavailable("Room control is busy; try again".into()))?;
            let room = measure(Stage::RoomLockWait, room_lock.read()).await;
            let lobby = room.settings.as_ref().is_some_and(|s| s.lobby_enabled);
            let invite_only = room.settings.as_ref().is_some_and(|s| s.invite_only);
            let owner = room.settings.as_ref().map(|s| s.owner_id);
            let pw = room.settings.as_ref().is_some_and(|s| s.password_protected);
            (
                lobby,
                invite_only,
                owner,
                pw,
                room.password_hash.clone(),
                room.persisted,
                room.policy_revision,
            )
        }; // read lock released

        // Persistent ban check (registered users by id, guests by IP)
        if persisted {
            if let Some(pool) = &self.db_pool {
                match measure_result(
                    Stage::RoomPolicyLookup,
                    moderation::is_banned(pool, room_id, user_uuid, client_ip, authenticated),
                )
                .await
                {
                    Ok(true) => {
                        return Err(
                            Refusal::Forbidden("You are banned from this room".into()).into()
                        );
                    }
                    Ok(false) => {}
                    Err(error) => {
                        warn!(room = %room_id.log_handle(), %error, "Ban check failed");
                        return Err(Refusal::Unavailable(
                            "Room access could not be verified".into(),
                        )
                        .into());
                    }
                }
            } else {
                return Err(
                    Refusal::Unavailable("Room access could not be verified".into()).into(),
                );
            }
        }

        // Every join to a persisted room resolves its role from the DB. In
        // particular, a temporarily empty runtime room does not make its first
        // connection the owner.
        let resolved_role = if persisted {
            let pool = self
                .db_pool
                .as_ref()
                .ok_or_else(|| Refusal::Unavailable("Room access could not be verified".into()))?;
            let owner_id = settings_owner_id
                .as_ref()
                .ok_or_else(|| Refusal::Unavailable("Room access could not be verified".into()))?;
            Some(
                measure_result(
                    Stage::RoomPolicyLookup,
                    roles::resolve_role(
                        pool,
                        room_id,
                        user_uuid.as_ref(),
                        &owner_id.as_uuid(),
                        authenticated,
                    ),
                )
                .await
                .map_err(|error| {
                    warn!(room = %room_id.log_handle(), %error, "Role lookup failed");
                    Refusal::Unavailable("Room access could not be verified".into())
                })?,
            )
        } else {
            None
        };

        // An account's saved look replaces what the client sent; a guest's is
        // kept within the palette. A cosmetic lookup never fails a join.
        let requested_style = chat_style
            .and_then(|style| style.validated())
            .unwrap_or_default();
        let chat_style = match (user_uuid, &self.db_pool) {
            (Some(user), Some(pool)) => match tokio::time::timeout(
                control::PERSISTENCE_TIMEOUT,
                measure_result(Stage::RoomPolicyLookup, social::load_chat_style(pool, user)),
            )
            .await
            {
                Ok(Ok(saved)) => saved.unwrap_or_default(),
                Ok(Err(error)) => {
                    warn!(room = %room_id.log_handle(), %error, "Chat style lookup failed");
                    requested_style
                }
                Err(_) => {
                    warn!(room = %room_id.log_handle(), "Chat style lookup timed out");
                    requested_style
                }
            },
            _ => requested_style,
        };

        // Persisted sanctions are part of join authorization. Treat a missing
        // pool or a lookup failure as a failed join rather than silently
        // granting an unsanctioned session during a database outage.
        let persisted_punitive = if persisted {
            let pool = self
                .db_pool
                .as_ref()
                .ok_or_else(|| Refusal::Unavailable("Room access could not be verified".into()))?;
            measure_result(
                Stage::RoomPolicyLookup,
                moderation::load_punitive_state(pool, room_id, user_uuid, client_ip, authenticated),
            )
            .await
            .map_err(|error| {
                warn!(room = %room_id.log_handle(), %error, "Sanction lookup failed");
                Refusal::Unavailable("Room access could not be verified".into())
            })?
        } else {
            moderation::PunitiveState::default()
        };

        // Password gate: everyone below Admin must present the room password.
        // The dormant precheck may already have proved it against this very
        // hash; a hash changed since then is verified again.
        if password_protected && !resolved_role.is_some_and(|r| r >= roles::Role::Admin) {
            let hash = password_hash
                .ok_or_else(|| Refusal::Unavailable("Room password is unavailable".into()))?;
            if prechecked_hash.as_deref() != Some(hash.as_str()) {
                let supplied = password.ok_or(RoomError::PasswordRequired)?.to_owned();
                self.verify_room_password(room_id, supplied, hash).await?;
            }
        }

        // A fresh media namespace is assigned to every successful room
        // membership. It deliberately survives lobby admission and reconnect,
        // but never a leave/kick followed by a new join with the same ID.
        let media_session_id = uuid::Uuid::new_v4();

        // Policy writers retain control across SQL, but not the state lock.
        // A join must not publish inside that database/runtime gap.
        let _control =
            tokio::time::timeout(control::ADMISSION_TIMEOUT, control::lock_room(&room_lock))
                .await
                .map_err(|_| Refusal::Unavailable("Room control is busy; try again".into()))?;
        let account_preferences = self
            .membership_chat_preferences(authenticated, &participant_id)
            .await?;
        // Now acquire write lock for mutation
        let mut room = measure(Stage::RoomLockWait, room_lock.write()).await;
        // Only post-lock validation/commit/fan-out work belongs to this stage;
        // password, database, media setup, and room contention are disjoint.
        measure_result(Stage::RoomMembershipCommit, async {
            let admission = self.drain.admit().map_err(|_| shutting_down())?;

            if !policy_snapshot_matches(&room, policy_revision) {
                return Err(Refusal::Unavailable(
                    "Room policy changed while joining; retry".into(),
                )
                .into());
            }

            // Check ban list before allowing join
            let now = std::time::Instant::now();
            room.prune_expired_bans(now);
            if room.identity_is_banned(&participant_id, authenticated, client_ip) {
                return Err(Refusal::Forbidden("You are banned from this room".into()).into());
            }
            if room.participants.contains_key(&participant_id)
                || room.lobby.contains_key(&participant_id)
            {
                return Err(Refusal::Conflict(
                    "This participant already has an active room session".into(),
                )
                .into());
            }

            // Enforce room modes
            if let Some(settings) = &room.settings {
                if settings.require_registration && !authenticated {
                    return Err(Refusal::Forbidden("This room requires registration".into()).into());
                }
                if !settings.guests_allowed && !authenticated {
                    return Err(
                        Refusal::Forbidden("Guests are not allowed in this room".into()).into(),
                    );
                }
            }
            self.ensure_room_has_space(&room)?;

            if room.lobby.len() >= 1_000 {
                return Err(Refusal::Forbidden("Room lobby is full".into()).into());
            }

            // Account display names are not globally unique either. Apply the
            // same room-local disambiguation to every join; stable UUIDs remain
            // the authorization and message identity, never the display label.
            let participant_name = room.unique_participant_name(participant_name);

            // Re-check is_first under write lock (another join could have raced)
            let is_first = room.participants.is_empty() && room.lobby.is_empty();

            // Determine final role
            let role = select_join_role(persisted, is_first, resolved_role, authenticated)
                .ok_or_else(|| Refusal::Unavailable("Room access could not be verified".into()))?;
            let sanction_key = SanctionKey::for_identity(&participant_id, authenticated, client_ip);
            // A sanction may have been installed after the DB snapshot but before
            // this join acquired the room lock. Restrictions only merge in the safe
            // direction.
            let punitive =
                merge_punitive_state(persisted_punitive, room.sanctions.get(&sanction_key));
            if punitive != moderation::PunitiveState::default() {
                room.sanctions.insert(sanction_key, punitive.clone());
            }

            // Invite-only rooms treat a persisted Member+ role as the backwards-
            // compatible invitation. Unknown users wait for an explicit moderator
            // admission even when the ordinary lobby toggle is off.
            let must_wait_for_invite = invite_only && role < roles::Role::Member;
            let ordinary_lobby = lobby_enabled && !is_first && role < roles::Role::Moderator;
            if must_wait_for_invite || ordinary_lobby {
                // Place in lobby
                let room_name = room
                    .settings
                    .as_ref()
                    .map_or_else(|| room.id.to_string(), |s| s.display_name.clone());
                let topic = room.settings.as_ref().and_then(|s| s.topic.clone());
                let (participant_count, moderator_count) = room.lobby_counts();

                // Send LobbyWaiting to the participant
                let lobby_waiting = ServerMessage::LobbyWaiting {
                    room_name,
                    topic,
                    participant_count,
                    moderator_count,
                };
                if let Ok(json) = serde_json::to_string(&lobby_waiting) {
                    let _ =
                        try_send_essential(&room.metrics, &sender, crate::OutboundJson::from(json));
                }

                // Broadcast LobbyJoin to Moderator+ participants
                room.broadcast_to_role(
                    roles::Role::Moderator,
                    &ServerMessage::LobbyJoin {
                        participant_id,
                        display_name: participant_name.clone(),
                        authenticated,
                    },
                );

                // Add to lobby map (reconnect_token set later by connection handler)
                room.lobby.insert(
                    participant_id,
                    LobbyEntry {
                        participant_id,
                        name: participant_name.clone(),
                        sender,
                        media_session_id,
                        authenticated,
                        reconnect_token: reconnect_token.to_string(),
                        in_lobby_flag,
                        ip: client_ip,
                        role,
                        punitive,
                        chat_style: chat_style.clone(),
                    },
                );
                pending_join.complete_locked(&mut room);
                drop(admission);

                info!(
                    "Participant {} entered lobby for room {}",
                    participant_id,
                    room_id.log_handle()
                );
                return Ok(JoinResult::Lobbied);
            }

            let mut social = social::ParticipantSocial::new(room.social.next_sequence);
            if let Some(preferences) = &account_preferences {
                social.apply_account_preferences(preferences);
            }
            let participant = Participant {
                id: participant_id,
                social,
                name: participant_name.clone(),
                sender,
                media_session_id,
                producers: HashMap::new(),
                role,
                punitive,
                authenticated,
                ip: client_ip,
                chat_style: chat_style.clone(),
            };

            room.participants.insert(participant_id, participant);
            room.notify_lobby_status();
            pending_join.complete_locked(&mut room);
            // The membership is committed. Do not serialize response snapshots
            // under the process-wide admission guard.
            drop(admission);

            info!(
                "Participant {} joined room {}",
                participant_id,
                room_id.log_handle()
            );

            // Notify other participants
            room.broadcast_except(
                &participant_id,
                &ServerMessage::ParticipantJoined {
                    participant_id,
                    participant_name: participant_name.clone(),
                    role,
                    authenticated,
                    chat_style: chat_style.clone(),
                },
            );

            // The mutation is complete; the response snapshot only needs a
            // consistent read. Downgrading atomically lets chat and lookups
            // proceed instead of waiting on O(participants) cloning.
            let room = tokio::sync::RwLockWriteGuard::downgrade(room);

            // Return list of existing participants
            let participants: Vec<ParticipantInfo> = room
                .participants
                .values()
                .filter(|p| p.id != participant_id)
                .map(|p| ParticipantInfo {
                    id: p.id,
                    name: p.name.clone(),
                    producers: p
                        .producers
                        .iter()
                        .map(
                            |(id, ProducerRecord { kind, source, .. })| ProducerMetadata {
                                id: *id,
                                kind: *kind,
                                source: source.clone(),
                            },
                        )
                        .collect(),
                    role: p.role,
                    authenticated: p.authenticated,
                    chat_style: p.chat_style.clone(),
                })
                .collect();

            // Serialize room settings for the joining client
            let room_settings = room
                .settings
                .as_ref()
                .map(|settings| Box::new(settings::RoomSettingsView::from(settings)));

            Ok(JoinResult::Joined {
                participants,
                role,
                room_settings,
                chat_style,
                name: participant_name,
            })
        })
        .await
    }

    /// Test convenience: remove a participant whatever socket owns the
    /// membership, as the server does for its own authoritative evictions.
    #[cfg(test)]
    pub(crate) async fn remove_participant(
        &self,
        room_id: &RoomId,
        participant_id: &ParticipantId,
    ) -> Result<(), RoomError> {
        self.remove_participant_inner(room_id, participant_id, None)
            .await
            .map(|_| ())
    }

    /// Removes a participant only if this sender is still the authoritative
    /// connection for the room membership. Used by socket leave/disconnect and
    /// grace timers so an old socket cannot evict a same-ID replacement.
    pub(crate) async fn remove_participant_for_sender(
        &self,
        room_id: &RoomId,
        participant_id: &ParticipantId,
        expected_sender: &mpsc::Sender<crate::OutboundJson>,
    ) -> Result<bool, RoomError> {
        self.remove_participant_inner(room_id, participant_id, Some(expected_sender))
            .await
    }

    pub(in crate::room) async fn remove_participant_inner(
        &self,
        room_id: &RoomId,
        participant_id: &ParticipantId,
        expected_sender: Option<&mpsc::Sender<crate::OutboundJson>>,
    ) -> Result<bool, RoomError> {
        let mut removed_any = false;
        let mut room_empty = false;
        let mut media_session_id = None;
        let mut audio_producer_ids: Vec<ProducerId> = Vec::new();
        let mut active_obs = None;
        let mut audio_obs = None;

        // Get room lock (brief outer read)
        let room_lock = {
            let rooms = self.rooms.read();
            match rooms.get(room_id) {
                Some(r) => r.clone(),
                None => return Ok(false),
            }
        };

        // Lock only this room
        {
            let _control = control::lock_room(&room_lock).await;
            let mut room = room_lock.write().await;

            if let Some(expected_sender) = expected_sender {
                let sender_matches = room
                    .participants
                    .get(participant_id)
                    .is_some_and(|participant| participant.sender.same_channel(expected_sender))
                    || room
                        .lobby
                        .get(participant_id)
                        .is_some_and(|entry| entry.sender.same_channel(expected_sender));
                if !sender_matches {
                    return Ok(false);
                }
            }

            // Also remove from lobby if present (lobby participants have no media to clean up)
            if room.lobby.remove(participant_id).is_some() {
                removed_any = true;
                info!(
                    "Lobby participant {} removed from room {}",
                    participant_id,
                    room_id.log_handle()
                );
                room_empty = room.participants.is_empty() && room.lobby.is_empty();
                // No media cleanup needed for lobby participants
            }

            if let Some(participant) = room.participants.remove(participant_id) {
                removed_any = true;
                media_session_id = Some(participant.media_session_id);
                info!(
                    "Participant {} left room {}",
                    participant_id,
                    room_id.log_handle()
                );

                // Collect audio producer IDs for observer cleanup
                for (pid, ProducerRecord { kind, .. }) in &participant.producers {
                    if *kind == MediaKind::Audio {
                        audio_producer_ids.push(*pid);
                    }
                }

                // Remove every producer from the room ownership index. Audio
                // producers are additionally removed from observers below.
                for pid in participant.producers.keys() {
                    room.producer_to_participant.remove(pid);
                }

                // Clone observers before releasing lock
                active_obs = room.active_speaker_observer.clone();
                audio_obs = room.audio_level_observer.clone();

                room.broadcast_participant_left(participant_id);
                room.notify_lobby_status();

                room_empty = room.participants.is_empty() && room.lobby.is_empty();
            }
        } // Release per-room lock before outer write

        // Remove audio producers from observers OUTSIDE lock
        for pid in audio_producer_ids {
            if let Some(obs) = &active_obs {
                let _ = obs.remove_producer(pid).await;
            }
            if let Some(obs) = &audio_obs {
                let _ = obs.remove_producer(pid).await;
            }
        }

        // Only a removed participant has a media session to close.
        if let Some(media_session_id) = media_session_id {
            // Close transports, producers, consumers — releases FDs
            let media_participant_id =
                Self::media_participant_id(room_id, participant_id, media_session_id);
            if let Err(e) = self
                .media_server
                .transport_manager()
                .remove_participant(&media_participant_id)
                .await
            {
                warn!(
                    "Failed to clean up media for participant {}: {}",
                    participant_id, e
                );
            }
        }

        if room_empty {
            // Serialize the final empty-room check, map removal, and router
            // removal against room creation. This also cleans routers for
            // lobby-only rooms and prevents deleting a router just recreated
            // by a concurrent join.
            let creation_guard =
                match tokio::time::timeout(ROOM_CREATION_TIMEOUT, self.room_creation_lock.lock())
                    .await
                {
                    Ok(guard) => guard,
                    Err(_) => {
                        warn!(
                            room = %room_id.log_handle(),
                            "Timed out waiting to evict an empty room"
                        );
                        return Ok(removed_any);
                    }
                };
            let control_guard = control::lock_room(&room_lock).await;
            let mut room = match tokio::time::timeout(ROOM_DELETE_TIMEOUT, room_lock.write()).await
            {
                Ok(room) => room,
                Err(_) => {
                    warn!(
                        room = %room_id.log_handle(),
                        "Timed out rechecking an empty room; leaving it active"
                    );
                    return Ok(removed_any);
                }
            };
            let still_mapped = self
                .rooms
                .read()
                .get(room_id)
                .is_some_and(|candidate| Arc::ptr_eq(candidate, &room_lock));
            let should_remove = still_mapped
                && !room.deleting
                && room.pending_joins == 0
                && room.participants.is_empty()
                && room.lobby.is_empty();
            let eviction_token = if should_remove {
                // Invalidate any join that cloned this Arc before we remove it.
                room.deleting = true;
                room.policy_revision = room.policy_revision.wrapping_add(1);
                let deletion_token = uuid::Uuid::new_v4();
                self.deleting_rooms
                    .write()
                    .insert(room_id.clone(), deletion_token);
                let mut rooms = self.rooms.write();
                if rooms
                    .get(room_id)
                    .is_some_and(|candidate| Arc::ptr_eq(candidate, &room_lock))
                {
                    rooms.remove(room_id);
                    Some(deletion_token)
                } else {
                    release_deletion_reservation(&self.deleting_rooms, room_id, deletion_token);
                    None
                }
            } else {
                None
            };
            drop(room);
            drop(control_guard);
            drop(creation_guard);
            if let Some(deletion_token) = eviction_token {
                let router_teardown_succeeded = match tokio::time::timeout(
                    ROOM_DELETE_TIMEOUT,
                    self.media_server.remove_router(room_id.as_str()),
                )
                .await
                {
                    Ok(Ok(())) => {
                        info!("Room {} is empty, cleaned up router", room_id.log_handle());
                        true
                    }
                    Ok(Err(error)) => {
                        warn!(
                            room = %room_id.log_handle(),
                            %error,
                            "Failed to tear down empty-room router; retaining room-ID reservation"
                        );
                        false
                    }
                    Err(_) => {
                        warn!(
                            room = %room_id.log_handle(),
                            "Timed out tearing down empty-room router; retaining room-ID reservation"
                        );
                        false
                    }
                };
                release_deletion_reservation_after_router_teardown(
                    &self.deleting_rooms,
                    room_id,
                    deletion_token,
                    router_teardown_succeeded,
                );
            }
        }

        Ok(removed_any)
    }

    /// Best-effort exclusion of this exact socket from lobby availability during grace.
    pub(crate) async fn mark_participant_disconnected(
        &self,
        room_id: &RoomId,
        participant_id: &ParticipantId,
        expected_sender: &mpsc::Sender<crate::OutboundJson>,
    ) -> Result<(), RoomError> {
        if self.drain.is_draining() {
            return Ok(());
        }
        let room_lock = self.get_room(room_id)?;
        // The writer is already closed in connection cleanup. A contended
        // notification must not hold a connection permit indefinitely; later
        // snapshots also exclude closed queues even if this update is skipped.
        let mut room = tokio::select! {
            biased;
            _ = self.drain.wait() => return Ok(()),
            result = tokio::time::timeout(std::time::Duration::from_millis(100), room_lock.write()) => {
                let Ok(room) = result else { return Ok(()); };
                room
            }
        };
        room.mark_disconnected(participant_id, expected_sender);
        Ok(())
    }

    /// Rebinds a participant's WS sender after reconnection
    pub(crate) async fn rebind_participant_sender(
        &self,
        room_id: &RoomId,
        participant_id: &ParticipantId,
        authenticated_subject: Option<crate::ids::UserId>,
        expected_sender: &mpsc::Sender<crate::OutboundJson>,
        new_sender: mpsc::Sender<crate::OutboundJson>,
    ) -> Result<bool, RoomError> {
        let room_lock = self.get_room(room_id)?;
        let _control = control::lock_room(&room_lock).await;
        let mut room = room_lock.write().await;
        let _admission = self.drain.admit().map_err(|_| shutting_down())?;
        room.ensure_live()?;
        if let Some(participant) = room.participants.get_mut(participant_id) {
            if !participant.sender.same_channel(expected_sender) {
                warn!(
                    room = %room_id.log_handle(),
                    %participant_id,
                    "Reconnect attempted against a superseded room session"
                );
                return Ok(false);
            }
            // An account participates as its own UUID.
            let identity_matches = match (participant.authenticated, authenticated_subject) {
                (true, Some(subject)) => subject.as_uuid() == participant_id.as_uuid(),
                (false, None) => true,
                _ => false,
            };
            if !identity_matches {
                warn!(
                    room = %room_id.log_handle(),
                    %participant_id,
                    "Reconnect authentication identity mismatch"
                );
                return Ok(false);
            }
            participant.sender = new_sender;
            participant.social.connected = true;
            room.notify_lobby_status();
            Ok(true)
        } else {
            Ok(false)
        }
    }

    pub(in crate::room) fn participant_for_sender<'a>(
        room: &'a Room,
        participant_id: &ParticipantId,
        expected_sender: &mpsc::Sender<crate::OutboundJson>,
    ) -> Result<&'a Participant, RoomError> {
        room.ensure_live()?;
        let participant = room.participants.get(participant_id).ok_or_else(|| {
            Refusal::NotFound("Participant is not an active member of this room".into())
        })?;
        if !participant.sender.same_channel(expected_sender) {
            return Err(Refusal::Forbidden(
                "This connection no longer owns the participant session".into(),
            )
            .into());
        }
        Ok(participant)
    }

    pub(in crate::room) fn participant_session_is_current(
        room: &Room,
        participant_id: &ParticipantId,
        expected_sender: &mpsc::Sender<crate::OutboundJson>,
        media_session_id: uuid::Uuid,
    ) -> bool {
        !room.deleting
            && room
                .participants
                .get(participant_id)
                .is_some_and(|participant| {
                    participant.media_session_id == media_session_id
                        && participant.sender.same_channel(expected_sender)
                })
    }

    pub(crate) async fn is_bound_participant(
        &self,
        room_id: &RoomId,
        participant_id: &ParticipantId,
        expected_sender: &mpsc::Sender<crate::OutboundJson>,
    ) -> bool {
        let Ok(room_lock) = self.get_room(room_id) else {
            return false;
        };
        let room = room_lock.read().await;
        !room.deleting
            && (room
                .participants
                .get(participant_id)
                .is_some_and(|participant| participant.sender.same_channel(expected_sender))
                || room
                    .lobby
                    .get(participant_id)
                    .is_some_and(|entry| entry.sender.same_channel(expected_sender)))
    }
}
