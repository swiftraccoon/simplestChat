//! Invoke the real dispatcher directly with fixed, decoded benign commands.
//! No network client, media traffic, mutation campaign, or fuzz loop is involved.

use super::super::authorization_tests::manifest;
use super::*;

#[tokio::test]
async fn authorization_dispatch_rejects_every_room_operation_after_sender_or_membership_changes() {
    let metrics = ServerMetrics::new();
    let manager = Arc::new(RoomManager::new_for_connection_tests(metrics.clone()).await);
    let (sender, mut receiver) = mpsc::channel(128);
    let (other_sender, mut other_receiver) = mpsc::channel(8);
    let participant = Uuid::new_v4().to_string();
    let lobby = Arc::new(AtomicBool::new(false));
    let mut room = None;
    let mut reconnect = Uuid::new_v4().to_string();
    let reply = ReplySender {
        metrics: &metrics,
        sender: &sender,
        request_id: Some("policy"),
    };
    let join = serde_json::from_value(serde_json::json!({"type":"joinRoom", "roomId":"policy-room", "participantName":"Policy fixture"})).unwrap();
    handle_client_message(
        &join,
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
    )
    .await
    .unwrap();
    assert_eq!(room.as_deref(), Some("policy-room"));
    assert!(
        manager
            .is_bound_participant("policy-room", &participant, &sender)
            .await
    );
    while receiver.try_recv().is_ok() {}
    let operations = manifest().websocket;
    for removed in [false, true] {
        if removed {
            manager
                .remove_participant_for_sender("policy-room", &participant, &sender)
                .await
                .unwrap();
        }
        let reply = ReplySender {
            metrics: &metrics,
            sender: if removed { &sender } else { &other_sender },
            request_id: Some("policy"),
        };
        for row in operations
            .iter()
            .filter(|row| row.boundary == "room-membership")
        {
            let message = serde_json::from_value(row.body.clone()).unwrap();
            let error = handle_client_message(
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
            )
            .await
            .unwrap_err();
            assert_eq!(
                error.to_string(),
                "Participant is no longer in this room",
                "{} removed={removed}",
                row.operation
            );
            assert_eq!(room.as_deref(), Some("policy-room"));
        }
    }
    assert!(
        other_receiver.try_recv().is_err(),
        "denied commands cannot emit a success reply"
    );
    manager.drain_signal().begin_draining();
}

#[tokio::test]
async fn authorization_dispatch_lobby_policy_covers_every_decoded_operation() {
    let metrics = ServerMetrics::new();
    let manager = Arc::new(RoomManager::new_for_connection_tests(metrics.clone()).await);
    let (sender, mut receiver) = mpsc::channel(8);
    let reply = ReplySender {
        metrics: &metrics,
        sender: &sender,
        request_id: Some("policy"),
    };
    let lobby = Arc::new(AtomicBool::new(true));
    let participant = Uuid::new_v4().to_string();
    let mut room = Some("policy-room".into());
    let mut reconnect = Uuid::new_v4().to_string();
    let mut allowed = Vec::new();
    for row in manifest().websocket {
        match row.lobby.as_str() {
            "socket-authentication" => {
                assert_eq!(row.boundary, "current-session-renewal");
                continue;
            }
            "allow" => {
                allowed.push(row.variant);
                continue;
            }
            "deny" => {}
            _ => panic!("unreviewed lobby policy"),
        }
        let message = serde_json::from_value(row.body).unwrap();
        let error = handle_client_message(
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
        )
        .await
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "Cannot perform this action while waiting in lobby",
            "{}",
            row.operation
        );
    }
    assert_eq!(
        allowed,
        ["JoinRoom", "LeaveRoom", "Reconnect", "ChatMessage"]
    );
    assert!(receiver.try_recv().is_err());
    manager.drain_signal().begin_draining();
}

