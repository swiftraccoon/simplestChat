#![forbid(unsafe_code)]

// Router management for rooms

use crate::media::config::RouterConfig;
use crate::media::types::{MediaError, MediaResult};
use crate::media::worker_manager::{RouterLoadReservation, WorkerManager};
use anyhow::Result;
use mediasoup::prelude::*;
use mediasoup::router::{PipeToRouterOptions, RouterDump};
use mediasoup::worker::WorkerId;
use std::collections::{HashMap, hash_map::Entry};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

/// Below this many consumers on a room's primary worker its viewers stay
/// there: every extra worker costs one pipe per producer, which small rooms
/// do not repay, and rooms below it behave exactly as before.
pub(crate) const VIEWER_SPREAD_MIN_CONSUMERS: usize = 64;

/// Information about a router and its associated worker
struct RouterInfo {
    router: Router,
    worker_id: WorkerId,
    /// Releases allocation load when this entry is removed or creation is cancelled.
    _load_reservation: RouterLoadReservation,
}

/// Both ends of a pipe from the primary router; dropping them closes it.
struct PipedProducerHandles {
    _pipe_consumer: Consumer,
    pipe_producer: Producer,
}

/// One room's viewer routers on other workers and the pipes that feed them.
#[derive(Default)]
struct ViewerRouters {
    by_worker: HashMap<WorkerId, RouterInfo>,
    /// Keyed by the original producer and the viewer worker it is piped to.
    pipes: HashMap<(ProducerId, WorkerId), PipedProducerHandles>,
}

/// Media participant id to the worker of its receive transport, with the
/// ticket of the lease that recorded it.
type Placements = Arc<std::sync::Mutex<HashMap<String, (WorkerId, u64)>>>;

/// Records where a viewer's receive transport lives. It drops with the
/// participant's media, so every removal path forgets the placement, and it
/// only removes the placement it recorded (a later lease for the same id wins).
pub struct ViewerLease {
    participant_id: String,
    ticket: u64,
    placements: Placements,
}

impl Drop for ViewerLease {
    fn drop(&mut self) {
        let mut placements = self.placements.lock().unwrap_or_else(|e| e.into_inner());
        if placements
            .get(&self.participant_id)
            .is_some_and(|(_, ticket)| *ticket == self.ticket)
        {
            placements.remove(&self.participant_id);
        }
    }
}

impl std::fmt::Debug for ViewerLease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ViewerLease")
            .field("participant_id", &self.participant_id)
            .field("ticket", &self.ticket)
            .finish()
    }
}

/// Manages routers for different rooms
pub struct RouterManager {
    routers: Arc<RwLock<HashMap<String, RouterInfo>>>,
    /// Viewer routers per room. Held across native creation and piping so a
    /// room pipes each producer to a worker exactly once.
    viewer_routers: tokio::sync::Mutex<HashMap<String, ViewerRouters>>,
    placements: Placements,
    next_lease_ticket: AtomicU64,
    worker_manager: Arc<WorkerManager>,
}

impl RouterManager {
    /// Creates a new RouterManager
    pub fn new(worker_manager: Arc<WorkerManager>) -> Self {
        Self {
            routers: Arc::new(RwLock::new(HashMap::new())),
            viewer_routers: tokio::sync::Mutex::new(HashMap::new()),
            placements: Arc::new(std::sync::Mutex::new(HashMap::new())),
            next_lease_ticket: AtomicU64::new(1),
            worker_manager,
        }
    }

