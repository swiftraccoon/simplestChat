//! The room manager's methods, one file per concern; the manager and room state, the
//! admission tables and the shared helpers stay in `room/mod.rs`. A method is
//! `pub(crate)` when the signaling or HTTP layer calls it and `pub(in crate::room)`
//! when only the room module tree does.

mod counts;
mod lifecycle;
mod lobby;
mod media_ops;
mod membership;
mod moderation_ops;
mod settings_ops;

use super::*;

impl RoomManager {
    /// Creates a new room manager
    ///
    /// # Errors
    /// Returns an error if room configuration is invalid or media server initialization fails
    pub(crate) async fn new(
        settings: ManagerSettings,
        media_config: MediaConfig,
        metrics: ServerMetrics,
        db_pool: Option<sqlx::PgPool>,
    ) -> Result<Self> {
        let ManagerSettings {
            allow_ad_hoc_rooms,
            max_participants_per_room,
            max_broadcasters_per_room,
            password_verify_workers,
            max_rooms,
            max_persisted_rooms,
            join_attempts_per_ip_per_minute,
            join_attempts_per_room_ip_per_minute,
            account_update_capacity,
        } = settings;
        let media_server = Arc::new(MediaServer::new(media_config).await?);

        Ok(Self {
            rooms: Arc::new(RwLock::new(HashMap::new())),
            media_server,
            drain: DrainSignal::default(),
            metrics,
            db_pool,
            account_updates: crate::auth::updates::Hub::with_capacity(account_update_capacity),
            room_creation_lock: Arc::new(tokio::sync::Mutex::new(())),
            deleting_rooms: Arc::new(RwLock::new(HashMap::new())),
            identity_updates: Arc::new(RwLock::new(HashSet::new())),
            max_rooms,
            max_persisted_rooms,
            allow_ad_hoc_rooms,
            max_participants_per_room,
            max_broadcasters_per_room,
            saturation: std::sync::OnceLock::new(),
            password_verify_work: Arc::new(tokio::sync::Semaphore::new(password_verify_workers)),
            // One bounded hashing lane prevents room settings from consuming
            // every CPU while the separate verification lane serves joiners.
            password_hash_work: Arc::new(tokio::sync::Semaphore::new(1)),
            max_password_verify_work: password_verify_workers,
            join_attempts_by_ip: SharedRateLimiter::new(join_attempts_per_ip_per_minute),
            join_attempts_by_room_ip: SharedRateLimiter::new(join_attempts_per_room_ip_per_minute),
        })
    }

    /// Whether this process permits creation of non-persisted rooms.
    pub(crate) fn allow_ad_hoc_rooms(&self) -> bool {
        self.allow_ad_hoc_rooms
    }

    pub(crate) fn account_updates(&self) -> &crate::auth::updates::Hub {
        &self.account_updates
    }

    /// Gets the media server for direct access (e.g., find_producer_paused)
    pub(crate) fn media_server(&self) -> &MediaServer {
        &self.media_server
    }

    /// Isolated real-socket connection regressions need an explicitly enabled
    /// ad-hoc room without mutating process-wide environment or binding a fixed
    /// native port. This constructor does not exist in production builds.
    #[cfg(test)]
    pub(crate) async fn new_for_connection_tests(metrics: ServerMetrics) -> Self {
        Self::new_for_connection_tests_with_pool(metrics, None).await
    }

    /// Authenticated connection fixtures use the same disposable database for
    /// socket authorization and the room's canonical account privacy reads.
    #[cfg(test)]
    pub(crate) async fn new_for_connection_tests_with_pool(
        metrics: ServerMetrics,
        pool: Option<sqlx::PgPool>,
    ) -> Self {
        let mut config = MediaConfig::default();
        config.worker_config.num_workers = 1;
        config.webrtc_server_port_base = 0;
        let mut manager = Self::new(ManagerSettings::default(), config, metrics, pool)
            .await
            .unwrap();
        manager.allow_ad_hoc_rooms = true;
        manager
    }

    #[cfg(test)]
    pub(crate) fn room_for_connection_tests(&self, room_id: &RoomId) -> Arc<TokioRwLock<Room>> {
        self.get_room(room_id).unwrap()
    }

    /// Shared one-way admission and shutdown notification for this process.
    pub(crate) fn drain_signal(&self) -> DrainSignal {
        self.drain.clone()
    }

    /// Installs the CPU saturation monitor once; later calls are ignored.
    pub(crate) fn attach_saturation(&self, monitor: crate::saturation::SaturationMonitor) -> bool {
        self.saturation.set(monitor).is_ok()
    }

    /// Whether fresh joins are currently refused for CPU saturation.
    pub(crate) fn saturated(&self) -> bool {
        self.saturation
            .get()
            .is_some_and(crate::saturation::SaturationMonitor::saturated)
    }

    /// Starts the periodic server-side media quality sample; the task holds
    /// only a weak handle and ends with the manager or on drain.
    pub(crate) fn spawn_quality_sampler(
        self: &Arc<Self>,
        interval: std::time::Duration,
        max_transport_stats: usize,
    ) {
        let manager = Arc::downgrade(self);
        let drain = self.drain.clone();
        tokio::spawn(async move {
            let mut cursor = 0usize;
            let mut ticker = tokio::time::interval(interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            ticker.tick().await;
            loop {
                tokio::select! {
                    _ = drain.wait() => break,
                    _ = ticker.tick() => {
                        let Some(manager) = manager.upgrade() else { break };
                        let sample = manager
                            .media_server
                            .transport_manager()
                            .quality_sample(&mut cursor, max_transport_stats)
                            .await;
                        manager.metrics.set_quality_sample(sample);
                    }
                }
            }
        });
    }

    pub(crate) fn password_work_capacity(&self) -> usize {
        self.max_password_verify_work + 1
    }

    /// Jobs retain these permits inside `spawn_blocking`, including after their
    /// requesting socket/HTTP future is cancelled. Includes hash and verify lanes.
    pub(crate) fn pending_password_work(&self) -> usize {
        self.max_password_verify_work
            .saturating_sub(self.password_verify_work.available_permits())
            + 1_usize.saturating_sub(self.password_hash_work.available_permits())
    }

    pub(crate) async fn hash_room_password(&self, password: String) -> Result<String> {
        let permit = self
            .password_hash_work
            .clone()
            .try_acquire_owned()
            .map_err(|_| anyhow::anyhow!("Password service is busy; try again"))?;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            crate::auth::password::hash_password(&password)
        })
        .await
        .map_err(|error| {
            warn!(%error, "Password hash task failed");
            anyhow::anyhow!("Password update failed")
        })?
        .map_err(|error| {
            warn!(%error, "Password hashing failed");
            anyhow::anyhow!("Password update failed")
        })
    }

    /// Excludes creation of one room while its durable identity is updated.
    /// The marker lives as long as the returned guard, so an update task that
    /// outlives its HTTP caller keeps the exclusion until it completes.
    pub(crate) fn begin_identity_update(&self, room_id: &RoomId) -> IdentityUpdateGuard {
        self.identity_updates.write().insert(room_id.to_owned());
        IdentityUpdateGuard {
            updates: self.identity_updates.clone(),
            room_id: room_id.to_owned(),
        }
    }

    /// Gets a room lock by ID (brief outer read lock, no await)
    pub(in crate::room) fn get_room(&self, room_id: &RoomId) -> Result<Arc<TokioRwLock<Room>>> {
        let rooms = self.rooms.read();
        rooms
            .get(room_id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Room not found: {room_id}"))
    }
}
