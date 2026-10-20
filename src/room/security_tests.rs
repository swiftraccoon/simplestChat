use super::*;
use crate::ids::fixtures::{pid, prid, rid};

#[tokio::test]
async fn live_counts_skip_empty_rooms_and_keep_locked_rooms_unknown() {
    let make_room = |name: &str, members: usize| {
        let mut room = Room::new(rid(name), None, false, None);
        for index in 0..members {
            room.participants.insert(
                pid(&format!("member-{index}")),
                participant(
                    "member",
                    roles::Role::Guest,
                    moderation::PunitiveState::default(),
                    None,
                ),
            );
        }
        Arc::new(TokioRwLock::new(room))
    };
    let rooms: HashMap<RoomId, Arc<TokioRwLock<Room>>> = [
        (rid("empty"), make_room("empty", 0)),
        (rid("busy"), make_room("busy", 3)),
        (rid("locked"), make_room("locked", 1)),
    ]
    .into_iter()
    .collect();
    let held = rooms[&rid("locked")].clone();
    let guard = held.write().await;
    let mut counts = RoomManager::live_counts(rooms.iter());
    drop(guard);
    counts.sort();
    assert_eq!(counts, vec![(rid("busy"), Some(3)), (rid("locked"), None)]);
}

#[tokio::test]
async fn participant_snapshot_distinguishes_empty_partial_and_complete_counts() {
    assert_eq!(RoomManager::count_readable_participants(&[]), (0, true));
    let make_room = |name: &str| {
        let mut room = Room::new(rid(name), None, false, None);
        room.participants.insert(
            pid("member"),
            participant(
                "member",
                roles::Role::Guest,
                moderation::PunitiveState::default(),
                None,
            ),
        );
        Arc::new(TokioRwLock::new(room))
    };
    let rooms = [make_room("first"), make_room("second")];
    assert_eq!(RoomManager::count_readable_participants(&rooms), (2, true));
    let first_locked = rooms[0].write().await;
    assert_eq!(RoomManager::count_readable_participants(&rooms), (1, false));
    let second_locked = rooms[1].write().await;
    assert_eq!(RoomManager::count_readable_participants(&rooms), (0, false));
    drop(first_locked);
    drop(second_locked);
    assert_eq!(RoomManager::count_readable_participants(&rooms), (2, true));
}

async fn drain_test_manager() -> RoomManager {
    let mut config = MediaConfig::default();
    config.worker_config.num_workers = 1;
    // Port zero asks the owned native listener to select an unused port.
    config.webrtc_server_port_base = 0;
    let mut manager = RoomManager::new(
        ManagerSettings::default(),
        config,
        ServerMetrics::new(),
        None,
    )
    .await
    .unwrap();
    manager.allow_ad_hoc_rooms = true;
    manager
}

async fn two_worker_test_manager() -> Arc<RoomManager> {
    let mut config = MediaConfig::default();
    config.worker_config.num_workers = 2;
    config.webrtc_server_port_base = crate::media::worker_manager::reserve_worker_ports(2);
    let mut manager = RoomManager::new(
        ManagerSettings::default(),
        config,
        ServerMetrics::new(),
        None,
    )
    .await
    .unwrap();
    manager.allow_ad_hoc_rooms = true;
    Arc::new(manager)
}

async fn join_guest(
    manager: &RoomManager,
    room: &str,
    name: &str,
) -> (
    mpsc::Sender<crate::OutboundJson>,
    mpsc::Receiver<crate::OutboundJson>,
) {
    let (tx, rx) = mpsc::channel(32);
    let joined = manager
        .add_participant(
            &rid(room),
            pid(name),
            name.into(),
            tx.clone(),
            false,
            Arc::new(AtomicBool::new(false)),
            None,
            &format!("{name}-token"),
            None,
            None,
        )
        .await
        .unwrap();
    assert!(matches!(joined, JoinResult::Joined { .. }));
    (tx, rx)
}

#[tokio::test]
async fn guests_sharing_a_name_get_numbered_and_nobody_may_take_a_name_in_use() {
    use crate::signaling::protocol::ClientMessage;
    let manager = drain_test_manager().await;
    let join = |id: ParticipantId, name: &str, authenticated: bool| {
        let manager = &manager;
        let name = name.to_string();
        async move {
            let (tx, rx) = mpsc::channel(32);
            let joined = manager
                .add_participant(
                    &rid("names"),
                    id,
                    name,
                    tx.clone(),
                    authenticated,
                    Arc::new(AtomicBool::new(false)),
                    None,
                    &format!("{id}-token"),
                    None,
                    None,
                )
                .await
                .unwrap();
            let JoinResult::Joined { name, .. } = joined else {
                panic!("a fresh join is admitted")
            };
            (name, tx, rx)
        }
    };
    let (first, _tx1, _rx1) = join(pid("g1"), "Maya", false).await;
    let (second, tx2, _rx2) = join(pid("g2"), " maya ", false).await;
    let (third, _tx3, _rx3) = join(pid("g3"), "MAYA", false).await;
    assert_eq!(
        (first.as_str(), second.as_str(), third.as_str()),
        ("Maya", "maya (2)", "MAYA (3)"),
        "a taken name gains the smallest free number, ignoring case and spaces"
    );
    let (account, _tx4, _rx4) = join(ParticipantId::random(), "Maya", false).await;
    assert_eq!(
        account, "Maya (4)",
        "UUID guest identities also receive an unambiguous room label"
    );
    let (variant, _tx7, _rx7) = join(pid("g7"), "Ma\u{200D}ya", false).await;
    assert_eq!(variant, "Ma\u{200D}ya (5)");
    let long = "x".repeat(crate::signaling::connection::MAX_PARTICIPANT_NAME_LEN);
    let (_l1, _tx5, _rx5) = join(pid("g5"), &long, false).await;
    let (l2, _tx6, _rx6) = join(pid("g6"), &long, false).await;
    assert!(l2.ends_with(" (2)") && l2.chars().count() <= 64, "{l2}");

    let rename = |nickname: &str| {
        let command = ClientMessage::ChangeNickname {
            request_id: uuid::Uuid::new_v4().to_string(),
            nickname: nickname.to_string(),
        };
        let (manager, tx2) = (&manager, &tx2);
        async move {
            manager
                .handle_social_request(&rid("names"), &pid("g2"), tx2, &command)
                .await
        }
    };
    assert!(rename("maya").await.is_err(), "another guest's name");
    assert!(
        rename("MAYA (3)").await.is_err(),
        "another guest's numbered name"
    );
    assert!(
        rename("maya (2)").await.is_ok(),
        "one's own name, differently cased"
    );
    assert!(rename("Dave").await.is_ok());
}

