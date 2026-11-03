//! Why a room operation failed, stated once.
//!
//! The room, social and media layers fail with a [`RoomError`]: a [`Refusal`]
//! with a client-safe message, the password challenge, or a database, media or
//! internal failure whose detail only the server log sees. The socket turns it
//! into an [`ErrorCode`] and a message, the HTTP handlers into a
//! [`StatusCode`]. Nothing classifies failures by their text.

use crate::media::MediaError;
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
    /// The operation definitely failed, not for want of a dependency, and the
    /// client may be told so in a fixed message, such as "Password
    /// verification failed": nothing the client sent was wrong. A failure
    /// whose detail must stay in the log is a [`RoomError::Internal`] instead.
    Internal(Cow<'static, str>),
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
            | Self::TimedOut(message)
            | Self::Internal(message) => message,
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
            Self::Internal(_) => ErrorCode::RequestFailed,
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
            Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for Refusal {}

/// What a client hears about the password challenge outside its own frame.
const PASSWORD_REQUIRED: &str = "This room requires a password";
/// What a client hears about a database failure.
const DATABASE_FAILURE: &str = "Service temporarily unavailable; try again";
/// What a client hears about a media or internal failure.
const SERVER_FAILURE: &str = "Request could not be completed";

/// What a room, social or media operation fails with. Only a refusal's own
/// message reaches a client; every other variant answers a fixed text and
/// keeps its detail, through [`Display`](std::fmt::Display), for the server
/// log.
///
/// `Display` is the whole report: a refusal's text, or the wrapped error's
/// detail, an anyhow error's chain included. No variant names a
/// [`source`](std::error::Error::source), so a reporter that walks the chain,
/// as `anyhow` does for `main`, prints each cause once.
#[derive(Debug)]
pub(crate) enum RoomError {
    /// A refusal the client can act on: its code and message are sent as
    /// they are.
    Refused(Refusal),
    /// The room has a password and the join supplied none: the client is
    /// asked for it (`roomPasswordRequired`) rather than refused.
    PasswordRequired,
    /// The database failed; the client hears that the service is unavailable.
    Database(sqlx::Error),
    /// The media layer failed; the client hears that the request failed.
    Media(MediaError),
    /// The server could not do its own part (a task, a serialization, an
    /// outbound queue, a startup or shutdown step); the client hears that the
    /// request failed. Raised explicitly where the failure starts, never by
    /// converting an arbitrary error.
    Internal(anyhow::Error),
}

impl RoomError {
    /// The socket's error code: a refusal's own, `forbidden` for the password
    /// challenge, `unavailable` for a database failure and `request_failed`
    /// for a media or internal one.
    pub(crate) fn code(&self) -> ErrorCode {
        match self {
            Self::Refused(refusal) => refusal.code(),
            Self::PasswordRequired => ErrorCode::Forbidden,
            Self::Database(_) => ErrorCode::Unavailable,
            Self::Media(_) | Self::Internal(_) => ErrorCode::RequestFailed,
        }
    }

    /// The text a client may be shown: a refusal's own message, otherwise a
    /// fixed one that names nothing of the failure.
    pub(crate) fn client_message(&self) -> &str {
        match self {
            Self::Refused(refusal) => refusal.message(),
            Self::PasswordRequired => PASSWORD_REQUIRED,
            Self::Database(_) => DATABASE_FAILURE,
            Self::Media(_) | Self::Internal(_) => SERVER_FAILURE,
        }
    }

    /// The refusal a client is given in place of this error: a refusal as it
    /// is, any other error as the fixed refusal with the same code and text,
    /// and an HTTP status of `403` for the password challenge, `503` for a
    /// database failure and `500` for a media or internal one, so a route that
    /// answers refusals can answer every room error.
    pub(crate) fn into_refusal(self) -> Refusal {
        match self {
            Self::Refused(refusal) => refusal,
            Self::PasswordRequired => Refusal::Forbidden(PASSWORD_REQUIRED.into()),
            Self::Database(_) => Refusal::Unavailable(DATABASE_FAILURE.into()),
            Self::Media(_) | Self::Internal(_) => Refusal::Internal(SERVER_FAILURE.into()),
        }
    }
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

impl From<MediaError> for RoomError {
    fn from(error: MediaError) -> Self {
        Self::Media(error)
    }
}

impl std::fmt::Display for RoomError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(refusal) => refusal.fmt(f),
            Self::PasswordRequired => f.write_str(PASSWORD_REQUIRED),
            Self::Database(error) => error.fmt(f),
            Self::Media(error) => error.fmt(f),
            Self::Internal(error) => write!(f, "{error:#}"),
        }
    }
}

/// `Display` already carries every cause, so `source` stays `None`.
impl std::error::Error for RoomError {}

/// Why a room password could not be hashed. `Busy` clears once the hashing permit is free;
/// `Failed` is a job failure (a panicked hashing job or an Argon2 error) that the caller may retry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PasswordHashError {
    /// Another room password holds the single hashing permit; nothing queues for it.
    Busy,
    /// The hashing job panicked or Argon2 failed.
    Failed,
}

impl PasswordHashError {
    /// The client-safe explanation.
    fn message(self) -> &'static str {
        match self {
            Self::Busy => "Password service is busy; try again",
            Self::Failed => "Password update failed",
        }
    }
}

impl std::fmt::Display for PasswordHashError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for PasswordHashError {}

impl From<PasswordHashError> for RoomError {
    fn from(error: PasswordHashError) -> Self {
        Refusal::Unavailable(error.message().into()).into()
    }
}

#[cfg(test)]
#[path = "refusal_tests.rs"]
mod tests;
