//! Trailing-window admission budgets: each room keeps one per kind of work it
//! admits, so a burst from one participant cannot starve the room.

use crate::limits::Budget;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// At most `capacity` reservations inside the trailing `window`. A reservation
/// taken for work that then never ran can be refunded, so a refused or rolled
/// back operation does not spend the room's budget.
#[derive(Debug, Clone)]
pub(crate) struct SlidingWindow {
    window: Duration,
    capacity: usize,
    reserved: VecDeque<Instant>,
}

impl SlidingWindow {
    pub(crate) const fn new(window: Duration, capacity: usize) -> Self {
        Self {
            window,
            capacity,
            reserved: VecDeque::new(),
        }
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.reserved.len()
    }

    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.reserved.is_empty()
    }
}

impl Budget for SlidingWindow {
    /// Admits an event at `now` when the window still has room, and remembers it.
    fn reserve(&mut self, now: Instant) -> bool {
        while self
            .reserved
            .front()
            .is_some_and(|reserved_at| now.duration_since(*reserved_at) >= self.window)
        {
            self.reserved.pop_front();
        }
        if self.reserved.len() >= self.capacity {
            return false;
        }
        self.reserved.push_back(now);
        true
    }

    /// Forgets one reservation taken at `reserved_at`, when one remains.
    fn refund(&mut self, reserved_at: Instant) {
        if let Some(position) = self.reserved.iter().position(|entry| *entry == reserved_at) {
            self.reserved.remove(position);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_window_refuses_until_the_oldest_reservation_ages_out() {
        let mut window = SlidingWindow::new(Duration::from_secs(1), 2);
        let start = Instant::now();
        assert!(window.reserve(start));
        assert!(window.reserve(start));
        assert!(!window.reserve(start + Duration::from_millis(999)));
        // Both reservations age out together, so the window holds only the new one.
        assert!(window.reserve(start + Duration::from_secs(1)));
        assert_eq!(window.len(), 1);
    }

    #[test]
    fn a_refund_returns_exactly_one_matching_reservation() {
        let mut window = SlidingWindow::new(Duration::from_secs(1), 2);
        let start = Instant::now();
        assert!(window.reserve(start));
        assert!(window.reserve(start));
        assert!(!window.reserve(start));
        window.refund(start);
        assert_eq!(window.len(), 1);
        // No reservation was taken at this instant, so nothing is forgotten.
        window.refund(start + Duration::from_millis(1));
        assert_eq!(window.len(), 1);
        window.refund(start);
        assert!(window.is_empty());
        window.refund(start);
        assert!(window.is_empty());
        assert!(window.reserve(start));
    }
}