#[tokio::test]
async fn a_guest_chat_style_reaches_everyone_and_rides_their_messages() {
    use crate::signaling::protocol::{ChatStyleKind, ClientMessage};
    let manager = drain_test_manager().await;
    let teal = ChatStyle {
        color: Some("teal".into()),
        style: ChatStyleKind::Bubble,
    };
    let join = |name: &str, style: Option<ChatStyle>| {
        let (tx, rx) = mpsc::channel(64);
        let manager = &manager;
        let name = name.to_string();
        async move {
            let joined = manager
                .add_participant(
                    &rid("styled"),
                    pid(&name),
                    name.clone(),
                    tx.clone(),
                    false,
                    Arc::new(AtomicBool::new(false)),
                    None,
                    &format!("{name}-token"),
                    None,
                    style,
                )
                .await
                .unwrap();
            (joined, tx, rx)
        }
    };
    let (joined, _alice, mut alice_rx) = join("alice", Some(teal.clone())).await;
    assert!(matches!(joined, JoinResult::Joined { ref chat_style, .. } if *chat_style == teal));
    // A color outside the palette is dropped at the join, not refused.
    let outside = ChatStyle {
        color: Some("#ff0000".into()),
        style: ChatStyleKind::Text,
    };
    let (joined, bob, mut bob_rx) = join("bob", Some(outside)).await;
    let JoinResult::Joined {
        participants,
        chat_style,
        ..
    } = joined
    else {
        panic!("bob joined");
    };
    assert_eq!(chat_style, ChatStyle::default());
    assert_eq!(participants[0].chat_style, teal, "bob sees alice's look");
    assert!(
        drain_messages(&mut alice_rx)
            .iter()
            .any(|m| m.contains("participantJoined") && m.contains(r#""chatStyle":{"color":null"#))
    );

    let sky = ChatStyle {
        color: Some("sky".into()),
        style: ChatStyleKind::Text,
    };
    manager
        .handle_social_request(
            &rid("styled"),
            &pid("bob"),
            &bob,
            &ClientMessage::SetChatStyle {
                request_id: "look".into(),
                chat_style: sky.clone(),
            },
        )
        .await
        .unwrap();
    let changed = serde_json::to_string(&ServerMessage::ChatStyleChanged {
        participant_id: pid("bob"),
        chat_style: sky.clone(),
    })
    .unwrap();
    assert!(drain_messages(&mut alice_rx).contains(&changed));
    assert!(drain_messages(&mut bob_rx).contains(&changed));

    let refused = manager
        .handle_social_request(
            &rid("styled"),
            &pid("bob"),
            &bob,
            &ClientMessage::SetChatStyle {
                request_id: "outside".into(),
                chat_style: ChatStyle {
                    color: Some("url(x)".into()),
                    style: ChatStyleKind::Accent,
                },
            },
        )
        .await;
    assert!(refused.is_err());
    assert!(
        drain_messages(&mut alice_rx).is_empty(),
        "nothing broadcast"
    );

    manager
        .handle_chat_command(
            &rid("styled"),
            &pid("bob"),
            &bob,
            &ClientMessage::ChatMessage {
                content: "hello".into(),
                attachment_ids: Vec::new(),
                client_message_id: Some(uuid::Uuid::new_v4().to_string()),
                sequence: None,
                reply_to: None,
            },
        )
        .await
        .unwrap();
    let seen = drain_messages(&mut alice_rx);
    assert!(
        seen.iter()
            .any(|m| m.contains("hello")
                && m.contains(r#""chatStyle":{"color":"sky","style":"text"}"#)),
        "{seen:?}"
    );
}

#[tokio::test]
async fn server_wide_participant_ceiling_refuses_the_next_join() {
    let mut manager = drain_test_manager().await;
    manager.max_participants_per_room = Some(2);
    let (_alice_tx, _alice_rx) = join_guest(&manager, "capped", "alice").await;
    let (_bob_tx, _bob_rx) = join_guest(&manager, "capped", "bob").await;
    let (carol_tx, _carol_rx) = mpsc::channel(32);
    let refused = manager
        .add_participant(
            &rid("capped"),
            pid("carol"),
            "carol".into(),
            carol_tx,
            false,
            Arc::new(AtomicBool::new(false)),
            None,
            "carol-token",
            None,
            None,
        )
        .await;
    let refused = refused.err().expect("the third join is refused");
    assert_eq!(refused.to_string(), "Room is full");
    manager.media_server().shutdown().await.unwrap();
}

#[tokio::test]
async fn server_wide_participant_ceiling_refuses_lobby_admission() {
    let mut manager = drain_test_manager().await;
    manager.max_participants_per_room = Some(2);
    let (owner_tx, _owner_rx) = join_guest(&manager, "capped-lobby", "owner").await;
    let room = manager.get_room(&rid("capped-lobby")).unwrap();
    let mut settings = settings::RoomSettings::defaults(&rid("capped-lobby"));
    settings.lobby_enabled = true;
    room.write().await.settings = Some(settings);
    // Both wait while the room still has space; the first admission fills it.
    let mut receivers = Vec::new();
    for waiting in ["first", "second"] {
        let (waiting_tx, waiting_rx) = mpsc::channel(16);
        receivers.push(waiting_rx);
        let lobbied = manager
            .add_participant(
                &rid("capped-lobby"),
                pid(waiting),
                waiting.into(),
                waiting_tx,
                false,
                Arc::new(AtomicBool::new(false)),
                None,
                &format!("{waiting}-token"),
                None,
                None,
            )
            .await
            .unwrap();
        assert!(matches!(lobbied, JoinResult::Lobbied));
    }
    manager
        .admit_from_lobby(
            &rid("capped-lobby"),
            &pid("owner"),
            &owner_tx,
            &pid("first"),
        )
        .await
        .unwrap();
    let refused = manager
        .admit_from_lobby(
            &rid("capped-lobby"),
            &pid("owner"),
            &owner_tx,
            &pid("second"),
        )
        .await
        .expect_err("admission past the ceiling is refused");
    assert_eq!(refused.to_string(), "Room is full");
    assert!(room.read().await.lobby.contains_key(&pid("second")));
    manager.media_server().shutdown().await.unwrap();
}

fn drain_messages(rx: &mut mpsc::Receiver<crate::OutboundJson>) -> Vec<String> {
    let mut messages = Vec::new();
    while let Ok(message) = rx.try_recv() {
        messages.push(message.to_string());
    }
    messages
}

/// A worker thread cannot be killed through the API. Releasing its
/// listener leaves what a dead worker leaves behind: a free port the
/// replacement must bind, and rooms whose media is gone.
#[tokio::test]
async fn worker_death_recreates_capacity_and_asks_only_its_rooms_to_rejoin() {
    let manager = two_worker_test_manager().await;
    let (_alpha_tx, mut alpha_rx) = join_guest(&manager, "alpha", "alice").await;
    let (_beta_tx, mut beta_rx) = join_guest(&manager, "beta", "bob").await;
    let routers = manager.media_server().router_manager();
    let workers = manager.media_server().worker_manager();
    let alpha_worker = routers.get_worker_id("alpha").await.unwrap();
    let beta_worker = routers.get_worker_id("beta").await.unwrap();
    assert_ne!(
        alpha_worker, beta_worker,
        "empty rooms spread across workers"
    );
    drain_messages(&mut alpha_rx);
    drain_messages(&mut beta_rx);
    assert!(workers.release_worker_listener(alpha_worker).await);

    manager.recover_from_worker_death(alpha_worker).await;

    let alpha_messages = drain_messages(&mut alpha_rx);
    assert!(
        alpha_messages
            .iter()
            .any(|m| m.contains("\"serverRestarting\"")),
        "{alpha_messages:?}"
    );
    assert!(
        drain_messages(&mut beta_rx).is_empty(),
        "the other worker's room is untouched"
    );
    assert!(manager.get_room(&rid("alpha")).is_err());
    assert!(manager.get_room(&rid("beta")).is_ok());
    assert_eq!(
        workers.live_worker_count().await,
        2,
        "the dead worker is replaced"
    );
    assert!(!workers.is_worker_alive(alpha_worker).await);
    assert!(workers.is_worker_alive(beta_worker).await);
    assert!(!routers.has_router("alpha").await);

    // The rejoin lands on live capacity with a fresh router.
    let (_again_tx, _again_rx) = join_guest(&manager, "alpha", "alice").await;
    assert!(routers.has_router("alpha").await);
    assert_ne!(routers.get_worker_id("alpha").await.unwrap(), alpha_worker);
}

#[tokio::test]
async fn recovery_reserves_the_room_until_its_old_router_cleanup_finishes() {
    let manager = two_worker_test_manager().await;
    let (_sender, _receiver) = join_guest(&manager, "recovering", "alice").await;
    let routers = manager.media_server().router_manager();
    let old_router = routers.get_router("recovering").await.unwrap().id();
    let expected_worker = routers.get_worker_id("recovering").await.unwrap();
    manager.get_or_create_room(&rid("healthy")).await.unwrap();
    let other_worker = routers.get_worker_id("healthy").await.unwrap();
    assert_ne!(expected_worker, other_worker);
    manager
        .close_room_for_rejoin(&rid("recovering"), other_worker, "Stale death notice")
        .await
        .unwrap();
    assert_eq!(
        routers.get_router("recovering").await.unwrap().id(),
        old_router
    );
    let pause = routers.pause_router_removal_for_test().await;
    let recovering = manager.clone();
    let task = tokio::spawn(async move {
        recovering
            .close_room_for_rejoin(&rid("recovering"), expected_worker, "Rejoining")
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while routers.has_router("recovering").await {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(manager.get_room(&rid("recovering")).is_err());
    assert!(
        matches!(manager.get_or_create_room(&rid("recovering")).await,
        Err(error) if error.to_string() == "Room is being deleted")
    );
    assert!(!task.is_finished());
    drop(pause);
    task.await.unwrap().unwrap();
    let (_new_sender, _new_receiver) = join_guest(&manager, "recovering", "alice").await;
    assert_ne!(
        routers.get_router("recovering").await.unwrap().id(),
        old_router
    );
}

#[tokio::test]
async fn saturated_server_refuses_fresh_joins_and_counts_them() {
    let manager = drain_test_manager().await;
    assert!(manager.attach_saturation(crate::saturation::SaturationMonitor::forced(true)));
    assert!(!manager.attach_saturation(crate::saturation::SaturationMonitor::forced(false)));
    let (tx, _rx) = mpsc::channel(8);
    let error = match manager
        .add_participant(
            &rid("busy"),
            pid("alice"),
            "alice".into(),
            tx,
            false,
            Arc::new(AtomicBool::new(false)),
            None,
            "alice-token",
            None,
            None,
        )
        .await
    {
        Ok(_) => panic!("a saturated server admitted a fresh join"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("at capacity"), "{error}");
    assert!(
        manager.get_room(&rid("busy")).is_err(),
        "no room is created for a refused join"
    );
    let body = manager.metrics.render_prometheus(0, 0, 1);
    assert!(body.contains("simplestchat_joins_refused_saturated_total 1\n"));
}

#[tokio::test]
async fn join_to_a_room_on_a_saturated_worker_is_refused_while_other_rooms_continue() {
    let manager = two_worker_test_manager().await;
    let (_a, _ra) = join_guest(&manager, "alpha", "alice").await;
    let (_b, _rb) = join_guest(&manager, "beta", "bob").await;
    let routers = manager.media_server().router_manager();
    let alpha = routers.get_worker_id("alpha").await.unwrap();
    let beta = routers.get_worker_id("beta").await.unwrap();
    assert_ne!(alpha, beta, "two rooms spread over two workers");
    manager
        .media_server()
        .worker_manager()
        .set_worker_saturated(alpha, true);

    let (tx, _rx) = mpsc::channel(8);
    let error = match manager
        .add_participant(
            &rid("alpha"),
            pid("carol"),
            "carol".into(),
            tx,
            false,
            Arc::new(AtomicBool::new(false)),
            None,
            "carol-token",
            None,
            None,
        )
        .await
    {
        Ok(_) => panic!("a room on a saturated worker admitted a fresh join"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("at capacity"), "{error}");
    let (_d, _rd) = join_guest(&manager, "beta", "dave").await;
    let (_e, _re) = join_guest(&manager, "gamma", "erin").await;
    assert_eq!(
        routers.get_worker_id("gamma").await.unwrap(),
        beta,
        "new rooms avoid the saturated worker"
    );
    let body = manager.metrics.render_prometheus(0, 0, 2);
    assert!(body.contains("simplestchat_joins_refused_saturated_total 1\n"));
}

#[tokio::test]
async fn queued_worker_deaths_reach_the_recovery_task() {
    let manager = two_worker_test_manager().await;
    manager.spawn_worker_recovery();
    let (_tx, _rx) = join_guest(&manager, "gamma", "carol").await;
    let workers = manager.media_server().worker_manager();
    let worker = manager
        .media_server()
        .router_manager()
        .get_worker_id("gamma")
        .await
        .unwrap();
    assert!(workers.release_worker_listener(worker).await);
    assert!(workers.simulate_death(worker));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while manager.get_room(&rid("gamma")).is_ok() {
        assert!(std::time::Instant::now() < deadline, "recovery did not run");
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert_eq!(workers.live_worker_count().await, 2);
    assert!(!workers.is_worker_alive(worker).await);
}

#[tokio::test]
async fn rejoin_after_abrupt_disconnect_replaces_the_retained_membership() {
    let manager = Arc::new(drain_test_manager().await);
    let user_id = ParticipantId::random();
    let (first_tx, first_rx) = mpsc::channel(16);
    let joined = manager
        .add_participant(
            &rid("rejoin"),
            user_id,
            "User".into(),
            first_tx.clone(),
            false,
            Arc::new(AtomicBool::new(false)),
            None,
            "token-1",
            None,
            None,
        )
        .await
        .unwrap();
    assert!(matches!(joined, JoinResult::Joined { .. }));

    // The socket closed abruptly: its writer is gone while the membership
    // is retained for reconnect grace. A reload has no reconnect token.
    drop(first_rx);
    let (second_tx, _second_rx) = mpsc::channel(16);
    let rejoined = manager
        .add_participant(
            &rid("rejoin"),
            user_id,
            "User".into(),
            second_tx.clone(),
            false,
            Arc::new(AtomicBool::new(false)),
            None,
            "token-2",
            None,
            None,
        )
        .await
        .unwrap();
    assert!(matches!(rejoined, JoinResult::Joined { .. }));

    let room = manager.get_room(&rid("rejoin")).unwrap();
    {
        let room = room.read().await;
        assert_eq!(room.participants.len(), 1);
        assert!(room.participants[&user_id].sender.same_channel(&second_tx));
    }
    // The stale socket's grace timer can no longer evict the replacement.
    assert!(
        !manager
            .remove_participant_for_sender(&rid("rejoin"), &user_id, &first_tx)
            .await
            .unwrap()
    );
    assert_eq!(room.read().await.participants.len(), 1);
}

#[tokio::test]
async fn room_creation_waits_out_only_the_room_whose_identity_is_updating() {
    let manager = drain_test_manager().await;
    let guard = manager.begin_identity_update(&rid("editing"));
    let blocked = manager
        .get_or_create_room(&rid("editing"))
        .await
        .err()
        .expect("creation of the updating room is excluded");
    assert!(blocked.to_string().contains("updated"), "{blocked}");
    let other = manager.get_or_create_room(&rid("other")).await;
    assert!(other.is_ok(), "unrelated rooms are unaffected");
    drop(other);
    drop(guard);
    assert!(manager.get_or_create_room(&rid("editing")).await.is_ok());
}

#[tokio::test]
async fn join_gives_up_on_a_busy_room_control_lock_instead_of_hanging() {
    let manager = Arc::new(drain_test_manager().await);
    let (owner_tx, _owner_rx) = mpsc::channel(16);
    manager
        .add_participant(
            &rid("busy-control"),
            pid("owner"),
            "Owner".into(),
            owner_tx,
            false,
            Arc::new(AtomicBool::new(false)),
            None,
            "owner-token",
            None,
            None,
        )
        .await
        .unwrap();
    let room = manager.get_room(&rid("busy-control")).unwrap();
    // A persisted policy write holds control across SQL for up to 15 s.
    let _held = control::lock_room(&room).await;
    let (tx, _rx) = mpsc::channel(16);
    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(12),
        manager.add_participant(
            &rid("busy-control"),
            pid("second"),
            "Second".into(),
            tx,
            false,
            Arc::new(AtomicBool::new(false)),
            None,
            "second-token",
            None,
            None,
        ),
    )
    .await
    .expect("join must fail within its own bound, not hang on control");
    let error = match outcome {
        Err(error) => error,
        Ok(_) => panic!("join succeeded while room control was held"),
    };
    assert!(error.to_string().contains("busy"), "{error}");
}

#[tokio::test]
async fn directory_counts_are_unknown_not_zero_while_a_room_is_write_locked() {
    let room = Arc::new(TokioRwLock::new(Room::new(rid("dir"), None, false, None)));
    room.write().await.participants.insert(
        pid("p"),
        participant(
            "p",
            roles::Role::User,
            moderation::PunitiveState::default(),
            None,
        ),
    );
    assert_eq!(RoomManager::readable_participant_count(&room), Some(1));
    assert_eq!(RoomManager::readable_broadcaster_count(&room), Some(0));
    let held = room.write().await;
    assert_eq!(RoomManager::readable_participant_count(&room), None);
    assert_eq!(RoomManager::readable_broadcaster_count(&room), None);
    drop(held);
}

#[tokio::test]
async fn lobby_status_tracks_live_admission_authority_across_membership_changes() {
    let manager = drain_test_manager().await;
    let mut channels = Vec::new();
    for id in ["owner", "member", "waiting", "other-waiting"] {
        if id == "waiting" {
            let room = manager.get_room(&rid("lobby-status")).unwrap();
            let mut settings = settings::RoomSettings::defaults(&rid("lobby-status"));
            settings.lobby_enabled = true;
            room.write().await.settings = Some(settings);
        }
        let (sender, receiver) = mpsc::channel(32);
        manager
            .add_participant(
                &rid("lobby-status"),
                pid(id),
                id.into(),
                sender.clone(),
                false,
                Arc::new(AtomicBool::new(false)),
                None,
                "test-token",
                None,
                None,
            )
            .await
            .unwrap();
        channels.push((sender, receiver));
    }
    let read = |receiver: &mut mpsc::Receiver<crate::OutboundJson>| -> serde_json::Value {
        serde_json::from_str(&receiver.try_recv().unwrap()).unwrap()
    };
    for index in [2, 3] {
        let waiting = read(&mut channels[index].1);
        assert_eq!(waiting["type"], "lobbyWaiting");
        assert_eq!(waiting["participantCount"], 2);
        assert_eq!(waiting["moderatorCount"], 1);
    }
    let status = |receiver: &mut mpsc::Receiver<crate::OutboundJson>,
                  participants: u32,
                  moderators: u32| {
        assert_eq!(
            read(receiver),
            serde_json::json!({"type":"lobbyStatus", "participantCount":participants,"moderatorCount":moderators})
        );
    };
    manager
        .mark_participant_disconnected(&rid("lobby-status"), &pid("owner"), &channels[0].0)
        .await
        .unwrap();
    assert!(
        !channels[0].0.is_closed(),
        "availability cannot rely on dropped receivers"
    );
    for index in [2, 3] {
        status(&mut channels[index].1, 1, 0);
    }
    let (rebound, _rebound_receiver) = mpsc::channel(32);
    assert!(
        manager
            .rebind_participant_sender(
                &rid("lobby-status"),
                &pid("owner"),
                None,
                &channels[0].0,
                rebound.clone()
            )
            .await
            .unwrap()
    );
    for index in [2, 3] {
        status(&mut channels[index].1, 2, 1);
    }
    manager
        .mark_participant_disconnected(&rid("lobby-status"), &pid("owner"), &channels[0].0)
        .await
        .unwrap();
    for index in [2, 3] {
        assert!(channels[index].1.try_recv().is_err());
    }
    manager
        .set_participant_role(
            &rid("lobby-status"),
            &pid("owner"),
            &rebound,
            &pid("member"),
            roles::Role::Moderator,
        )
        .await
        .unwrap();
    for index in [2, 3] {
        status(&mut channels[index].1, 2, 2);
    }
    manager
        .kick_participant(
            &rid("lobby-status"),
            &pid("owner"),
            &rebound,
            &pid("member"),
            None,
            None,
        )
        .await
        .unwrap();
    for index in [2, 3] {
        status(&mut channels[index].1, 1, 1);
    }
    manager
        .admit_from_lobby(
            &rid("lobby-status"),
            &pid("owner"),
            &rebound,
            &pid("waiting"),
        )
        .await
        .unwrap();
    status(&mut channels[3].1, 2, 1);
    manager
        .ban_participant(
            &rid("lobby-status"),
            &pid("owner"),
            &rebound,
            &pid("waiting"),
            None,
            None,
            None,
        )
        .await
        .unwrap();
    status(&mut channels[3].1, 1, 1);
    manager
        .remove_participant(&rid("lobby-status"), &pid("owner"))
        .await
        .unwrap();
    status(&mut channels[3].1, 0, 0);
    manager.shutdown().await.unwrap();
    manager.media_server().shutdown().await.unwrap();
}

#[test]
fn lobby_counts_exclude_closed_queues_and_disconnected_grace_members() {
    let mut room = Room::new(rid("room"), None, false, None);
    room.participants.insert(
        pid("closed"),
        participant(
            "closed",
            roles::Role::Owner,
            moderation::PunitiveState::default(),
            None,
        ),
    );
    let (sender, _receiver) = mpsc::channel(1);
    let mut connected = participant(
        "live",
        roles::Role::User,
        moderation::PunitiveState::default(),
        None,
    );
    connected.sender = sender;
    room.participants.insert(pid("live"), connected);
    assert_eq!(room.lobby_counts(), (1, 0));
    room.participants
        .get_mut(&pid("live"))
        .unwrap()
        .social
        .connected = false;
    assert_eq!(room.lobby_counts(), (0, 0));
}

#[tokio::test]
async fn shutdown_rejects_admissions_and_clears_live_and_lobby_membership() {
    let manager = Arc::new(drain_test_manager().await);
    let (owner_tx, _owner_rx) = mpsc::channel(16);
    let owner_join = manager
        .add_participant(
            &rid("drain-test"),
            pid("owner"),
            "Owner".into(),
            owner_tx.clone(),
            false,
            Arc::new(AtomicBool::new(false)),
            None,
            "owner-token",
            None,
            None,
        )
        .await
        .unwrap();
    assert!(matches!(owner_join, JoinResult::Joined { .. }));
    let room = manager.get_room(&rid("drain-test")).unwrap();
    let mut settings = settings::RoomSettings::defaults(&rid("drain-test"));
    settings.lobby_enabled = true;
    room.write().await.settings = Some(settings);
    let (waiting_tx, _waiting_rx) = mpsc::channel(16);
    let waiting = manager
        .add_participant(
            &rid("drain-test"),
            pid("waiting"),
            "Waiting".into(),
            waiting_tx,
            false,
            Arc::new(AtomicBool::new(false)),
            None,
            "waiting-token",
            None,
            None,
        )
        .await
        .unwrap();
    assert!(matches!(waiting, JoinResult::Lobbied));

    // This reservation began before drain but cannot commit until its room
    // lock becomes available. The post-await admission guard must reject it.
    let held_room = room.write().await;
    let joining_manager = manager.clone();
    let joining_room = room.clone();
    let pending = tokio::spawn(async move {
        joining_manager
            .reserve_existing_room_join(&rid("drain-test"), joining_room)
            .await
            .is_err()
    });
    tokio::task::yield_now().await;
    manager.drain_signal().begin_draining();
    drop(held_room);
    let late_join_rejected = pending.await.unwrap();
    let new_room_rejected = manager.get_or_create_room(&rid("late-room")).await.is_err();
    let reconnect_rejected = manager
        .rebind_participant_sender(
            &rid("drain-test"),
            &pid("owner"),
            None,
            &owner_tx,
            owner_tx.clone(),
        )
        .await
        .is_err();
    let admission_rejected = manager
        .admit_from_lobby(
            &rid("drain-test"),
            &pid("owner"),
            &owner_tx,
            &pid("waiting"),
        )
        .await
        .is_err();
    let cleanup = manager.shutdown().await;
    let repeated_cleanup = manager.shutdown().await;
    let media_cleanup = manager.media_server().shutdown().await;
    assert!(late_join_rejected && new_room_rejected && reconnect_rejected && admission_rejected);
    cleanup.unwrap();
    repeated_cleanup.unwrap();
    media_cleanup.unwrap();
    assert_eq!(manager.room_count().await, 0);
    assert_eq!(
        manager
            .media_server()
            .worker_manager()
            .live_worker_count()
            .await,
        0
    );
    let retired = room.read().await;
    assert!(retired.participants.is_empty() && retired.lobby.is_empty() && retired.deleting);
}

#[tokio::test]
async fn shutdown_reports_stalled_creation_and_room_locks_without_reopening_admission() {
    let manager = drain_test_manager().await;
    let pending_hash = manager
        .password_hash_work
        .clone()
        .try_acquire_owned()
        .unwrap();
    let pending_verify = manager
        .password_verify_work
        .clone()
        .try_acquire_owned()
        .unwrap();
    assert_eq!(manager.pending_password_work(), 2);
    drop(pending_hash);
    drop(pending_verify);
    assert_eq!(manager.pending_password_work(), 0);
    let room = Arc::new(TokioRwLock::new(Room::new(
        rid("stalled"),
        None,
        false,
        None,
    )));
    manager.rooms.write().insert(rid("stalled"), room.clone());
    let held_creation = manager.room_creation_lock.lock().await;
    let creation_result = manager
        .shutdown_with_budget(std::time::Duration::from_millis(5))
        .await;
    drop(held_creation);
    let held_room = room.write().await;
    let result = manager
        .shutdown_with_budget(std::time::Duration::from_millis(5))
        .await;
    drop(held_room);
    let media_cleanup = manager.media_server().shutdown().await;
    assert!(creation_result.is_err());
    assert!(result.is_err());
    assert!(manager.drain_signal().is_draining());
    assert!(manager.get_or_create_room(&rid("new-room")).await.is_err());
    media_cleanup.unwrap();
}

#[tokio::test]
async fn silence_observer_event_clears_audio_levels_for_every_participant() {
    let mut room = Room::new(rid("room"), None, false, None);
    let (sender, mut receiver) = mpsc::channel(4);
    let mut member = participant(
        "member",
        roles::Role::Member,
        moderation::PunitiveState::default(),
        None,
    );
    member.sender = sender;
    room.participants.insert(pid("member"), member);
    let room = Arc::new(TokioRwLock::new(room));
    let (events, event_receiver) = mpsc::channel(4);
    events.try_send(ObserverEvent::Silence).unwrap();
    drop(events);
    RoomManager::observer_broadcast_task(event_receiver, Arc::downgrade(&room)).await;
    let message = receiver.try_recv().unwrap();
    assert_eq!(message.as_str(), r#"{"type":"audioLevels","levels":[]}"#);
}

#[test]
fn broadcast_queue_counters_count_selected_essential_rejections_only() {
    let mut room = Room::new(rid("room"), None, false, None);
    let (owner_sender, _owner_receiver) = mpsc::channel(1);
    let (member_sender, _member_receiver) = mpsc::channel(1);
    owner_sender
        .try_send(crate::OutboundJson::from("queued"))
        .unwrap();
    member_sender
        .try_send(crate::OutboundJson::from("queued"))
        .unwrap();
    for (id, role, sender) in [
        ("owner", roles::Role::Owner, Some(owner_sender)),
        ("member", roles::Role::Member, Some(member_sender)),
        ("closed", roles::Role::Guest, None),
    ] {
        let mut entry = participant(id, role, moderation::PunitiveState::default(), None);
        if let Some(sender) = sender {
            entry.sender = sender;
        }
        room.participants.insert(pid(id), entry);
    }
    let control = ServerMessage::RoomClosed {
        reason: "Room closed".into(),
    };
    room.broadcast_except(&pid("owner"), &control);
    room.broadcast_to_role(roles::Role::Moderator, &control);
    room.broadcast_all(&control);

    let before_hints = room.metrics.render_prometheus(0, 0, 0);
    assert!(
        before_hints
            .lines()
            .any(|line| line == "simplestchat_outbound_queue_full_total 4")
    );
    assert!(
        before_hints
            .lines()
            .any(|line| line == "simplestchat_outbound_queue_closed_total 2")
    );
    for hint in [
        ServerMessage::ActiveSpeaker {
            participant_id: pid("owner"),
        },
        ServerMessage::AudioLevels { levels: vec![] },
    ] {
        room.broadcast_all(&hint);
        room.broadcast_except(&pid("owner"), &hint);
        room.broadcast_to_role(roles::Role::Moderator, &hint);
    }
    assert_eq!(room.metrics.render_prometheus(0, 0, 0), before_hints);
}

#[test]
fn runtime_room_queue_failures_reach_shared_process_metrics() {
    let metrics = ServerMetrics::new();
    let mut room =
        Room::new_with_observers(rid("room"), None, false, None, None, None, metrics.clone());
    // This helper intentionally drops its receiver, simulating a departed socket.
    let entry = participant(
        "closed",
        roles::Role::Guest,
        moderation::PunitiveState::default(),
        None,
    );
    room.participants.insert(entry.id, entry);
    room.broadcast_all(&ServerMessage::RoomClosed {
        reason: "Room closed".into(),
    });
    assert!(
        metrics
            .render_prometheus(0, 0, 0)
            .lines()
            .any(|line| line == "simplestchat_outbound_queue_closed_total 1")
    );
}

#[test]
fn successful_broadcast_enqueue_does_not_count_as_failure_or_socket_write() {
    let mut room = Room::new(rid("room"), None, false, None);
    let (sender, mut receiver) = mpsc::channel(1);
    let mut entry = participant(
        "owner",
        roles::Role::Owner,
        moderation::PunitiveState::default(),
        None,
    );
    entry.sender = sender;
    room.participants.insert(entry.id, entry);
    let event = ServerMessage::RoomClosed {
        reason: "Room closed".into(),
    };
    let before = room.metrics.render_prometheus(0, 0, 0);
    room.broadcast_all(&event);
    assert_eq!(
        receiver.try_recv().unwrap().as_str(),
        serde_json::to_string(&event).unwrap()
    );
    assert_eq!(room.metrics.render_prometheus(0, 0, 0), before);
}

#[test]
fn aggregate_room_chat_budget_is_bounded_and_recovers() {
    let mut room = Room::new(rid("room"), None, false, None);
    let start = std::time::Instant::now();
    for _ in 0..MAX_ROOM_CHAT_MESSAGES_PER_WINDOW {
        assert!(room.chat_broadcasts.reserve(start));
    }
    assert!(!room.chat_broadcasts.reserve(start));
    assert!(room.chat_broadcasts.reserve(start + ROOM_CHAT_WINDOW));
    assert_eq!(room.chat_broadcasts.len(), 1);
}

#[test]
fn aggregate_room_media_budget_is_bounded_and_recovers() {
    let mut room = Room::new(rid("room"), None, false, None);
    let start = std::time::Instant::now();
    for _ in 0..MAX_ROOM_MEDIA_MUTATIONS_PER_WINDOW {
        assert!(room.media_mutations.reserve(start));
    }
    assert!(!room.media_mutations.reserve(start));
    assert!(
        room.media_mutations
            .reserve(start + ROOM_MEDIA_MUTATION_WINDOW)
    );
    assert_eq!(room.media_mutations.len(), 1);
}

#[test]
fn no_op_media_mutation_refund_preserves_aggregate_capacity() {
    let mut room = Room::new(rid("room"), None, false, None);
    let start = std::time::Instant::now();
    for _ in 0..(MAX_ROOM_MEDIA_MUTATIONS_PER_WINDOW * 2) {
        assert!(room.media_mutations.reserve(start));
        assert!(room.media_mutations.refund(start));
    }
    assert!(room.media_mutations.is_empty());
    for _ in 0..MAX_ROOM_MEDIA_MUTATIONS_PER_WINDOW {
        assert!(room.media_mutations.reserve(start));
    }
    assert!(!room.media_mutations.reserve(start));
}

#[tokio::test]
async fn media_mutation_refund_targets_original_room_generation() {
    let start = std::time::Instant::now();
    let original = Arc::new(TokioRwLock::new(Room::new(rid("room"), None, false, None)));
    let replacement = Arc::new(TokioRwLock::new(Room::new(rid("room"), None, false, None)));
    assert!(original.write().await.media_mutations.reserve(start));
    assert!(replacement.write().await.media_mutations.reserve(start));

    RoomManager::refund_media_mutation(MediaMutationReservation {
        room: original.clone(),
        reserved_at: start,
    })
    .await;

    assert!(original.read().await.media_mutations.is_empty());
    assert_eq!(replacement.read().await.media_mutations.len(), 1);
}

#[test]
fn aggregate_room_media_control_ipc_budget_is_bounded_and_recovers() {
    let mut room = Room::new(rid("room"), None, false, None);
    let start = std::time::Instant::now();
    for _ in 0..MAX_ROOM_MEDIA_CONTROL_IPC_PER_WINDOW {
        assert!(room.media_control_ipc.reserve(start));
    }
    assert!(!room.media_control_ipc.reserve(start));
    assert!(
        room.media_control_ipc
            .reserve(start + ROOM_MEDIA_CONTROL_IPC_WINDOW)
    );
    assert_eq!(room.media_control_ipc.len(), 1);
}

#[test]
fn no_op_media_control_ipc_refund_preserves_aggregate_capacity() {
    let mut room = Room::new(rid("room"), None, false, None);
    let start = std::time::Instant::now();
    for _ in 0..(MAX_ROOM_MEDIA_CONTROL_IPC_PER_WINDOW * 2) {
        assert!(room.media_control_ipc.reserve(start));
        assert!(room.media_control_ipc.refund(start));
    }
    assert!(room.media_control_ipc.is_empty());
    for _ in 0..MAX_ROOM_MEDIA_CONTROL_IPC_PER_WINDOW {
        assert!(room.media_control_ipc.reserve(start));
    }
    assert!(!room.media_control_ipc.reserve(start));
}

#[test]
fn aggregate_room_admin_budget_is_bounded_and_recovers() {
    let mut room = Room::new(rid("room"), None, false, None);
    let start = std::time::Instant::now();
    for _ in 0..MAX_ROOM_ADMIN_MUTATIONS_PER_WINDOW {
        assert!(room.admin_mutations.reserve(start));
    }
    assert!(!room.admin_mutations.reserve(start));
    assert!(
        room.admin_mutations
            .reserve(start + ROOM_ADMIN_MUTATION_WINDOW)
    );
    assert_eq!(room.admin_mutations.len(), 1);
}

#[test]
fn aggregate_room_voice_request_budget_is_bounded_and_recovers() {
    let mut room = Room::new(rid("room"), None, false, None);
    let start = std::time::Instant::now();
    for _ in 0..MAX_ROOM_VOICE_REQUESTS_PER_WINDOW {
        assert!(room.voice_requests.reserve(start));
    }
    assert!(!room.voice_requests.reserve(start));
    assert!(
        room.voice_requests
            .reserve(start + ROOM_VOICE_REQUEST_WINDOW)
    );
    assert_eq!(room.voice_requests.len(), 1);
}

#[test]
fn shared_join_budget_aggregates_sockets_and_ipv6_addresses() {
    let limiter = SharedRateLimiter::new(2);
    let start = std::time::Instant::now();
    let first = crate::net::cohort("2001:db8:1:2::1".parse().unwrap());
    let rotated = crate::net::cohort("2001:db8:1:2:ffff::42".parse().unwrap());
    assert_eq!(first, rotated);
    assert!(limiter.allow(first, start));
    assert!(limiter.allow(rotated, start));
    assert!(!limiter.allow(first, start));
    assert!(limiter.allow(first, start + JOIN_RATE_WINDOW));

    let mapped_first = crate::net::cohort("::ffff:192.0.2.1".parse().unwrap());
    let mapped_second = crate::net::cohort("::ffff:192.0.2.2".parse().unwrap());
    assert_eq!(mapped_first, "192.0.2.1".parse::<IpAddr>().unwrap());
    assert_ne!(mapped_first, mapped_second);
}

#[test]
fn failed_join_cleanup_evicts_only_the_last_unpopulated_reservation() {
    let mut abandoned = Room::new(rid("abandoned"), None, false, None);
    abandoned.pending_joins = 1;
    assert!(release_join_reservation(&mut abandoned));
    assert_eq!(abandoned.pending_joins, 0);

    let mut concurrent = Room::new(rid("concurrent"), None, false, None);
    concurrent.pending_joins = 2;
    // One failed join must not evict the room while another join is still
    // performing its authorization/password work.
    assert!(!release_join_reservation(&mut concurrent));
    assert_eq!(concurrent.pending_joins, 1);

    concurrent.participants.insert(
        pid("winner"),
        participant(
            "winner",
            roles::Role::Guest,
            moderation::PunitiveState::default(),
            Some(IpAddr::from([192, 0, 2, 9])),
        ),
    );
    // Once the concurrent join succeeds, releasing its reservation leaves
    // an occupied room and therefore cannot authorize teardown.
    assert!(!release_join_reservation(&mut concurrent));
    assert_eq!(concurrent.pending_joins, 0);
    assert!(concurrent.participants.contains_key(&pid("winner")));
}

#[test]
fn join_capacity_preserves_all_live_windows_and_bounds_overflow() {
    let limiter = SharedRateLimiter::new(1);
    let now = std::time::Instant::now();
    for key in 0..MAX_TRACKED_JOIN_KEYS {
        assert!(limiter.allow(key, now));
    }
    assert!(limiter.allow(MAX_TRACKED_JOIN_KEYS, now));
    assert!(!limiter.allow(MAX_TRACKED_JOIN_KEYS + 1, now));
    for key in 0..MAX_TRACKED_JOIN_KEYS {
        assert!(!limiter.allow(key, now));
    }
    assert_eq!(limiter.tracked_keys(), MAX_TRACKED_JOIN_KEYS);
    let table = limiter.table.lock();
    assert_eq!(table.order.len(), MAX_TRACKED_JOIN_KEYS);
    drop(table);
    assert!(limiter.allow(0, now + JOIN_RATE_WINDOW));
    assert_eq!(limiter.tracked_keys(), 1);
}

#[test]
fn join_rate_limiter_reclaims_expired_windows_before_using_capacity() {
    let limiter = SharedRateLimiter::new(1);
    let t0 = std::time::Instant::now();
    let later = |secs: u64| t0 + std::time::Duration::from_secs(secs);
    for key in 0..5_u32 {
        assert!(limiter.allow(key, t0));
    }
    assert!(limiter.allow(5, later(61)));
    assert_eq!(
        limiter.tracked_keys(),
        1,
        "expired windows are reclaimed on insert"
    );

    // Saturate the table with stale windows, then keep one recent key at
    // its limit: capacity pressure must evict stale windows, not that key.
    for key in 1000..(1000 + MAX_TRACKED_JOIN_KEYS as u32) {
        limiter.allow(key, later(100));
    }
    assert!(limiter.allow(7, later(150)));
    assert!(!limiter.allow(7, later(150)));
    for key in 20_000..20_010_u32 {
        assert!(limiter.allow(key, later(161)));
    }
    assert!(
        !limiter.allow(7, later(161)),
        "a recent limited key survives eviction"
    );
    assert!(limiter.tracked_keys() <= 11);
}

fn sorted(mut ids: Vec<ParticipantId>) -> Vec<ParticipantId> {
    ids.sort();
    ids
}

fn participant(
    id: &str,
    role: roles::Role,
    punitive: moderation::PunitiveState,
    ip: Option<IpAddr>,
) -> Participant {
    let (sender, _receiver) = mpsc::channel(4);
    Participant {
        id: pid(id),
        social: social::ParticipantSocial::new(0),
        name: id.to_string(),
        sender,
        media_session_id: uuid::Uuid::new_v4(),
        producers: HashMap::new(),
        role,
        punitive,
        authenticated: role != roles::Role::Guest,
        ip,
        chat_style: Default::default(),
    }
}

#[test]
fn persisted_first_joiner_never_inherits_owner() {
    assert_eq!(
        select_join_role(true, true, Some(roles::Role::Guest), false),
        Some(roles::Role::Guest)
    );
    assert_eq!(select_join_role(true, true, None, true), None);
    assert_eq!(
        select_join_role(false, true, None, false),
        Some(roles::Role::Owner)
    );
}

#[test]
fn guest_sanctions_survive_a_new_connection_identity() {
    let ip: IpAddr = "203.0.113.7".parse().unwrap();
    assert_eq!(
        SanctionKey::for_identity(&pid("old-socket"), false, Some(ip)),
        SanctionKey::for_identity(&pid("new-socket"), false, Some(ip))
    );
    assert_ne!(
        SanctionKey::for_identity(&pid("old-socket"), false, None),
        SanctionKey::for_identity(&pid("new-socket"), false, None)
    );

    let merged = merge_punitive_state(
        moderation::PunitiveState {
            cam_banned: false,
            text_muted: true,
        },
        Some(&moderation::PunitiveState {
            cam_banned: true,
            text_muted: false,
        }),
    );
    assert!(merged.cam_banned && merged.text_muted);
}

#[test]
fn unchanged_punitive_cohort_is_detected_without_mutation() {
    let mut room = Room::new(rid("room"), None, false, None);
    let target_id = pid("target");
    let key = SanctionKey::User(target_id.to_string());
    let muted = moderation::PunitiveState {
        cam_banned: false,
        text_muted: true,
    };
    room.participants.insert(
        target_id,
        participant(
            &target_id.to_string(),
            roles::Role::User,
            muted.clone(),
            None,
        ),
    );
    room.sanctions.insert(key.clone(), muted);

    assert!(punitive_cohort_matches(
        &room,
        std::slice::from_ref(&target_id),
        &[],
        &key,
        moderation::PunitiveKind::Muted,
        true,
    ));
    assert!(!punitive_cohort_matches(
        &room,
        std::slice::from_ref(&target_id),
        &[],
        &key,
        moderation::PunitiveKind::Muted,
        false,
    ));

    room.sanctions.remove(&key);
    room.participants
        .get_mut(&target_id)
        .unwrap()
        .punitive
        .text_muted = false;
    assert!(punitive_cohort_matches(
        &room,
        std::slice::from_ref(&target_id),
        &[],
        &key,
        moderation::PunitiveKind::Muted,
        false,
    ));
}

#[test]
fn guest_ip_moderation_covers_every_unauthenticated_socket() {
    let ip: IpAddr = "203.0.113.7".parse().unwrap();
    let other_ip: IpAddr = "203.0.113.8".parse().unwrap();
    let mut room = Room::new(rid("room"), None, false, None);

    room.participants.insert(
        pid("target"),
        participant(
            "target",
            roles::Role::Guest,
            moderation::PunitiveState::default(),
            Some(ip),
        ),
    );
    let mut peer = participant(
        "peer",
        roles::Role::Moderator,
        moderation::PunitiveState::default(),
        Some(ip),
    );
    peer.authenticated = false;
    room.participants.insert(pid("peer"), peer);
    room.participants.insert(
        pid("registered"),
        participant(
            "registered",
            roles::Role::User,
            moderation::PunitiveState::default(),
            Some(ip),
        ),
    );
    room.participants.insert(
        pid("other"),
        participant(
            "other",
            roles::Role::Guest,
            moderation::PunitiveState::default(),
            Some(other_ip),
        ),
    );
    let (lobby_sender, _lobby_receiver) = mpsc::channel(4);
    room.lobby.insert(
        pid("waiting"),
        LobbyEntry {
            participant_id: pid("waiting"),
            name: "waiting".to_string(),
            sender: lobby_sender,
            media_session_id: uuid::Uuid::new_v4(),
            authenticated: false,
            reconnect_token: "token".to_string(),
            in_lobby_flag: Arc::new(AtomicBool::new(true)),
            ip: Some(ip),
            role: roles::Role::Moderator,
            punitive: moderation::PunitiveState::default(),
            chat_style: Default::default(),
        },
    );

    assert!(moderation_cohort(&room, &pid("target"), roles::Role::Moderator).is_err());
    assert!(guest_ip_lobby_cohort(&room, false, Some(ip), roles::Role::Moderator).is_err());

    room.participants.get_mut(&pid("peer")).unwrap().role = roles::Role::User;
    room.lobby.get_mut(&pid("waiting")).unwrap().role = roles::Role::User;
    let cohort = moderation_cohort(&room, &pid("target"), roles::Role::Moderator).unwrap();
    assert_eq!(
        sorted(cohort.clone()),
        sorted(vec![pid("peer"), pid("target")])
    );
    let lobby_cohort =
        guest_ip_lobby_cohort(&room, false, Some(ip), roles::Role::Moderator).unwrap();
    assert_eq!(lobby_cohort, vec![pid("waiting")]);

    let banned_ids: Vec<ParticipantId> =
        cohort.iter().chain(lobby_cohort.iter()).cloned().collect();
    record_banned_cohort(&mut room, &banned_ids, false, Some(ip), None);
    assert!(room.identity_is_banned(&pid("target"), false, Some(ip)));
    assert!(room.identity_is_banned(&pid("preopened-guest"), false, Some(ip)));
    assert!(!room.identity_is_banned(&pid("registered"), true, Some(ip)));
    assert!(room.banned_guest_participants.contains(&pid("target")));
    assert!(room.banned_guest_participants.contains(&pid("peer")));
    assert!(room.banned_guest_participants.contains(&pid("waiting")));
}

#[test]
fn guest_ipv6_moderation_matches_one_64_but_not_an_adjacent_prefix() {
    let first: IpAddr = "2001:db8:1:2::1".parse().unwrap();
    let rotated: IpAddr = "2001:db8:1:2:ffff:ffff:ffff:42".parse().unwrap();
    let adjacent: IpAddr = "2001:db8:1:3::1".parse().unwrap();
    let mut room = Room::new(rid("room"), None, false, None);

    for (id, ip) in [
        ("target", first),
        ("rotated", rotated),
        ("adjacent", adjacent),
    ] {
        room.participants.insert(
            pid(id),
            participant(
                id,
                roles::Role::Guest,
                moderation::PunitiveState::default(),
                Some(ip),
            ),
        );
    }

    let cohort = moderation_cohort(&room, &pid("target"), roles::Role::Moderator).unwrap();
    assert_eq!(
        sorted(cohort.clone()),
        sorted(vec![pid("rotated"), pid("target")])
    );

    record_banned_cohort(&mut room, &cohort, false, Some(first), None);
    assert!(room.identity_is_banned(&pid("new-socket"), false, Some(rotated)));
    assert!(!room.identity_is_banned(&pid("outside-prefix"), false, Some(adjacent)));
    assert_eq!(
        SanctionKey::for_identity(&pid("first"), false, Some(first)),
        SanctionKey::for_identity(&pid("rotated"), false, Some(rotated))
    );
    assert_ne!(
        SanctionKey::for_identity(&pid("first"), false, Some(first)),
        SanctionKey::for_identity(&pid("adjacent"), false, Some(adjacent))
    );
}

#[test]
fn guest_ip_punitive_state_is_applied_consistently_to_the_cohort() {
    let ip: IpAddr = "203.0.113.7".parse().unwrap();
    let mut room = Room::new(rid("room"), None, false, None);
    room.participants.insert(
        pid("first"),
        participant(
            "first",
            roles::Role::Guest,
            moderation::PunitiveState {
                cam_banned: false,
                text_muted: true,
            },
            Some(ip),
        ),
    );
    room.participants.insert(
        pid("second"),
        participant(
            "second",
            roles::Role::Guest,
            moderation::PunitiveState {
                cam_banned: true,
                text_muted: false,
            },
            Some(ip),
        ),
    );
    let (lobby_sender, _lobby_receiver) = mpsc::channel(4);
    room.lobby.insert(
        pid("waiting"),
        LobbyEntry {
            participant_id: pid("waiting"),
            name: "waiting".to_string(),
            sender: lobby_sender,
            media_session_id: uuid::Uuid::new_v4(),
            authenticated: false,
            reconnect_token: "token".to_string(),
            in_lobby_flag: Arc::new(AtomicBool::new(true)),
            ip: Some(ip),
            role: roles::Role::Guest,
            punitive: moderation::PunitiveState {
                cam_banned: true,
                text_muted: true,
            },
            chat_style: Default::default(),
        },
    );
    let cohort = vec![pid("first"), pid("second")];
    let lobby_cohort = vec![pid("waiting")];

    let punitive = apply_punitive_to_cohort(
        &mut room,
        &cohort,
        &lobby_cohort,
        SanctionKey::GuestIp(ip),
        moderation::PunitiveKind::Muted,
        false,
    );
    assert_eq!(
        punitive,
        moderation::PunitiveState {
            cam_banned: true,
            text_muted: false,
        }
    );
    assert_eq!(room.participants[&pid("first")].punitive, punitive);
    assert_eq!(room.participants[&pid("second")].punitive, punitive);
    assert_eq!(room.lobby[&pid("waiting")].punitive, punitive);

    apply_punitive_to_cohort(
        &mut room,
        &cohort,
        &lobby_cohort,
        SanctionKey::GuestIp(ip),
        moderation::PunitiveKind::CamBanned,
        false,
    );
    assert!(!room.sanctions.contains_key(&SanctionKey::GuestIp(ip)));
    assert_eq!(
        room.participants[&pid("first")].punitive,
        moderation::PunitiveState::default()
    );
    assert_eq!(
        room.participants[&pid("second")].punitive,
        moderation::PunitiveState::default()
    );
    assert_eq!(
        room.lobby[&pid("waiting")].punitive,
        moderation::PunitiveState::default()
    );
}

#[test]
fn expired_guest_ban_pruning_clears_participant_origin_marker() {
    let mut room = Room::new(rid("room"), None, false, None);
    room.banned_participants.insert(
        pid("guest"),
        Some(std::time::Instant::now() - std::time::Duration::from_secs(1)),
    );
    room.banned_guest_participants.insert(pid("guest"));

    room.prune_expired_bans(std::time::Instant::now());
    assert!(!room.banned_participants.contains_key(&pid("guest")));
    assert!(!room.banned_guest_participants.contains(&pid("guest")));
}

#[test]
fn stale_join_policy_snapshot_is_rejected() {
    let mut room = Room::new(rid("room"), None, false, None);
    let snapshot = room.policy_revision;
    assert!(policy_snapshot_matches(&room, snapshot));
    room.policy_revision = room.policy_revision.wrapping_add(1);
    assert!(!policy_snapshot_matches(&room, snapshot));
}

#[test]
fn deletion_tombstone_rejects_in_flight_room_commits() {
    let mut room = Room::new(rid("room"), None, true, None);
    let snapshot = room.policy_revision;
    room.deleting = true;
    room.policy_revision = room.policy_revision.wrapping_add(1);

    assert!(room.ensure_live().is_err());
    assert!(!policy_snapshot_matches(&room, snapshot));
}

#[test]
fn only_the_owning_delete_task_releases_a_room_reservation() {
    let deleting = RwLock::new(HashMap::new());
    let owner = uuid::Uuid::new_v4();
    deleting.write().insert(rid("room"), owner);

    release_deletion_reservation(&deleting, &rid("room"), uuid::Uuid::new_v4());
    assert_eq!(deleting.read().get("room"), Some(&owner));

    release_deletion_reservation(&deleting, &rid("room"), owner);
    assert!(!deleting.read().contains_key("room"));
}

#[test]
fn failed_router_teardown_retains_the_room_reservation() {
    let deleting = RwLock::new(HashMap::new());
    let owner = uuid::Uuid::new_v4();
    deleting.write().insert(rid("room"), owner);

    release_deletion_reservation_after_router_teardown(&deleting, &rid("room"), owner, false);
    assert_eq!(deleting.read().get("room"), Some(&owner));

    release_deletion_reservation_after_router_teardown(&deleting, &rid("room"), owner, true);
    assert!(!deleting.read().contains_key("room"));
}

#[test]
fn sender_and_media_session_identify_one_room_incarnation() {
    let (old_sender, _old_receiver) = mpsc::channel(1);
    let (new_sender, _new_receiver) = mpsc::channel(1);
    let media_session_id = uuid::Uuid::new_v4();
    let mut room = Room::new(rid("room"), None, false, None);
    let mut member = participant(
        "member",
        roles::Role::User,
        moderation::PunitiveState::default(),
        None,
    );
    member.sender = old_sender.clone();
    member.media_session_id = media_session_id;
    room.participants.insert(pid("member"), member);

    assert!(RoomManager::participant_for_sender(&room, &pid("member"), &old_sender).is_ok());
    assert!(RoomManager::participant_for_sender(&room, &pid("member"), &new_sender).is_err());
    assert!(RoomManager::participant_session_is_current(
        &room,
        &pid("member"),
        &old_sender,
        media_session_id,
    ));
    assert!(!RoomManager::participant_session_is_current(
        &room,
        &pid("member"),
        &old_sender,
        uuid::Uuid::new_v4(),
    ));
}

#[test]
fn media_namespaces_do_not_alias_same_uuid_rejoins() {
    let participant_id = ParticipantId::random();
    let first =
        RoomManager::media_participant_id(&rid("room"), &participant_id, uuid::Uuid::new_v4());
    let second =
        RoomManager::media_participant_id(&rid("room"), &participant_id, uuid::Uuid::new_v4());
    assert_ne!(first, second);
}

#[test]
fn lobby_admission_preserves_resolved_role_and_sanctions() {
    let participant_id = ParticipantId::random();
    let (sender, _receiver) = mpsc::channel(4);
    let mut room = Room::new(rid("room"), None, false, None);
    room.sanctions.insert(
        SanctionKey::User(participant_id.to_string()),
        moderation::PunitiveState {
            cam_banned: true,
            text_muted: false,
        },
    );
    let entry = LobbyEntry {
        participant_id,
        name: "member".to_string(),
        sender,
        media_session_id: uuid::Uuid::new_v4(),
        authenticated: true,
        reconnect_token: "token".to_string(),
        in_lobby_flag: Arc::new(AtomicBool::new(true)),
        ip: None,
        role: roles::Role::Member,
        punitive: moderation::PunitiveState {
            cam_banned: false,
            text_muted: true,
        },
        chat_style: Default::default(),
    };
    let (role, punitive) = lobby_admission_state(&room, &entry);
    assert_eq!(role, roles::Role::Member);
    assert!(punitive.cam_banned && punitive.text_muted);
}

#[test]
fn restrictive_policy_removes_existing_unauthorized_producers() {
    let mut settings = settings::RoomSettings::defaults(&rid("room"));
    settings.guests_can_broadcast = false;
    let mut room = Room::new(rid("room"), Some(settings), false, None);
    let mut guest = participant(
        "guest",
        roles::Role::Guest,
        moderation::PunitiveState::default(),
        Some("203.0.113.8".parse().unwrap()),
    );
    guest.producers.insert(
        prid("microphone"),
        ProducerRecord {
            kind: MediaKind::Audio,
            source: Some("microphone".to_string()),
        },
    );
    room.producer_to_participant
        .insert(prid("microphone"), pid("guest"));
    room.participants.insert(pid("guest"), guest);

    let revoked = RoomManager::revoke_unauthorized_producers(&mut room, None, "policy changed");
    assert_eq!(revoked.len(), 1);
    assert!(room.participants[&pid("guest")].producers.is_empty());
    assert!(
        !room
            .producer_to_participant
            .contains_key(&prid("microphone"))
    );
}

#[test]
fn moderated_role_demotion_removes_existing_producers() {
    let mut settings = settings::RoomSettings::defaults(&rid("room"));
    settings.moderated = true;
    let mut room = Room::new(rid("room"), Some(settings), false, None);
    let mut member = participant(
        "member",
        roles::Role::User,
        moderation::PunitiveState::default(),
        None,
    );
    member.producers.insert(
        prid("camera"),
        ProducerRecord {
            kind: MediaKind::Video,
            source: Some("camera".to_string()),
        },
    );
    room.participants.insert(pid("member"), member);

    let revoked =
        RoomManager::revoke_unauthorized_producers(&mut room, Some(&pid("member")), "role changed");
    assert_eq!(revoked.len(), 1);
    assert!(room.participants[&pid("member")].producers.is_empty());
}

#[test]
fn media_source_must_match_kind_and_cam_ban_uses_kind() {
    assert!(RoomManager::valid_media_source(
        MediaKind::Audio,
        "microphone"
    ));
    assert!(RoomManager::valid_media_source(MediaKind::Video, "camera"));
    assert!(!RoomManager::valid_media_source(
        MediaKind::Video,
        "microphone"
    ));

    let (sender, _receiver) = mpsc::channel(1);
    let participant = Participant {
        id: ParticipantId::random(),
        social: social::ParticipantSocial::new(0),
        name: "test".to_string(),
        sender,
        media_session_id: uuid::Uuid::new_v4(),
        producers: HashMap::new(),
        role: roles::Role::Guest,
        punitive: moderation::PunitiveState {
            cam_banned: true,
            text_muted: false,
        },
        authenticated: false,
        ip: None,
        chat_style: Default::default(),
    };
    let room = Room::new(rid("room"), None, false, None);
    assert!(!RoomManager::participant_can_produce(
        &room,
        &participant,
        MediaKind::Video,
        "camera"
    ));
    assert!(RoomManager::participant_can_produce(
        &room,
        &participant,
        MediaKind::Audio,
        "microphone"
    ));
}

#[test]
fn guest_broadcast_and_chat_settings_are_enforceable() {
    let (sender, _receiver) = mpsc::channel(1);
    let participant = Participant {
        id: ParticipantId::random(),
        social: social::ParticipantSocial::new(0),
        name: "guest".to_string(),
        sender,
        media_session_id: uuid::Uuid::new_v4(),
        producers: HashMap::new(),
        role: roles::Role::Guest,
        punitive: moderation::PunitiveState::default(),
        authenticated: false,
        ip: None,
        chat_style: Default::default(),
    };
    let mut settings = settings::RoomSettings::defaults(&rid("room"));
    settings.guests_can_broadcast = false;
    settings.allow_chat = false;
    let room = Room::new(rid("room"), Some(settings), false, None);
    assert!(!RoomManager::participant_can_produce(
        &room,
        &participant,
        MediaKind::Audio,
        "microphone"
    ));
    assert!(!room.settings.as_ref().unwrap().allow_chat);
}
