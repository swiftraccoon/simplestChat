//! Trailing-window admission budgets: each room keeps one per kind of work it
//! admits, so a burst from one participant cannot starve the room.

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

    /// Admits an event at `now` when the window still has room, and remembers it.
    pub(crate) fn reserve(&mut self, now: Instant) -> bool {
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

    /// Forgets one reservation taken at `reserved_at`; false when none remains.
    pub(crate) fn refund(&mut self, reserved_at: Instant) -> bool {
        let Some(position) = self.reserved.iter().position(|entry| *entry == reserved_at) else {
            return false;
        };
        self.reserved.remove(position);
        true
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
        let mut window = SlidingWindow::new(Duration::from_secs(1), 1);
        let start = Instant::now();
        assert!(window.reserve(start));
        assert!(!window.reserve(start));
        assert!(window.refund(start));
        assert!(!window.refund(start));
        assert!(window.is_empty());
        assert!(window.reserve(start));
    }
}
