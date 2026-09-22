//! Exercise correlation through the real WebSocket dispatcher and native worker.

use super::heartbeat_tests::Fixture;
use super::*;
use serde_json::{Value, json};

#[tokio::test]
async fn correlated_media_replies_keep_notifications_outside_the_request_envelope() {
    let mut fixture = Fixture::new().await;
    let joined = fixture.join().await.unwrap();
    assert!(joined.get("requestId").is_none());
    let caps = fixture
        .request(
            json!({"type": "getRouterRtpCapabilities", "requestId": "caps"}),
            "routerRtpCapabilities",
        )
        .await
        .unwrap();
    assert_eq!(caps["requestId"], "caps");

    let mut transports = Vec::new();
    for (command, request_id) in [
        ("createSendTransport", "send"),
        ("createRecvTransport", "recv"),
    ] {
        let created = fixture
            .request(
                json!({"type": command, "requestId": request_id}),
                "transportCreated",
            )
            .await
            .unwrap();
        assert_eq!(created["requestId"], request_id);
        let connected = fixture.request(json!({
            "type": "connectTransport", "requestId": format!("connect-{request_id}"),
            "transportId": created["transportId"],
            "dtlsParameters": { "role": "client", "fingerprints": created["dtlsParameters"]["fingerprints"] },
        }), "transportConnected").await.unwrap();
        assert_eq!(connected["requestId"], format!("connect-{request_id}"));
        assert_eq!(connected["transportId"], created["transportId"]);
        transports.push(created["transportId"].clone());
    }
    assert_ne!(transports[0], transports[1]);
    let produced = fixture.request(json!({
        "type": "produce", "requestId": "produce", "transportId": transports[0],
        "kind": "audio", "source": "microphone",
        "rtpParameters": {
            "codecs": [{"mimeType": "audio/opus", "payloadType": 111, "clockRate": 48000, "channels": 2}],
            "headerExtensions": [], "encodings": [{"ssrc": 123456}],
            "rtcp": {"cname": "correlation-test", "reducedSize": true},
        },
    }), "producerCreated").await.unwrap();
    assert_eq!(produced["requestId"], "produce");
    let producer_id = produced["producerId"].clone();
    let paused = fixture.request(json!({"type": "pauseProducer", "requestId": "pause-producer", "producerId": producer_id}), "producerPaused").await.unwrap();
    assert_eq!(paused["requestId"], "pause-producer");

    let consumed = fixture
        .request(
            json!({
                "type": "consume", "requestId": "consume", "producerId": producer_id,
                "rtpCapabilities": caps["rtpCapabilities"],
            }),
            "consumerCreated",
        )
        .await
        .unwrap();
    assert_eq!(consumed["requestId"], "consume");
    assert_eq!(consumed["producerId"], producer_id);
    let notification = fixture.next_message().await.unwrap();
    assert_eq!(
        notification,
        json!({"type": "producerPaused", "producerId": producer_id})
    );
    for (command, response) in [
        ("resumeConsumer", "consumerResumed"),
        ("pauseConsumer", "consumerPaused"),
    ] {
        let reply = fixture.request(json!({"type": command, "requestId": command, "consumerId": consumed["consumerId"]}), response).await.unwrap();
        assert_eq!(reply["requestId"], command);
        assert_eq!(reply["consumerId"], consumed["consumerId"]);
    }
    let resumed = fixture.request(json!({"type": "resumeProducer", "requestId": "resume-producer", "producerId": producer_id}), "producerResumed").await.unwrap();
    assert_eq!(resumed["requestId"], "resume-producer");
    let ice = fixture
        .request(
            json!({"type": "restartIce", "requestId": "ice", "transportId": transports[1]}),
            "iceRestarted",
        )
        .await
        .unwrap();
    assert_eq!(ice["requestId"], "ice");
    assert_eq!(ice["transportId"], transports[1]);
    // Existing native/load clients omit the optional envelope and still work.
    let legacy = fixture
        .request(
            json!({"type": "getRouterRtpCapabilities"}),
            "routerRtpCapabilities",
        )
        .await
        .unwrap();
    assert!(legacy.get("requestId").is_none());
    fixture.finish().await;
}

