use super::*;
use crate::ids::fixtures::{coid, pid, prid, rid, trid};

#[tokio::test]
async fn shutdown_aborts_owned_tasks_on_drop() {
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (dropped_tx, dropped_rx) = tokio::sync::oneshot::channel();
    struct NotifyDrop(Option<tokio::sync::oneshot::Sender<()>>);
    impl Drop for NotifyDrop {
        fn drop(&mut self) {
            let _ = self.0.take().unwrap().send(());
        }
    }
    let task = OwnedTask(tokio::spawn(async move {
        let _guard = NotifyDrop(Some(dropped_tx));
        let _ = entered_tx.send(());
        std::future::pending::<()>().await;
    }));
    entered_rx.await.unwrap();
    drop(task);
    tokio::time::timeout(Duration::from_secs(1), dropped_rx)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn shutdown_cancels_retained_grace_and_rejects_late_timers() {
    let map = GracePeriodMap::new();
    let retained = grace_entry("retained");
    let retained_abort = retained.timer.abort_handle();
    assert!(map.insert(rid("room"), pid("peer"), retained));
    assert_eq!(map.close(), 1);
    assert_eq!(map.close(), 0);
    let late = grace_entry("late");
    let late_abort = late.timer.abort_handle();
    assert!(!map.clone().insert(rid("room"), pid("late-peer"), late));
    tokio::time::timeout(Duration::from_secs(1), async {
        while !retained_abort.is_finished() || !late_abort.is_finished() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(map.inner.read().unwrap().is_empty());
}

#[test]
fn control_acknowledgement_preserves_fire_and_forget_and_echoes_request_ids() {
    let metrics = ServerMetrics::new();
    let (sender, mut receiver) = mpsc::channel(2);
    for request_id in [None, Some("control-1")] {
        ReplySender {
            metrics: &metrics,
            sender: &sender,
            request_id,
        }
        .acknowledge_control()
        .unwrap();
        if let Some(request_id) = request_id {
            let reply: serde_json::Value =
                serde_json::from_str(&receiver.try_recv().unwrap()).unwrap();
            assert_eq!(
                reply,
                serde_json::json!({ "type": "mediaControlApplied", "requestId": request_id })
            );
        } else {
            assert!(matches!(
                receiver.try_recv(),
                Err(mpsc::error::TryRecvError::Empty)
            ));
        }
    }
}

#[test]
fn room_control_acknowledgement_requires_and_echoes_request_id() {
    let metrics = ServerMetrics::new();
    let (sender, mut receiver) = mpsc::channel(2);
    for request_id in [None, Some("room-control-1")] {
        ReplySender {
            metrics: &metrics,
            sender: &sender,
            request_id,
        }
        .acknowledge_room_control()
        .unwrap();
        if let Some(request_id) = request_id {
            let reply: serde_json::Value =
                serde_json::from_str(&receiver.try_recv().unwrap()).unwrap();
            assert_eq!(
                reply,
                serde_json::json!({"type":"roomControlApplied", "requestId": request_id})
            );
        } else {
            assert!(matches!(
                receiver.try_recv(),
                Err(mpsc::error::TryRecvError::Empty)
            ));
        }
    }
}

#[test]
fn password_challenge_is_typed_and_internal_errors_stay_generic() {
    let error = anyhow::Error::new(RoomPasswordRequired).context("joining a room");
    assert_eq!(
        serde_json::to_value(client_error_response(&error)).unwrap(),
        serde_json::json!({ "type": "roomPasswordRequired" }),
    );
    for error in [
        anyhow::anyhow!("database connection details"),
        anyhow::anyhow!("This room requires a password"),
    ] {
        assert_eq!(
            serde_json::to_value(client_error_response(&error)).unwrap(),
            serde_json::json!({
                "type": "error",
                "code": "request_failed",
                "message": "Request could not be completed",
            }),
        );
    }
}

#[tokio::test]
async fn socket_counters_distinguish_completed_writes_errors_and_timeouts() {
    let metrics = ServerMetrics::new();
    let mut successful = futures_util::sink::drain();
    assert!(
        write_message(
            &mut successful,
            Message::Text("fixture".into()),
            &metrics,
            None,
            tokio::time::Instant::now() + Duration::from_secs(1),
            OperationKind::SocketWrite
        )
        .await
        .is_ok()
    );
    let mut failed = Box::pin(futures_util::sink::unfold((), |(), _: Message| async {
        Err::<(), ()>(())
    }));
    assert!(matches!(
        write_message(
            &mut failed,
            Message::Text("fixture".into()),
            &metrics,
            None,
            tokio::time::Instant::now() + Duration::from_secs(1),
            OperationKind::SocketWrite
        )
        .await,
        Err(Outcome::Error)
    ));
    let mut stalled = Box::pin(futures_util::sink::unfold((), |(), _: Message| {
        std::future::pending::<Result<(), ()>>()
    }));
    assert!(matches!(
        write_message(
            &mut stalled,
            Message::Text("fixture".into()),
            &metrics,
            None,
            tokio::time::Instant::now(),
            OperationKind::SocketWrite
        )
        .await,
        Err(Outcome::Timeout)
    ));
    let rendered = metrics.render_prometheus(0, 0, 0);
    assert!(rendered.contains("simplestchat_messages_sent_total 1\n"));
    assert!(rendered.contains("simplestchat_message_send_failed_total 2\n"));
}

#[tokio::test]
async fn cancelled_socket_write_is_not_a_completed_write_or_error() {
    let metrics = ServerMetrics::new();
    let mut stalled = Box::pin(futures_util::sink::unfold((), |(), _: Message| {
        std::future::pending::<Result<(), ()>>()
    }));
    let mut future = Box::pin(write_message(
        &mut stalled,
        Message::Text("fixture".into()),
        &metrics,
        None,
        tokio::time::Instant::now() + Duration::from_secs(10),
        OperationKind::SocketWrite,
    ));
    assert!(futures_util::poll!(&mut future).is_pending());
    drop(future);
    let rendered = metrics.render_prometheus(0, 0, 0);
    assert!(rendered.contains("simplestchat_messages_sent_total 0\n"));
    assert!(rendered.contains("simplestchat_message_send_failed_total 0\n"));
}

#[tokio::test]
async fn shutdown_notice_uses_one_deadline_and_counts_a_timeout() {
    let metrics = ServerMetrics::new();
    let mut stalled = Box::pin(futures_util::sink::unfold((), |(), _: Message| {
        std::future::pending::<Result<(), ()>>()
    }));
    let result = write_message(
        &mut stalled,
        Message::Text("fixture shutdown notice".into()),
        &metrics,
        None,
        tokio::time::Instant::now(),
        OperationKind::ShutdownNotification,
    )
    .await;
    assert!(matches!(result, Err(Outcome::Timeout)));
    let rendered = metrics.render_prometheus(0, 0, 0);
    assert!(rendered.contains("simplestchat_messages_sent_total 0\n"));
    assert!(rendered.contains("simplestchat_message_send_failed_total 1\n"));
}

#[test]
fn direct_signaling_queue_rejections_are_counted_without_payloads() {
    let metrics = ServerMetrics::new();
    let (sender, receiver) = mpsc::channel(1);
    let registration = crate::signaling::outbound::register(&sender);
    let message = ServerMessage::Error {
        code: ErrorCode::RateLimited,
        message: "PRIVATE_FIXTURE_CONTENT".into(),
    };
    send_json(&metrics, &sender, &message).unwrap();
    assert!(!*registration.subscribe().borrow());
    assert!(
        !send_json(&metrics, &sender, &message)
            .unwrap_err()
            .to_string()
            .contains("PRIVATE")
    );
    assert!(*registration.subscribe().borrow());
    drop(receiver);
    assert!(send_json(&metrics, &sender, &message).is_err());
    let rendered = metrics.render_prometheus(0, 0, 0);
    assert!(rendered.contains("simplestchat_outbound_queue_full_total 1\n"));
    assert!(rendered.contains("simplestchat_outbound_queue_closed_total 1\n"));
    assert!(rendered.contains("simplestchat_messages_sent_total 0\n"));
}

#[test]
fn password_challenge_allows_one_immediate_answer_for_the_same_room() {
    let now = Instant::now();
    let mut attempts = JoinAttemptState::default();
    assert!(attempts.allow(&rid("private-room"), false, now));
    attempts.password_challenge_room = Some(rid("private-room"));
    assert!(attempts.allow(&rid("private-room"), true, now));
    assert!(!attempts.allow(&rid("private-room"), true, now));
    assert!(attempts.allow(&rid("private-room"), true, now + Duration::from_secs(1)));
}

#[test]
fn unrelated_joins_do_not_use_the_password_challenge_allowance() {
    let now = Instant::now();
    for (room_id, has_password) in [("another-room", true), ("private-room", false)] {
        let mut attempts = JoinAttemptState::default();
        assert!(attempts.allow(&rid("private-room"), false, now));
        attempts.password_challenge_room = Some(rid("private-room"));
        assert!(!attempts.allow(&rid(room_id), has_password, now));
        assert!(!attempts.allow(&rid("private-room"), true, now));
    }
}

#[test]
fn media_mutation_budgets_cover_all_client_driven_mediasoup_ipc() {
    assert!(is_media_mutation(&ClientMessage::Produce {
        transport_id: trid("transport"),
        kind: mediasoup::prelude::MediaKind::Audio,
        rtp_parameters: mediasoup::prelude::RtpParameters::default(),
        source: Some("microphone".to_string()),
    }));
    assert!(is_media_mutation(&ClientMessage::CloseProducer {
        producer_id: prid("producer"),
    }));
    assert!(is_media_mutation(&ClientMessage::PauseProducer {
        producer_id: prid("producer"),
    }));
    assert!(is_media_mutation(&ClientMessage::ResumeProducer {
        producer_id: prid("producer"),
    }));
    assert!(is_transport_mutation(&ClientMessage::CreateSendTransport));
    assert!(is_transport_mutation(&ClientMessage::CreateRecvTransport));
    assert!(is_transport_mutation(
        &ClientMessage::ReplaceRecvTransport {
            transport_id: trid("transport"),
        }
    ));
    assert!(is_transport_mutation(&ClientMessage::RestartIce {
        transport_id: trid("transport"),
    }));
    assert!(is_consumer_mutation(&ClientMessage::PauseConsumer {
        consumer_id: coid("consumer"),
    }));
    assert!(is_consumer_mutation(&ClientMessage::CloseConsumer {
        consumer_id: coid("consumer"),
    }));
    assert!(is_consumer_mutation(&ClientMessage::ResumeConsumer {
        consumer_id: coid("consumer"),
    }));
    assert!(is_consumer_mutation(
        &ClientMessage::SetConsumerPreferredLayers {
            consumer_id: coid("consumer"),
            spatial_layer: 1,
            temporal_layer: Some(1),
        }
    ));
    assert!(!is_media_mutation(&ClientMessage::RequestVoice));
    assert!(!is_transport_mutation(&ClientMessage::RequestVoice));
    assert!(!is_consumer_mutation(&ClientMessage::RequestVoice));
    assert!(!is_media_mutation(&ClientMessage::ChatMessage {
        content: "hello".to_string(),
        attachment_ids: Vec::new(),
        client_message_id: None,
        sequence: None,
        reply_to: None,
    }));
}

#[test]
fn consumer_and_frame_bursts_admit_the_configured_media_startup() {
    let now = Instant::now();
    for limit in [16, 64, 128] {
        let mut session = MediaSessionRateState::with_consumer_limit(now, limit);
        let mut frame_bucket = TokenBucket::full(
            now,
            RATE_LIMIT_REFILL_RATE,
            frame_capacity(session.consumer_mutations.capacity_us()),
        );
        // Full producer and transport setup plus join and capabilities.
        for _ in
            0..MEDIA_MUTATION_RATE_LIMIT_MAX_TOKENS + TRANSPORT_MUTATION_RATE_LIMIT_MAX_TOKENS + 2
        {
            assert!(frame_bucket.try_consume(now));
        }
        for index in 0..limit {
            for message in [
                ClientMessage::Consume {
                    producer_id: prid(&format!("producer-{index}")),
                    rtp_capabilities: Default::default(),
                },
                ClientMessage::ResumeConsumer {
                    consumer_id: coid(&format!("consumer-{index}")),
                },
                ClientMessage::SetConsumerPreferredLayers {
                    consumer_id: coid(&format!("consumer-{index}")),
                    spatial_layer: 1,
                    temporal_layer: None,
                },
            ] {
                assert!(!session.limit_exceeded(&message, now));
                assert!(frame_bucket.try_consume(now));
            }
        }
        let extra = ClientMessage::ResumeConsumer {
            consumer_id: coid("extra"),
        };
        assert!(session.limit_exceeded(&extra, now), "burst remains bounded");
        let later = now + Duration::from_secs(1);
        for _ in 0..CONSUMER_MUTATION_RATE_LIMIT_REFILL_RATE {
            assert!(!session.limit_exceeded(&extra, later));
        }
        assert!(
            session.limit_exceeded(&extra, later),
            "sustained rate remains bounded"
        );
    }
    if let Ok(overflowing_limit) = usize::try_from(u64::MAX / (3 * TOKEN_US) + 1) {
        assert_eq!(consumer_mutation_capacity(overflowing_limit), u64::MAX);
    }
    assert_eq!(frame_capacity(u64::MAX), u64::MAX);
}

#[tokio::test]
async fn grace_reconnect_does_not_restore_consumer_burst_capacity() {
    let map = GracePeriodMap::new();
    let participant_id = ParticipantId::random();
    let token = Uuid::new_v4().to_string();
    let now = Instant::now();
    let consumer_mutation = ClientMessage::PauseConsumer {
        consumer_id: coid("consumer"),
    };
    let transport_mutation = ClientMessage::RestartIce {
        transport_id: trid("transport"),
    };
    let mut entry = grace_entry(&token);
    entry.media_rate_state = MediaSessionRateState::with_consumer_limit(
        now,
        crate::media::config::DEFAULT_MAX_CONSUMERS_PER_PARTICIPANT,
    );

    for _ in 0..entry.media_rate_state.consumer_mutations.capacity_us() / TOKEN_US {
        assert!(
            !entry
                .media_rate_state
                .limit_exceeded(&consumer_mutation, now)
        );
    }
    for _ in 0..TRANSPORT_MUTATION_RATE_LIMIT_MAX_TOKENS {
        assert!(
            !entry
                .media_rate_state
                .limit_exceeded(&transport_mutation, now)
        );
    }
    assert!(
        entry
            .media_rate_state
            .limit_exceeded(&consumer_mutation, now)
    );
    assert!(
        entry
            .media_rate_state
            .limit_exceeded(&transport_mutation, now)
    );
    assert_eq!(entry.media_rate_state.record_violation(now), 1);
    assert_eq!(entry.media_rate_state.record_violation(now), 2);

    map.insert(rid("room-a"), participant_id, entry);
    let mut restored = map
        .remove_if_token_matches(&rid("room-a"), &participant_id, &token, None)
        .expect("the matching reconnect token must recover its session state");
    restored.timer.abort();

    assert!(
        restored
            .media_rate_state
            .limit_exceeded(&consumer_mutation, now)
    );
    assert!(
        restored
            .media_rate_state
            .limit_exceeded(&transport_mutation, now)
    );
    assert_eq!(restored.media_rate_state.record_violation(now), 3);
}

#[tokio::test]
async fn reconnect_rate_state_is_isolated_between_session_incarnations() {
    let map = GracePeriodMap::new();
    let participant_id = ParticipantId::random();
    let stale_token = Uuid::new_v4().to_string();
    let replacement_token = Uuid::new_v4().to_string();
    let now = Instant::now();
    let mutation = ClientMessage::Produce {
        transport_id: trid("transport"),
        kind: mediasoup::prelude::MediaKind::Audio,
        rtp_parameters: mediasoup::prelude::RtpParameters::default(),
        source: Some("microphone".to_string()),
    };

    let mut stale = grace_entry(&stale_token);
    stale.media_rate_state = MediaSessionRateState::with_consumer_limit(
        now,
        crate::media::config::DEFAULT_MAX_CONSUMERS_PER_PARTICIPANT,
    );
    for _ in 0..MEDIA_MUTATION_RATE_LIMIT_MAX_TOKENS {
        assert!(!stale.media_rate_state.limit_exceeded(&mutation, now));
    }
    assert!(stale.media_rate_state.limit_exceeded(&mutation, now));

    let mut replacement = grace_entry(&replacement_token);
    replacement.media_rate_state = MediaSessionRateState::with_consumer_limit(
        now,
        crate::media::config::DEFAULT_MAX_CONSUMERS_PER_PARTICIPANT,
    );
    map.insert(rid("room-a"), participant_id, stale);
    map.insert(rid("room-a"), participant_id, replacement);

    let mut replacement = map
        .remove_if_token_matches(&rid("room-a"), &participant_id, &replacement_token, None)
        .expect("replacement incarnation should remain independently addressable");
    replacement.timer.abort();
    assert!(!replacement.media_rate_state.limit_exceeded(&mutation, now));

    let mut stale = map
        .remove_if_token_matches(&rid("room-a"), &participant_id, &stale_token, None)
        .expect("stale incarnation should remain independently addressable");
    stale.timer.abort();
    assert!(stale.media_rate_state.limit_exceeded(&mutation, now));
}

#[test]
fn admin_mutation_budget_covers_moderation_and_settings_messages() {
    assert!(is_admin_mutation(&ClientMessage::CloseCam {
        target_participant_id: pid("target"),
    }));
    assert!(is_admin_mutation(&ClientMessage::SetTopic {
        topic: "topic".to_string(),
    }));
    assert!(is_admin_mutation(&ClientMessage::UpdateRoomSettings {
        patch: SettingsPatch {
            moderated: Some(true),
            ..Default::default()
        }
    }));
    assert!(is_admin_mutation(&ClientMessage::DenyFromLobby {
        target_participant_id: pid("target"),
    }));
    assert!(!is_admin_mutation(&ClientMessage::RequestVoice));
    assert!(!is_admin_mutation(&ClientMessage::ChatMessage {
        content: "hello".to_string(),
        attachment_ids: Vec::new(),
        client_message_id: None,
        sequence: None,
        reply_to: None,
    }));
}

fn grace_entry(token: &str) -> GraceEntry {
    let (sender, _receiver) = mpsc::channel(1);
    GraceEntry {
        reconnect_token: token.to_string(),
        authenticated_subject: None,
        authenticated_version: None,
        media_rate_state: MediaSessionRateState::new(
            crate::media::config::DEFAULT_MAX_CONSUMERS_PER_PARTICIPANT,
        ),
        sender,
        timer: tokio::spawn(std::future::pending()),
    }
}

#[tokio::test]
async fn credential_revocation_only_removes_older_account_grace_entries() {
    let map = GracePeriodMap::new();
    for (token, subject, version) in [
        ("old", Some("alice"), Some(0)),
        ("new", Some("alice"), Some(1)),
        ("other", Some("bob"), Some(0)),
        ("guest", None, None),
    ] {
        let mut entry = grace_entry(token);
        entry.authenticated_subject = subject.map(str::to_owned);
        entry.authenticated_version = version;
        assert!(map.insert(rid("room"), pid(token), entry));
    }
    let removed = map.take_revoked("alice", 1);
    assert_eq!(removed.len(), 1);
    assert_eq!(removed[0].1, pid("old"));
    for (token, subject) in [
        ("new", Some("alice")),
        ("other", Some("bob")),
        ("guest", None),
    ] {
        map.remove_if_token_matches(&rid("room"), &pid(token), token, subject)
            .expect("unaffected session retained")
            .timer
            .abort();
    }
}

#[test]
fn joined_token_is_the_token_retained_for_disconnect() {
    let mut retained = "old-token".to_string();
    let presented_in_room_joined = begin_join_session(&mut retained);
    assert_ne!(presented_in_room_joined, "old-token");
    assert_eq!(presented_in_room_joined, retained);
}

#[test]
fn reconnect_is_only_allowed_from_an_unbound_socket() {
    assert!(reconnect_attempt_allowed(None, false));
    assert!(!reconnect_attempt_allowed(Some(&rid("room-a")), false));
    assert!(!reconnect_attempt_allowed(None, true));
}

#[test]
fn successful_reconnect_result_carries_rotated_token() {
    let token = Uuid::new_v4().to_string();
    let json = serde_json::to_value(ServerMessage::ReconnectResult {
        success: true,
        participant_id: ParticipantId::random(),
        reconnect_token: Some(token.clone().into()),
    })
    .unwrap();
    assert_eq!(
        json.get("reconnectToken").and_then(|value| value.as_str()),
        Some(token.as_str())
    );
}

#[tokio::test]
async fn wrong_reconnect_token_does_not_consume_or_extend_grace() {
    let map = GracePeriodMap::new();
    let participant_id = ParticipantId::random();
    let token = Uuid::new_v4().to_string();
    map.insert(rid("room-a"), participant_id, grace_entry(&token));

    assert!(
        map.remove_if_token_matches(&rid("room-a"), &participant_id, "wrong", None)
            .is_none()
    );
    let entry = map
        .remove_if_token_matches(&rid("room-a"), &participant_id, &token, None)
        .expect("valid token must still work after a failed attempt");
    entry.timer.abort();
}

#[tokio::test]
async fn grace_entries_are_scoped_by_room() {
    let map = GracePeriodMap::new();
    let participant_id = ParticipantId::random();
    let token_a = Uuid::new_v4().to_string();
    let token_b = Uuid::new_v4().to_string();
    map.insert(rid("room-a"), participant_id, grace_entry(&token_a));
    map.insert(rid("room-b"), participant_id, grace_entry(&token_b));

    let first = map
        .remove_if_token_matches(&rid("room-a"), &participant_id, &token_a, None)
        .unwrap();
    let second = map
        .remove_if_token_matches(&rid("room-b"), &participant_id, &token_b, None)
        .unwrap();
    first.timer.abort();
    second.timer.abort();
}

#[tokio::test]
async fn grace_capacity_rejects_new_state_without_evicting_reconnectable_session() {
    let map = GracePeriodMap::with_capacity(1);
    let first_id = ParticipantId::random();
    let first_token = Uuid::new_v4().to_string();
    let rejected_id = ParticipantId::random();
    let rejected_token = Uuid::new_v4().to_string();

    assert!(map.insert(rid("room-a"), first_id, grace_entry(&first_token),));
    assert!(!map.insert(rid("room-b"), rejected_id, grace_entry(&rejected_token),));
    assert!(
        map.remove_if_token_matches(&rid("room-b"), &rejected_id, &rejected_token, None)
            .is_none()
    );

    let retained = map
        .remove_if_token_matches(&rid("room-a"), &first_id, &first_token, None)
        .expect("capacity failure must not evict an existing grace session");
    retained.timer.abort();
}

#[tokio::test]
async fn stale_disconnect_does_not_replace_same_uuid_grace_entry() {
    let map = GracePeriodMap::new();
    let participant_id = ParticipantId::random();
    let stale_token = Uuid::new_v4().to_string();
    let replacement_token = Uuid::new_v4().to_string();
    map.insert(rid("room-a"), participant_id, grace_entry(&stale_token));
    map.insert(
        rid("room-a"),
        participant_id,
        grace_entry(&replacement_token),
    );

    let stale = map
        .remove_if_token_matches(&rid("room-a"), &participant_id, &stale_token, None)
        .expect("stale and replacement incarnations must coexist");
    let replacement = map
        .remove_if_token_matches(&rid("room-a"), &participant_id, &replacement_token, None)
        .expect("stale disconnect must not abort replacement grace");
    stale.timer.abort();
    replacement.timer.abort();
}

#[tokio::test]
async fn authenticated_grace_requires_same_refreshed_identity() {
    let map = GracePeriodMap::new();
    let participant_id = ParticipantId::random();
    let token = Uuid::new_v4().to_string();
    let mut entry = grace_entry(&token);
    entry.authenticated_subject = Some(participant_id.to_string());
    map.insert(rid("room-a"), participant_id, entry);

    assert!(
        map.remove_if_token_matches(&rid("room-a"), &participant_id, &token, None)
            .is_none()
    );
    let entry = map
        .remove_if_token_matches(
            &rid("room-a"),
            &participant_id,
            &token,
            Some(&participant_id.to_string()),
        )
        .unwrap();
    entry.timer.abort();
}