    /// Chooses the router for a participant's receive transport. The primary
    /// router serves it while the primary worker carries fewer than
    /// `VIEWER_SPREAD_MIN_CONSUMERS` consumers or is the least loaded worker;
    /// otherwise the room's viewer router on the least loaded worker does,
    /// created on first use. A lease comes back only for a new placement off
    /// the primary router; a placed participant gets its router again.
    ///
    /// # Errors
    /// Returns an error if the room has no router, no worker is eligible, or
    /// native router creation fails.
    pub async fn place_viewer(
        &self,
        room_id: &str,
        participant_id: &str,
        config: &RouterConfig,
    ) -> MediaResult<(Router, WorkerId, Option<ViewerLease>)> {
        let (primary_router, primary_worker) = {
            let routers = self.routers.read().await;
            let info = routers.get(room_id).ok_or_else(|| {
                MediaError::RouterError(format!("Router not found for room: {room_id}"))
            })?;
            (info.router.clone(), info.worker_id)
        };

        if let Some(worker_id) = self.worker_for_participant(participant_id) {
            let viewers = self.viewer_routers.lock().await;
            if let Some(info) = viewers
                .get(room_id)
                .and_then(|room| room.by_worker.get(&worker_id))
                && !info.router.closed()
            {
                return Ok((info.router.clone(), worker_id, None));
            }
            drop(viewers);
            // The placed router is gone (its worker died): place again below.
            self.placements
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(participant_id);
        }

        let primary_consumers = self
            .worker_manager
            .get_consumer_counter(primary_worker)
            .map_or(0, |counter| counter.load(Ordering::Relaxed));
        if primary_consumers < VIEWER_SPREAD_MIN_CONSUMERS {
            return Ok((primary_router, primary_worker, None));
        }

        let (worker, reservation) = self.worker_manager.reserve_worker().await?;
        let worker_id = worker.id();
        if worker_id == primary_worker {
            drop(reservation);
            return Ok((primary_router, primary_worker, None));
        }

        let mut viewers = self.viewer_routers.lock().await;
        let room = viewers.entry(room_id.to_string()).or_default();
        if let Some(info) = room.by_worker.get(&worker_id)
            && !info.router.closed()
        {
            drop(reservation);
            let router = info.router.clone();
            let lease = self.record_placement(participant_id, worker_id);
            return Ok((router, worker_id, Some(lease)));
        }
        let router = worker
            .create_router(config.to_router_options())
            .await
            .map_err(|e| MediaError::RouterError(format!("Failed to create viewer router: {e}")))?;
        self.setup_router_handlers(&router, room_id);
        info!(
            "Created viewer router {} for room {} on worker {}",
            router.id(),
            room_id,
            worker_id
        );
        room.by_worker.insert(
            worker_id,
            RouterInfo {
                router: router.clone(),
                worker_id,
                _load_reservation: reservation,
            },
        );
        let lease = self.record_placement(participant_id, worker_id);
        Ok((router, worker_id, Some(lease)))
    }

    fn record_placement(&self, participant_id: &str, worker_id: WorkerId) -> ViewerLease {
        let ticket = self.next_lease_ticket.fetch_add(1, Ordering::Relaxed);
        self.placements
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(participant_id.to_string(), (worker_id, ticket));
        ViewerLease {
            participant_id: participant_id.to_string(),
            ticket,
            placements: Arc::clone(&self.placements),
        }
    }

    /// The worker of a participant's receive transport when it is not the
    /// room's primary worker.
    pub fn worker_for_participant(&self, participant_id: &str) -> Option<WorkerId> {
        self.placements
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(participant_id)
            .map(|(worker_id, _)| *worker_id)
    }

    /// Pipes a producer from the room's primary router to the viewer router of
    /// a placed participant, once per producer and worker. Participants on the
    /// primary router need nothing. The pipe follows the producer's close; a
    /// closed pipe is forgotten and a request for a closed producer fails.
    ///
    /// # Errors
    /// Returns an error if the room or the participant's viewer router is gone
    /// or the native pipe cannot be created.
    pub async fn ensure_piped(
        &self,
        room_id: &str,
        participant_id: &str,
        producer_id: ProducerId,
    ) -> MediaResult<()> {
        let Some(worker_id) = self.worker_for_participant(participant_id) else {
            return Ok(());
        };
        let primary_router = self.get_router(room_id).await?;
        let mut viewers = self.viewer_routers.lock().await;
        let room = viewers.get_mut(room_id).ok_or_else(|| {
            MediaError::RouterError(format!("No viewer routers for room: {room_id}"))
        })?;
        let key = (producer_id, worker_id);
        if let Some(handles) = room.pipes.get(&key) {
            if !handles.pipe_producer.closed() {
                return Ok(());
            }
            room.pipes.remove(&key);
        }
        let target = room
            .by_worker
            .get(&worker_id)
            .filter(|info| !info.router.closed())
            .map(|info| info.router.clone())
            .ok_or_else(|| {
                MediaError::RouterError(format!("Viewer router is gone for room: {room_id}"))
            })?;
        let pair = primary_router
            .pipe_producer_to_router(producer_id, PipeToRouterOptions::new(target))
            .await
            .map_err(|e| {
                MediaError::RouterError(format!("Failed to pipe producer {producer_id}: {e}"))
            })?;
        debug!(
            "Piped producer {} of room {} to worker {}",
            producer_id, room_id, worker_id
        );
        room.pipes.insert(
            key,
            PipedProducerHandles {
                _pipe_consumer: pair.pipe_consumer,
                pipe_producer: pair.pipe_producer.into_inner(),
            },
        );
        Ok(())
    }

    /// Viewer routers a room currently has on other workers.
    pub async fn viewer_router_count(&self, room_id: &str) -> usize {
        self.viewer_routers
            .lock()
            .await
            .get(room_id)
            .map_or(0, |room| room.by_worker.len())
    }