#[tokio::test]
async fn authorization_dispatch_moderation_checks_actor_and_target_roles() {
    use crate::room::roles::Role::{self, *};
    let metrics = ServerMetrics::new();
    let manager = Arc::new(RoomManager::new_for_connection_tests(metrics.clone()).await);
    let mut participants = Vec::new();
    for name in ["Actor", "Target"] {
        let (sender, receiver) = mpsc::channel(128);
        let id = Uuid::new_v4().to_string();
        let mut room = None;
        let reply = ReplySender {
            metrics: &metrics,
            sender: &sender,
            request_id: None,
        };
        let message = serde_json::from_value(
            serde_json::json!({"type":"joinRoom", "roomId":"policy-roles", "participantName":name}),
        )
        .unwrap();
        handle_client_message(
            &message,
            &id,
            &mut room,
            &Arc::new(AtomicBool::new(false)),
            &reply,
            &manager,
            &None,
            &mut Uuid::new_v4().to_string(),
            &None,
            false,
            None,
            None,
        )
        .await
        .unwrap();
        participants.push((id, sender, receiver));
    }
    let room = manager.room_for_connection_tests("policy-roles");
    let roles = [Guest, User, Member, Moderator, Admin, Owner];
    let targets: [&[Role]; 6] = [
        &[],
        &[],
        &[],
        &[Guest, User, Member],
        &[Guest, User, Member, Moderator],
        &[Guest, User, Member, Moderator, Admin],
    ];
    let (actor, sender, _) = &participants[0];
    let target = &participants[1].0;
    let reply = ReplySender {
        metrics: &metrics,
        sender,
        request_id: None,
    };
    for (index, role) in roles.into_iter().enumerate() {
        for target_role in roles {
            {
                let mut state = room.write().await;
                state.participants.get_mut(actor).unwrap().role = role;
                state.participants.get_mut(target).unwrap().role = target_role;
            }
            // Both are successful no-ops when allowed: no media to close and
            // an unchanged role. This tests permission order without spending
            // the production administration rate bucket or weakening it.
            for message in [
                ClientMessage::CloseCam {
                    target_participant_id: target.clone(),
                },
                ClientMessage::SetRole {
                    target_participant_id: target.clone(),
                    role: target_role as u8,
                },
            ] {
                let result = handle_client_message(
                    &message,
                    actor,
                    &mut Some("policy-roles".into()),
                    &Arc::new(AtomicBool::new(false)),
                    &reply,
                    &manager,
                    &None,
                    &mut String::new(),
                    &None,
                    false,
                    None,
                    None,
                )
                .await;
                assert_eq!(
                    result.is_ok(),
                    targets[index].contains(&target_role),
                    "{role:?} -> {target_role:?}: {result:?}"
                );
            }
            assert_eq!(
                room.read().await.participants.get(target).unwrap().role,
                target_role
            );
        }
        // Read-only moderation views have their own thresholds; they must not
        // inherit a generic "any moderator" assumption. Settings are Admin+.
        for (operation, permitted) in [
            ("listRoomBans", [false, false, false, false, true, true]),
            ("listRoomMembers", [false, false, false, true, true, true]),
            ("listRoomReports", [false, false, false, true, true, true]),
            (
                "listModerationEvents",
                [false, false, false, true, true, true],
            ),
            (
                "updateRoomSettings",
                [false, false, false, false, true, true],
            ),
            ("setTopic", [false, false, false, false, true, true]),
        ] {
            let row = manifest()
                .websocket
                .into_iter()
                .find(|row| row.operation == operation)
                .unwrap();
            let message = serde_json::from_value(row.body).unwrap();
            let result = handle_client_message(
                &message,
                actor,
                &mut Some("policy-roles".into()),
                &Arc::new(AtomicBool::new(false)),
                &reply,
                &manager,
                &None,
                &mut String::new(),
                &None,
                false,
                None,
                None,
            )
            .await;
            assert_eq!(
                result.is_ok(),
                permitted[index],
                "{role:?} {operation}: {result:?}"
            );
        }
    }
    manager.drain_signal().begin_draining();
}
