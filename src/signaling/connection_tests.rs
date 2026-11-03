//! What a command's failure looks like on the wire: a refusal keeps its own
//! code and text, a server failure answers a fixed text, and a social command's
//! frame keeps the identifiers the client correlates it by.

use super::heartbeat_tests::Fixture;
use super::*;
use crate::ids::fixtures::pid;
use serde_json::json;

/// A chat send or social request answers a refusal with a `socialError` that
/// carries the client message ID or request ID it came with, and the refusal's
/// own code and text; any other command has no social frame and answers the
/// plain error frame.
#[test]
fn social_errors_keep_request_and_client_message_ids() {
    let error = RoomError::from(Refusal::RateLimited("Room chat rate limit exceeded".into()));
    let cases = [
        (
            json!({"type": "chatMessage", "content": "hello", "clientMessageId": "public-1"}),
            json!({"clientMessageId": "public-1"}),
        ),
        (
            json!({
                "type": "privateMessage", "targetParticipantId": pid("bob"),
                "content": "hello", "clientMessageId": "private-1",
            }),
            json!({"clientMessageId": "private-1"}),
        ),
        (
            json!({
                "type": "retryChatMessage", "clientMessageId": "retry-1", "sequence": 1,
                "chatSessionId": "session", "content": "hello again",
            }),
            json!({"clientMessageId": "retry-1"}),
        ),
        (
            json!({"type": "getPinnedMessages", "requestId": "pins-1"}),
            json!({"requestId": "pins-1"}),
        ),
    ];
    for (command, identifiers) in cases {
        let command: ClientMessage = serde_json::from_value(command).unwrap();
        let mut expected = json!({
            "type": "socialError",
            "code": "rate_limited",
            "message": "Room chat rate limit exceeded",
        });
        expected
            .as_object_mut()
            .unwrap()
            .extend(identifiers.as_object().unwrap().clone());
        let frame = social_error_frame(&command, &error).expect("a social command's frame");
        assert_eq!(
            serde_json::to_value(frame).unwrap(),
            expected,
            "{command:?}"
        );
    }
    for command in [
        json!({"type": "chatMessage", "content": "uncorrelated"}),
        json!({"type": "createSendTransport"}),
        json!({"type": "kick", "targetParticipantId": pid("bob")}),
    ] {
        let command: ClientMessage = serde_json::from_value(command).unwrap();
        assert!(
            social_error_frame(&command, &error).is_none(),
            "{command:?}"
        );
    }
}

/// What a database, media or internal failure says never reaches a client:
/// the plain and the social frame both carry only the fixed text of its class.
#[test]
fn server_failure_detail_never_reaches_a_frame() {
    let detail = "the rooms row of secret-room and its password_hash";
    let command: ClientMessage =
        serde_json::from_value(json!({"type": "getPinnedMessages", "requestId": "pins-1"}))
            .unwrap();
    for (error, code, message) in [
        (
            RoomError::Database(sqlx::Error::Protocol(detail.into())),
            "unavailable",
            "Service temporarily unavailable; try again",
        ),
        (
            RoomError::Media(crate::media::MediaError::RouterError(detail.into())),
            "request_failed",
            "Request could not be completed",
        ),
        (
            RoomError::Internal(anyhow::anyhow!("{detail}")),
            "request_failed",
            "Request could not be completed",
        ),
    ] {
        let plain = serde_json::to_value(error_frame(&error)).unwrap();
        assert_eq!(
            plain,
            json!({"type": "error", "code": code, "message": message})
        );
        let social = serde_json::to_value(social_error_frame(&command, &error).unwrap()).unwrap();
        assert_eq!(
            social,
            json!({"type": "socialError", "requestId": "pins-1", "code": code, "message": message}),
        );
        for frame in [plain, social] {
            assert!(!frame.to_string().contains("secret-room"), "{frame}");
        }
    }
}

/// Refusals from the dispatcher and from the room manager reach the client with
/// the code and text the refusal carries, on commands that are not social.
#[tokio::test]
async fn a_refused_command_answers_with_its_own_code_and_text() {
    let mut fixture = Fixture::new().await;
    fixture
        .send(json!({"type":"createSendTransport","requestId":"unjoined"}))
        .await
        .unwrap();
    assert_eq!(
        fixture.next_message().await.unwrap(),
        json!({
            "type": "error",
            "code": "forbidden",
            "message": "Not in a room",
            "requestId": "unjoined",
        }),
    );
    fixture.join().await.unwrap();
    fixture
        .send(json!({"type":"setTopic","topic":"bell\u{7}","requestId":"topic"}))
        .await
        .unwrap();
    assert_eq!(
        fixture.next_message().await.unwrap(),
        json!({
            "type": "error",
            "code": "invalid",
            "message": "Room topic must be at most 512 bytes without control characters",
            "requestId": "topic",
        }),
    );
    fixture
        .send(json!({
            "type": "ban",
            "targetParticipantId": Uuid::new_v4(),
            "reason": "é".repeat(129),
            "requestId": "ban",
        }))
        .await
        .unwrap();
    assert_eq!(
        fixture.next_message().await.unwrap(),
        json!({
            "type": "error",
            "code": "invalid",
            "message": "Reason must be at most 256 bytes without control characters",
            "requestId": "ban",
        }),
    );
    fixture.finish().await;
}

/// Thirty-three characters are sixty-six bytes: a join name's limit counts
/// bytes, and so does the refusal's text.
#[tokio::test]
async fn a_join_name_over_its_byte_limit_is_refused_in_bytes() {
    let mut fixture = Fixture::new().await;
    fixture
        .send(json!({
            "type": "joinRoom", "roomId": "heartbeat-test", "participantName": "é".repeat(33)
        }))
        .await
        .unwrap();
    assert_eq!(
        fixture.next_message().await.unwrap(),
        json!({
            "type": "error",
            "code": "invalid",
            "message": "Name must be 1-64 bytes of plain text; \"You\" is taken",
        }),
    );
    fixture.finish().await;
}
