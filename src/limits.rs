//! Per-owner rate limits, and the limiters whose shapes keep them apart.
//!
//! [`Budget`] is the contract of a refundable per-owner budget. One owner holds
//! it exclusively and reserves before the work runs; a reservation whose work
//! never ran can be refunded, so a refused or rolled-back operation does not
//! spend the owner's budget. Each implementation sits beside its owner, whose
//! exclusive access is what serializes it:
//!
//! - [`SlidingWindow`](crate::room::window::SlidingWindow) in `room/window.rs`:
//!   the trailing windows a room keeps for chat broadcasts, administration,
//!   media mutations, media control requests and voice requests, changed only
//!   under the room's write lock.
//! - [`TokenBucket`](crate::signaling::connection::token_bucket::TokenBucket) in
//!   `signaling/connection_token_bucket.rs`: the refilling capacities a
//!   connection owns for its frames, chat, administration and media commands;
//!   the media buckets pass to a reconnecting socket with the session they
//!   belong to.
//!
//! Three limiters keep shapes of their own and stay outside the trait:
//!
//! - The keyed approximate account sketch, `PrincipalRateLimiter` in
//!   `signaling/mod.rs`, estimates each account's requests in a fixed window
//!   with a count-min sketch shared through the server state, updated
//!   conservatively: only the cells at an account's current minimum rise. A
//!   refund cannot know which cells its reservation raised, so any decrement
//!   could lower a colliding account's estimate below its true count, and
//!   colluding accounts could then pass their limit. It stays beside the
//!   server state and the guards that call it, and shares its window with the
//!   per-address request guard there.
//! - The sign-in penalty box, [`FailureLimiter`](accounts::FailureLimiter),
//!   reserves nothing before a sign-in or recovery redemption: a failure is
//!   charged after the check and, past three forgiven ones, earns a doubling
//!   wait for that address cohort and account, which a success clears.
//! - The per-address registration window,
//!   [`WindowLimiter`](accounts::WindowLimiter), is a fixed window per address
//!   cohort shared by every request. A registration attempt spends one of its
//!   answers before the email is looked up, so an attempt refused as taken
//!   spends one too: that answer tells whether an address has an account, and
//!   nothing is handed back.
//!
//! The last two live in [`accounts`]: both guard the account routes, keep their
//! records by address cohort (the penalty box per account too) in tables of one
//! bounded size, and limit rather than authenticate.

pub(crate) mod accounts;

use std::time::Instant;

/// A refundable budget held by one owner: a room or a connection.
pub(crate) trait Budget {
    /// Admits one unit of work at `now` when the budget has room, and spends it.
    fn reserve(&mut self, now: Instant) -> bool;

    /// Hands back the reservation taken at `reserved_at` for work that never ran.
    /// A caller refunds only a reservation it took; no budget rises past the
    /// capacity it was built with.
    fn refund(&mut self, reserved_at: Instant);
}

#[cfg(test)]
#[path = "limits_tests.rs"]
mod tests;