#[tokio::test]
async fn correlated_dispatch_and_decode_errors_echo_only_valid_request_ids() {
    let mut fixture = Fixture::new().await;
    let rejected = fixture
        .request(
            json!({"type": "createSendTransport", "requestId": "unjoined"}),
            "error",
        )
        .await
        .unwrap();
    assert_eq!(rejected["requestId"], "unjoined");
    fixture.join().await.unwrap();
    for command in [
        json!({"type": "consume", "requestId": "missing-fields"}),
        json!({"type": "unknownCommand", "requestId": "unknown-command"}),
        json!({"type": "resumeConsumer", "requestId": "missing-consumer", "consumerId": Uuid::new_v4().to_string()}),
    ] {
        let rejected = fixture.request(command.clone(), "error").await.unwrap();
        assert_eq!(rejected["requestId"], command["requestId"]);
    }
    for request_id in [
        Value::Null,
        json!(""),
        json!("bad id"),
        json!("x".repeat(65)),
        json!(1),
    ] {
        let rejected = fixture
            .request(
                json!({"type": "createSendTransport", "requestId": request_id}),
                "error",
            )
            .await
            .unwrap();
        assert!(rejected.get("requestId").is_none());
        assert_eq!(rejected["message"], "Invalid message format");
    }
    let legacy = fixture
        .request(
            json!({"type": "resumeConsumer", "consumerId": "missing"}),
            "error",
        )
        .await
        .unwrap();
    assert!(legacy.get("requestId").is_none());
    fixture.finish().await;
}

#[tokio::test]
async fn correlated_early_rate_rejection_echoes_the_rejected_request() {
    let mut fixture = Fixture::new().await;
    fixture.join().await.unwrap();
    // Invalid transport IDs spend the same budget without allocating transports.
    // Stop at the first rate rejection, before the repeated-abuse close policy.
    let mut rate_limited = false;
    for sequence in 0..20 {
        let request_id = format!("ice-{sequence}");
        let rejected = fixture
            .request(
                json!({"type": "restartIce", "requestId": request_id, "transportId": "missing"}),
                "error",
            )
            .await
            .unwrap();
        assert_eq!(rejected["requestId"], request_id);
        if rejected["message"] == "Media changes are rate limited" {
            rate_limited = true;
            break;
        }
    }
    fixture.finish().await;
    assert!(
        rate_limited,
        "the fixture must exercise rejection before dispatch"
    );
}

#[tokio::test]
async fn correlated_reconnect_replies_cover_unbound_and_already_joined_sockets() {
    let mut fixture = Fixture::new().await;
    let rejected = fixture
        .request(
            json!({
                "type": "reconnect", "requestId": "unbound-reconnect", "participantId": "missing",
                "roomId": "heartbeat-test", "reconnectToken": "missing",
            }),
            "reconnectResult",
        )
        .await
        .unwrap();
    assert_eq!(rejected["requestId"], "unbound-reconnect");
    assert_eq!(rejected["success"], false);
    let joined = fixture.join().await.unwrap();
    let rejected = fixture.request(json!({
        "type": "reconnect", "requestId": "joined-reconnect", "participantId": joined["participantId"],
        "roomId": "heartbeat-test", "reconnectToken": joined["reconnectToken"],
    }), "reconnectResult").await.unwrap();
    assert_eq!(rejected["requestId"], "joined-reconnect");
    assert_eq!(rejected["success"], false);
    fixture.finish().await;
}

#[test]
fn correlated_reply_envelopes_do_not_duplicate_authentication_or_social_ids() {
    let metrics = ServerMetrics::new();
    let (sender, mut receiver) = mpsc::channel(8);
    let reply = ReplySender {
        metrics: &metrics,
        sender: &sender,
        request_id: Some("request-1"),
    };
    for message in [
        ServerMessage::AuthenticationRenewalFailed {
            request_id: "auth-1".into(),
        },
        ServerMessage::SocialError {
            request_id: Some("social-1".into()),
            client_message_id: None,
            message: "Rejected".into(),
        },
        ServerMessage::SocialError {
            request_id: None,
            client_message_id: Some("chat-1".into()),
            message: "Rejected".into(),
        },
    ] {
        reply.send(&message).unwrap();
        let wire = receiver.try_recv().unwrap();
        // RequestHeader rejects duplicate IDs instead of silently taking the last.
        let _: RequestHeader = serde_json::from_str(&wire).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&wire).unwrap(),
            serde_json::to_value(&message).unwrap()
        );
    }
    let message = ServerMessage::ConsumerResumed {
        consumer_id: "consumer".into(),
    };
    reply.send(&message).unwrap();
    send_json(&metrics, &sender, &message).unwrap();
    let response: Value = serde_json::from_str(&receiver.try_recv().unwrap()).unwrap();
    let notification: Value = serde_json::from_str(&receiver.try_recv().unwrap()).unwrap();
    assert_eq!(response["requestId"], "request-1");
    assert!(notification.get("requestId").is_none());
}