    /// Live pipes feeding a room's viewer routers.
    pub async fn pipe_count(&self, room_id: &str) -> usize {
        self.viewer_routers
            .lock()
            .await
            .get(room_id)
            .map_or(0, |room| {
                room.pipes
                    .values()
                    .filter(|handles| !handles.pipe_producer.closed())
                    .count()
            })
    }

    /// Forgets the viewer routers and pipes a dead worker hosted. Their
    /// participants' transports died with the worker; primary routers are
    /// handled by [`Self::rooms_on_worker`]. Returns the number dropped.
    pub async fn drop_viewer_routers_on_worker(&self, worker_id: WorkerId) -> usize {
        let mut viewers = self.viewer_routers.lock().await;
        let mut dropped = 0;
        for room in viewers.values_mut() {
            if room.by_worker.remove(&worker_id).is_some() {
                dropped += 1;
            }
            room.pipes
                .retain(|(_, pipe_worker), _| *pipe_worker != worker_id);
        }
        dropped
    }

    /// Creates and registers one router for a room without holding the room map
    /// lock across native creation. Concurrent creation for the same room can
    /// register only one winner; a losing call cannot replace its router.
    ///
    /// Pending allocation load is refunded if this future fails or is dropped.
    /// After registration, the entry owns the reservation until removal or drain.
    /// This accounting guarantee does not make native IPC cancellation atomic.
    ///
    /// # Errors
    /// Returns an error if the room already has a router, no worker with an open
    /// listener is available, or native router creation fails.
    pub async fn create_router(
        &self,
        room_id: String,
        config: RouterConfig,
    ) -> MediaResult<String> {
        // Check if router already exists
        {
            let routers = self.routers.read().await;
            if routers.contains_key(&room_id) {
                return Err(MediaError::RouterError(format!(
                    "Router already exists for room: {room_id}"
                )));
            }
        }

        // Keep the reservation alive through native creation and map insertion.
        // Every error or cancellation path releases it synchronously.
        let (worker, load_reservation) = self.worker_manager.reserve_worker().await?;
        let worker_id = worker.id();

        // Create the router
        let router_options = config.to_router_options();
        let router = worker
            .create_router(router_options)
            .await
            .map_err(|e| MediaError::RouterError(format!("Failed to create router: {e}")))?;

        let router_id = router.id().to_string();
        // Set up router event handlers
        self.setup_router_handlers(&router, &room_id);

        // Store router info
        let router_info = RouterInfo {
            router,
            worker_id,
            _load_reservation: load_reservation,
        };
        let mut routers = self.routers.write().await;
        // Another caller may have registered this room while native creation
        // was pending. Never replace its router or retain the losing load slot.
        match routers.entry(room_id.clone()) {
            Entry::Occupied(_) => {
                return Err(MediaError::RouterError(format!(
                    "Router already exists for room: {room_id}"
                )));
            }
            Entry::Vacant(entry) => {
                entry.insert(router_info);
            }
        }

        info!(
            "Created router {} for room {} on worker {}",
            router_id, room_id, worker_id
        );

        Ok(router_id)
    }

    /// Gets a router for a room
    pub async fn get_router(&self, room_id: &str) -> MediaResult<Router> {
        let routers = self.routers.read().await;

        routers
            .get(room_id)
            .map(|info| info.router.clone())
            .ok_or_else(|| MediaError::RouterError(format!("Router not found for room: {room_id}")))
    }

    /// Gets the worker ID for a room's router
    pub async fn get_worker_id(&self, room_id: &str) -> MediaResult<WorkerId> {
        let routers = self.routers.read().await;
        routers
            .get(room_id)
            .map(|info| info.worker_id)
            .ok_or_else(|| MediaError::RouterError(format!("Router not found for room: {room_id}")))
    }

    /// Gets the consumer counter for the worker hosting a room's router
    pub async fn get_consumer_counter_for_room(
        &self,
        room_id: &str,
    ) -> MediaResult<Option<Arc<AtomicUsize>>> {
        let routers = self.routers.read().await;
        match routers.get(room_id) {
            Some(info) => Ok(self.worker_manager.get_consumer_counter(info.worker_id)),
            None => Err(MediaError::RouterError(format!(
                "Router not found for room: {room_id}"
            ))),
        }
    }

    /// The consumer counter of the worker that hosts a participant's receive
    /// transport: its viewer worker when placed there, else the room's worker.
    pub async fn get_consumer_counter_for_participant(
        &self,
        room_id: &str,
        participant_id: &str,
    ) -> MediaResult<Option<Arc<AtomicUsize>>> {
        match self.worker_for_participant(participant_id) {
            Some(worker_id) => Ok(self.worker_manager.get_consumer_counter(worker_id)),
            None => self.get_consumer_counter_for_room(room_id).await,
        }
    }

