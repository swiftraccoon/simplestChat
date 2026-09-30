//! The real dispatcher and diagnostic recorder receive fixed benign messages.
//! Client delivery is checked separately because chat replies contain messages.

use super::*;
use crate::security_canary_tests::{Capture, SensitiveValues};
use tracing::instrument::WithSubscriber;

#[tokio::test]
async fn runtime_canary_dispatch_keeps_messages_and_credentials_out_of_observation_outputs() {
    let logs = Capture::default();
    let diagnostic_output = Capture::default();
    let diagnostics =
        crate::diagnostics::Diagnostics::with_test_writer(diagnostic_output.clone()).unwrap();
    let metrics = ServerMetrics::with_diagnostics(diagnostics.clone());
    let manager = Arc::new(RoomManager::new_for_connection_tests(metrics.clone()).await);
    let (sender, mut receiver) = mpsc::channel(128);
    let participant = Uuid::new_v4().to_string();
    let nonce = Uuid::new_v4().simple().to_string();
    let display_name = format!("DisplayCanary-{nonce}");
    let password = format!("RoomPasswordCanary-{nonce}");
    let content = format!("MessageCanary-{nonce}");
    let denied_content = format!("DeniedMessageCanary-{nonce}");
    let mut reconnect = Uuid::new_v4().to_string();
    let mut secrets = SensitiveValues::default();
    for (label, value) in [
        ("participant display name", display_name.as_str()),
        ("submitted room password", password.as_str()),
        ("chat content", content.as_str()),
        ("denied chat content", denied_content.as_str()),
        ("initial reconnect credential", reconnect.as_str()),
    ] {
        secrets.add(label, value);
    }
    let mut room = None;
    let lobby = Arc::new(AtomicBool::new(false));
    let reply = ReplySender {
        metrics: &metrics,
        sender: &sender,
        request_id: None,
    };
    let messages = [
        serde_json::json!({"type":"joinRoom","roomId":"canary-observation-room",
            "participantName":display_name,"password":password}),
        serde_json::json!({"type":"chatMessage","content":content}),
        serde_json::json!({"type":"chatMessage","content":denied_content}),
    ];
    async {
        for (index, body) in messages.into_iter().enumerate() {
            if index == 2 {
                manager
                    .remove_participant_for_sender("canary-observation-room", &participant, &sender)
                    .await
                    .unwrap();
            }
            let message: ClientMessage = serde_json::from_value(body).unwrap();
            let operation =
                diagnostics.operation(diagnostic_operation(&message), diagnostics.connection_id());
            let result = operation
                .scope(diagnostics::measure_result(
                    Stage::Dispatch,
                    handle_client_message(
                        &message,
                        &participant,
                        &mut room,
                        &lobby,
                        &reply,
                        &manager,
                        &None,
                        &mut reconnect,
                        &None,
                        false,
                        None,
                        None,
                    ),
                ))
                .await;
            operation.finish(if result.is_ok() {
                Outcome::Ok
            } else {
                Outcome::Error
            });
            assert_eq!(result.is_ok(), index != 2, "fixed dispatcher outcome");
            if index == 0 {
                secrets.add("issued reconnect credential", &reconnect);
                assert_eq!(room.as_deref(), Some("canary-observation-room"));
                assert!(
                    manager
                        .is_bound_participant("canary-observation-room", &participant, &sender)
                        .await
                );
                while receiver.try_recv().is_ok() {}
            } else if index == 1 {
                let mut delivered = false;
                while let Ok(message) = receiver.try_recv() {
                    delivered |= message.contains(&content);
                }
                assert!(
                    delivered,
                    "successful chat must reach the actual client reply channel"
                );
            }
        }
        manager.drain_signal().begin_draining();
    }
    .with_subscriber(logs.subscriber())
    .await;
    assert!(
        diagnostics.shutdown().await,
        "diagnostic recorder must flush successfully"
    );
    let records = diagnostic_output.records();
    assert!(
        records
            .iter()
            .any(|record| record["operation"] == "join_room" && record["outcome"] == "ok")
    );
    assert!(
        records
            .iter()
            .any(|record| record["operation"] == "chat" && record["outcome"] == "ok")
    );
    assert!(
        records
            .iter()
            .any(|record| record["operation"] == "chat" && record["outcome"] == "error")
    );
    let summary = records.last().unwrap();
    assert_eq!(summary["kind"], "summary");
    assert_eq!(summary["accepted"], summary["written"]);
    assert_eq!(
        summary["written"].as_u64().unwrap() as usize,
        records.len() - 1
    );
    for counter in ["dropped", "expired", "unfinished"] {
        assert_eq!(summary[counter], 0, "diagnostic coverage must be complete");
    }
    assert_eq!(summary["writeFailed"], false);
    assert!(
        logs.records()
            .iter()
            .any(|record| record["fields"]["message"]
                .as_str()
                .is_some_and(|message| message.contains("joined room"))),
        "actual membership log missing"
    );
    secrets.assert_absent("dispatcher tracing", &logs.text());
    secrets.assert_absent("diagnostic JSONL", &diagnostic_output.text());
    secrets.assert_absent("dispatcher metrics", &metrics.render_prometheus(1, 0, 1));
}
