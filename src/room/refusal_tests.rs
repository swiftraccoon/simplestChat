use super::*;

#[test]
fn every_refusal_has_one_error_code() {
    let refusals = [
        Refusal::NotFound("a".into()),
        Refusal::Unauthorized("b".into()),
        Refusal::Forbidden("c".into()),
        Refusal::Invalid("d".into()),
        Refusal::Conflict("e".into()),
        Refusal::RateLimited("f".into()),
        Refusal::Unavailable("g".into()),
        Refusal::TimedOut("h".into()),
        Refusal::Internal("i".into()),
    ];
    for refusal in refusals {
        // No wildcard: a new refusal does not compile until it is given its
        // socket code and HTTP status here.
        let (code, status) = match &refusal {
            Refusal::NotFound(_) => (ErrorCode::NotFound, StatusCode::NOT_FOUND),
            Refusal::Unauthorized(_) => (ErrorCode::Forbidden, StatusCode::UNAUTHORIZED),
            Refusal::Forbidden(_) => (ErrorCode::Forbidden, StatusCode::FORBIDDEN),
            Refusal::Invalid(_) => (ErrorCode::Invalid, StatusCode::BAD_REQUEST),
            Refusal::Conflict(_) => (ErrorCode::Invalid, StatusCode::CONFLICT),
            Refusal::RateLimited(_) => (ErrorCode::RateLimited, StatusCode::TOO_MANY_REQUESTS),
            Refusal::Unavailable(_) => (ErrorCode::Unavailable, StatusCode::SERVICE_UNAVAILABLE),
            Refusal::TimedOut(_) => (ErrorCode::TimedOut, StatusCode::SERVICE_UNAVAILABLE),
            Refusal::Internal(_) => (ErrorCode::RequestFailed, StatusCode::INTERNAL_SERVER_ERROR),
        };
        assert_eq!(refusal.code(), code, "{refusal:?}");
        assert_eq!(refusal.status(), status, "{refusal:?}");
        assert_eq!(refusal.to_string(), refusal.message());
        // Raised as a room error, a refusal keeps its code, text and status.
        let error = RoomError::from(refusal.clone());
        assert_eq!(error.code(), code, "{refusal:?}");
        assert_eq!(error.client_message(), refusal.message());
        assert_eq!(error.into_refusal(), refusal);
    }
}

/// A failure that is not a refusal answers a fixed text, whatever its detail
/// says; the detail stays in `Display`, which only the server log reads.
#[test]
fn database_and_media_failures_map_to_their_codes() {
    let detail = "password_hash for room secret-room";
    let cases = [
        (
            RoomError::Database(sqlx::Error::Protocol(detail.into())),
            ErrorCode::Unavailable,
            StatusCode::SERVICE_UNAVAILABLE,
            "Service temporarily unavailable; try again",
        ),
        (
            RoomError::Media(MediaError::TransportError(detail.into())),
            ErrorCode::RequestFailed,
            StatusCode::INTERNAL_SERVER_ERROR,
            "Request could not be completed",
        ),
        (
            RoomError::Internal(anyhow::anyhow!("{detail}")),
            ErrorCode::RequestFailed,
            StatusCode::INTERNAL_SERVER_ERROR,
            "Request could not be completed",
        ),
    ];
    for (error, code, status, message) in cases {
        assert_eq!(error.code(), code, "{error:?}");
        assert_eq!(error.client_message(), message, "{error:?}");
        assert!(error.to_string().contains(detail), "{error}");
        // A route that answers refusals gives the same code and text.
        let refusal = error.into_refusal();
        assert_eq!(refusal.code(), code);
        assert_eq!(refusal.status(), status);
        assert_eq!(refusal.message(), message);
    }
    let challenge = RoomError::PasswordRequired;
    assert_eq!(challenge.code(), ErrorCode::Forbidden);
    assert_eq!(challenge.client_message(), "This room requires a password");
    let refusal = challenge.into_refusal();
    assert_eq!(refusal.code(), ErrorCode::Forbidden);
    assert_eq!(refusal.status(), StatusCode::FORBIDDEN);
    assert_eq!(refusal.message(), "This room requires a password");
}

/// `Display` is the whole report and no variant names a source, so a reporter
/// that walks the chain, as `anyhow` does, prints each cause once.
#[test]
fn a_room_error_reports_each_cause_once() {
    let internal = RoomError::Internal(
        anyhow::anyhow!("worker exited").context("media server could not start"),
    );
    assert_eq!(
        internal.to_string(),
        "media server could not start: worker exited"
    );
    let reported = anyhow::Error::from(internal);
    assert_eq!(
        format!("{reported:#}"),
        "media server could not start: worker exited"
    );
    assert_eq!(reported.chain().count(), 1);
    for error in [
        RoomError::from(Refusal::NotFound("Room not found".into())),
        RoomError::PasswordRequired,
        RoomError::from(sqlx::Error::PoolClosed),
        RoomError::Media(MediaError::TransportError("fixture".into())),
        RoomError::Internal(anyhow::anyhow!("fixture")),
    ] {
        assert!(std::error::Error::source(&error).is_none(), "{error:?}");
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

/// Both hashing paths answered with these exact texts before they shared one site.
#[test]
fn password_hash_errors_keep_their_messages_and_refuse_as_unavailable() {
    for (error, message) in [
        (
            PasswordHashError::Busy,
            "Password service is busy; try again",
        ),
        (PasswordHashError::Failed, "Password update failed"),
    ] {
        assert_eq!(error.to_string(), message);
        let RoomError::Refused(refusal) = RoomError::from(error) else {
            panic!("{error:?} must be a refusal");
        };
        assert_eq!(refusal, Refusal::Unavailable(message.into()));
        assert_eq!(refusal.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(refusal.code(), ErrorCode::Unavailable);
    }
}