    /// Removes a room's router entry and immediately releases its allocation load.
    /// Other callers may still hold native router handles; the count tracks
    /// registered and pending rooms, not the lifetime of every cloned handle.
    /// The room's viewer routers and pipes go with it.
    pub async fn remove_router(&self, room_id: &str) -> MediaResult<()> {
        let removed = {
            let mut routers = self.routers.write().await;
            routers.remove(room_id)
        };
        let viewer_routers = self.viewer_routers.lock().await.remove(room_id);

        if let Some(router_info) = removed {
            // The router handle and its load reservation are released together.

            info!(
                "Removed router for room {} from worker {} with {} viewer routers",
                room_id,
                router_info.worker_id,
                viewer_routers.map_or(0, |viewers| viewers.by_worker.len())
            );
            Ok(())
        } else {
            Err(MediaError::RouterError(format!(
                "Router not found for room: {room_id}"
            )))
        }
    }

    /// Rooms whose router lives on this worker; their media went with it.
    pub async fn rooms_on_worker(&self, worker_id: WorkerId) -> Vec<String> {
        self.routers
            .read()
            .await
            .iter()
            .filter(|(_, info)| info.worker_id == worker_id)
            .map(|(room_id, _)| room_id.clone())
            .collect()
    }

    /// Checks if a router exists for a room
    pub async fn has_router(&self, room_id: &str) -> bool {
        self.routers.read().await.contains_key(room_id)
    }

    /// Gets the number of active routers
    pub async fn router_count(&self) -> usize {
        self.routers.read().await.len()
    }

    /// Gets router statistics for a room
    pub async fn get_router_stats(&self, room_id: &str) -> MediaResult<RouterDump> {
        let router = self.get_router(room_id).await?;

        router
            .dump()
            .await
            .map_err(|e| MediaError::RouterError(format!("Failed to get router stats: {e}")))
    }

    /// Sets up event handlers for a router
    fn setup_router_handlers(&self, router: &Router, room_id: &str) {
        let room_id = room_id.to_string();

        // .detach() — dropping the HandlerId would unregister the callback immediately
        router
            .on_close({
                let room_id = room_id.clone();
                move || {
                    // mediasoup fires this for ordinary final-handle Drop as
                    // well as worker closure. The distinct worker-close event
                    // below retains its warning; worker death is logged as an error.
                    debug!("Router closed for room: {}", room_id);
                }
            })
            .detach();

        router
            .on_worker_close({
                move || {
                    warn!("Worker closed for router in room: {}", room_id);
                }
            })
            .detach();
    }

    /// Gets all active room IDs
    pub async fn get_active_rooms(&self) -> Vec<String> {
        self.routers.read().await.keys().cloned().collect()
    }

    /// Drops every registered router entry and releases its allocation load.
    /// Callers must quiesce room creation first: draining the map does not fence
    /// creation calls that are still awaiting native work or map insertion.
    pub async fn close_all(&self) -> Result<()> {
        info!("Closing all routers");

        // Pipes and viewer routers close before the primary routers they hang off.
        self.viewer_routers.lock().await.clear();
        let mut routers = self.routers.write().await;

        // Routers are automatically closed when dropped
        for (room_id, _router_info) in routers.drain() {
            debug!("Closed router for room: {}", room_id);
        }

        Ok(())
    }
}

#[cfg(test)]
#[path = "placement_tests.rs"]
mod placement_tests;

#[cfg(test)]
#[path = "allocation_tests.rs"]
mod allocation_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::config::MediaConfig;

    #[tokio::test]
    async fn test_router_lifecycle() {
        let reservation = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let mut config = MediaConfig::default();
        config.worker_config.num_workers = 1;
        config.webrtc_server_port_base = reservation.local_addr().unwrap().port();
        drop(reservation);
        let config = Arc::new(config);
        let worker_manager = Arc::new(WorkerManager::new(config.clone()).await.unwrap());
        let router_manager = RouterManager::new(worker_manager);

        // Create router
        let room_id = "test-room".to_string();
        let result = router_manager
            .create_router(room_id.clone(), config.router_config.clone())
            .await;
        assert!(result.is_ok());

        // Check if router exists
        assert!(router_manager.has_router(&room_id).await);

        // Remove router
        let result = router_manager.remove_router(&room_id).await;
        assert!(result.is_ok());

        // Check if router was removed
        assert!(!router_manager.has_router(&room_id).await);
    }
}
