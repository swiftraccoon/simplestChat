//! Why a room operation was refused, stated once.
//!
//! The persistence layer and the room manager raise a [`Refusal`] with a
//! client-safe message; the socket turns it into an [`ErrorCode`] and the HTTP
//! handlers into a [`StatusCode`]. Nothing classifies refusals by their text.

use crate::signaling::protocol::ErrorCode;
use axum::http::StatusCode;
use std::borrow::Cow;

/// A refusal the client can act on; the message is safe to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// The addressed record is gone or was never there.
    NotFound(Cow<'static, str>),
    /// The caller's sign-in no longer authorizes the operation.
    Unauthorized(Cow<'static, str>),
    /// The caller may not perform the operation.
    Forbidden(Cow<'static, str>),
    /// The request itself is wrong.
    Invalid(Cow<'static, str>),
    /// The request conflicts with current state or a quota the caller owns.
    Conflict(Cow<'static, str>),
    /// The caller exceeded a rate limit.
    RateLimited(Cow<'static, str>),
    /// A dependency the operation needs is not available right now.
    Unavailable(Cow<'static, str>),
    /// The operation did not finish within its deadline.
    TimedOut(Cow<'static, str>),
}

impl Refusal {
    /// The client-safe explanation.
    pub(crate) fn message(&self) -> &str {
        match self {
            Self::NotFound(message)
            | Self::Unauthorized(message)
            | Self::Forbidden(message)
            | Self::Invalid(message)
            | Self::Conflict(message)
            | Self::RateLimited(message)
            | Self::Unavailable(message)
            | Self::TimedOut(message) => message,
        }
    }

    /// The socket's error code for this refusal.
    pub(crate) fn code(&self) -> ErrorCode {
        match self {
            Self::NotFound(_) => ErrorCode::NotFound,
            Self::Unauthorized(_) | Self::Forbidden(_) => ErrorCode::Forbidden,
            Self::Invalid(_) | Self::Conflict(_) => ErrorCode::Invalid,
            Self::RateLimited(_) => ErrorCode::RateLimited,
            Self::Unavailable(_) => ErrorCode::Unavailable,
            Self::TimedOut(_) => ErrorCode::TimedOut,
        }
    }

    /// The HTTP status for this refusal.
    pub(crate) fn status(&self) -> StatusCode {
        match self {
            Self::NotFound(_) => StatusCode::NOT_FOUND,
            Self::Unauthorized(_) => StatusCode::UNAUTHORIZED,
            Self::Forbidden(_) => StatusCode::FORBIDDEN,
            Self::Invalid(_) => StatusCode::BAD_REQUEST,
            Self::Conflict(_) => StatusCode::CONFLICT,
            Self::RateLimited(_) => StatusCode::TOO_MANY_REQUESTS,
            Self::Unavailable(_) | Self::TimedOut(_) => StatusCode::SERVICE_UNAVAILABLE,
        }
    }
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for Refusal {}

/// What a persistence operation can fail with: a refusal the client can act
/// on, or a database failure that only the operator can.
#[derive(Debug)]
pub(crate) enum RoomError {
    Refused(Refusal),
    Database(sqlx::Error),
}

impl From<Refusal> for RoomError {
    fn from(refusal: Refusal) -> Self {
        Self::Refused(refusal)
    }
}

impl From<sqlx::Error> for RoomError {
    fn from(error: sqlx::Error) -> Self {
        Self::Database(error)
    }
}

impl std::fmt::Display for RoomError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(refusal) => refusal.fmt(f),
            Self::Database(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for RoomError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Refused(refusal) => Some(refusal),
            Self::Database(error) => Some(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Refusal, RoomError};
    use crate::signaling::protocol::ErrorCode;
    use axum::http::StatusCode;

    #[test]
    fn every_refusal_maps_to_one_code_and_one_status() {
        let cases = [
            (
                Refusal::NotFound("a".into()),
                ErrorCode::NotFound,
                StatusCode::NOT_FOUND,
            ),
            (
                Refusal::Unauthorized("b".into()),
                ErrorCode::Forbidden,
                StatusCode::UNAUTHORIZED,
            ),
            (
                Refusal::Forbidden("c".into()),
                ErrorCode::Forbidden,
                StatusCode::FORBIDDEN,
            ),
            (
                Refusal::Invalid("d".into()),
                ErrorCode::Invalid,
                StatusCode::BAD_REQUEST,
            ),
            (
                Refusal::Conflict("e".into()),
                ErrorCode::Invalid,
                StatusCode::CONFLICT,
            ),
            (
                Refusal::RateLimited("f".into()),
                ErrorCode::RateLimited,
                StatusCode::TOO_MANY_REQUESTS,
            ),
            (
                Refusal::Unavailable("g".into()),
                ErrorCode::Unavailable,
                StatusCode::SERVICE_UNAVAILABLE,
            ),
            (
                Refusal::TimedOut("h".into()),
                ErrorCode::TimedOut,
                StatusCode::SERVICE_UNAVAILABLE,
            ),
        ];
        for (refusal, code, status) in cases {
            assert_eq!(refusal.code(), code, "{refusal:?}");
            assert_eq!(refusal.status(), status, "{refusal:?}");
            assert_eq!(refusal.to_string(), refusal.message());
        }
    }

    #[test]
    fn database_failures_and_refusals_stay_distinguishable() {
        let refused = RoomError::from(Refusal::NotFound("Member not found in this room".into()));
        assert!(matches!(refused, RoomError::Refused(Refusal::NotFound(_))));
        assert_eq!(refused.to_string(), "Member not found in this room");
        let database = RoomError::from(sqlx::Error::PoolClosed);
        assert!(matches!(
            database,
            RoomError::Database(sqlx::Error::PoolClosed)
        ));
    }
}
