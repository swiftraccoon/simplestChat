//! Non-blocking participant and broadcaster counts for the directory and metrics.

use super::super::*;

impl RoomManager {
    /// Gets current room count
    pub(crate) async fn room_count(&self) -> usize {
        self.rooms.read().unwrap_or_else(|e| e.into_inner()).len()
    }

    /// Count memberships (including disconnected grace sessions) only if every
    /// room in the map snapshot is readable. `Some(0)` means known empty; `None`
    /// means a room was write-locked. This avoids waiting on room operations but
    /// is not a globally atomic snapshot across independently changing rooms.
    pub(crate) async fn try_total_participant_count(&self) -> Option<usize> {
        let (total, complete) = self.participant_count_snapshot();
        complete.then_some(total)
    }

    pub(in crate::room) fn participant_count_snapshot(&self) -> (usize, bool) {
        let room_locks: Vec<Arc<TokioRwLock<Room>>> = {
            let rooms = self.rooms.read().unwrap_or_else(|e| e.into_inner());
            rooms.values().cloned().collect()
        };
        Self::count_readable_participants(&room_locks)
    }

    pub(in crate::room) fn count_readable_participants(
        room_locks: &[Arc<TokioRwLock<Room>>],
    ) -> (usize, bool) {
        let mut total = 0;
        let mut complete = true;
        for room_lock in room_locks {
            if let Ok(room) = room_lock.try_read() {
                total += room.participants.len();
            } else {
                complete = false;
            }
        }
        (total, complete)
    }

    /// Participant count for a room, or `None` when the room is not live or its
    /// state lock is held by a writer. Callers must not render `None` as empty:
    /// under load the busiest rooms are exactly the ones that are locked.
    pub(crate) fn participant_count_for_room(&self, room_id: &RoomId) -> Option<usize> {
        let rooms = self.rooms.read().unwrap_or_else(|e| e.into_inner());
        rooms
            .get(room_id)
            .and_then(Self::readable_participant_count)
    }

    pub(crate) fn readable_participant_count(lock: &Arc<TokioRwLock<Room>>) -> Option<usize> {
        lock.try_read().ok().map(|room| room.participants.len())
    }

    /// Every live room with its readable participant count (`None` while a
    /// writer holds the room), for directory ordering. Empty rooms are not live.
    pub(crate) fn live_room_counts(&self) -> Vec<(RoomId, Option<usize>)> {
        let rooms = self.rooms.read().unwrap_or_else(|e| e.into_inner());
        Self::live_counts(rooms.iter())
    }

    pub(crate) fn live_counts<'a>(
        rooms: impl Iterator<Item = (&'a RoomId, &'a Arc<TokioRwLock<Room>>)>,
    ) -> Vec<(RoomId, Option<usize>)> {
        rooms
            .filter_map(|(id, lock)| match Self::readable_participant_count(lock) {
                Some(0) => None,
                count => Some((id.clone(), count)),
            })
            .collect()
    }

    pub(crate) fn readable_broadcaster_count(lock: &Arc<TokioRwLock<Room>>) -> Option<usize> {
        lock.try_read().ok().map(|room| {
            room.participants
                .values()
                .filter(|participant| !participant.producers.is_empty())
                .count()
        })
    }
}
