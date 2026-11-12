//! The public lobby's seeding transaction. A deployment's lobby belongs to
//! one retained owner account; this library signs that owner in through the
//! private loopback backend, creating the account on first use, creates the
//! lobby only when the owner has none, and ends its session. It never
//! changes an existing room and never prints or returns a credential.
//!
//! The host command that retains the owner's credentials runs it, and the
//! server's own tests drive the same code against the real router.

mod client;
mod cookie;
mod json;
mod response;
#[cfg(any(test, feature = "testing"))]
pub mod testing;
#[cfg(test)]
mod tests;
mod transaction;

pub use json::{DUPLICATE, INVALID, Json, parse};
pub use transaction::{
    Owner, ROOM_REQUEST, Report, authentication, created_lobby, owned_lobby, seed, token_shape,
};

use std::time::{Duration, Instant};

/// The backend's refresh cookie: host-only, secure, for the whole site.
pub const COOKIE: &str = "__Host-refresh_token";
/// The length of an issued access token.
pub const TOKEN_CHARACTERS: usize = 43;
/// The most bytes a successful answer's body may hold.
pub const MAX_RESPONSE: usize = 256 * 1024;
/// The code for a backend that is absent, silent or gone.
pub const UNAVAILABLE: &str = "private_api_unavailable";
/// The code for an operation that ran out of time.
pub const DEADLINE: &str = "operation_deadline";
/// The code for every failure whose cause has no name of its own.
pub const FAILED: &str = "seed_operation_failed";

/// How long a seeding may take.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// The longest wait for any one step of a request.
    pub request: Duration,
    /// How long ending the session may take once everything else is over.
    pub farewell: Duration,
    /// When everything but ending the session must be over.
    pub until: Instant,
}

impl Limits {
    /// A request step's five seconds, eight to end the session, and
    /// `whole` from now for everything before that.
    pub fn within(whole: Duration) -> Self {
        Self {
            request: Duration::from_secs(5),
            farewell: Duration::from_secs(8),
            until: Instant::now() + whole,
        }
    }
}
