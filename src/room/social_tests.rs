//! Unit tests of the social dispatcher: chat, private messages, reports, members, bans,
//! pins and reactions against in-memory rooms (database-backed cases are `#[ignore]`).

use super::*;
use crate::ids::fixtures::{mid, pid, rid};
use serde_json::{Value, json};

fn message(json: &str) -> ClientMessage {
    serde_json::from_str(json).unwrap()
}

#[test]
fn uncertain_chat_storage_never_becomes_a_definite_rejection() {
    let metrics = ServerMetrics::new();
    let (sender, mut receiver) = mpsc::channel(1);
    chat_persistence_failure(
        &metrics,
        &sender,
        "uncertain",
        sqlx::Error::Io(std::io::Error::from(std::io::ErrorKind::ConnectionAborted)),
    )
    .unwrap();
    let result: Value = serde_json::from_str(&receiver.try_recv().unwrap()).unwrap();
    assert_eq!(result["type"], "messageRetryResult");
    assert_eq!(result["outcome"], "unknown");
    assert_eq!(result["reason"], "storage_unconfirmed");
    assert_eq!(result["clientMessageId"], "uncertain");
    let definite =
        chat_persistence_failure(&metrics, &sender, "rejected", sqlx::Error::PoolTimedOut)
            .unwrap_err();
    assert!(definite.downcast_ref::<SocialFailure>().is_some());
    assert!(receiver.try_recv().is_err());
    for _ in 0..2 {
        chat_persistence_failure(
            &metrics,
            &sender,
            "uncertain",
            sqlx::Error::Io(std::io::Error::from(std::io::ErrorKind::ConnectionAborted)),
        )
        .unwrap();
    }
    // Even an unavailable outbound queue cannot turn an unknown commit
    // into the SocialError which invites the client to submit a new ID.
}

#[test]
fn read_only_social_requests_reserve_no_room_budget() {
    for json in [
        r#"{"type":"getRoomSnapshot","requestId":"r"}"#,
        r#"{"type":"listRoomBans","requestId":"r"}"#,
        r#"{"type":"listRoomMembers","requestId":"r"}"#,
        r#"{"type":"listRoomReports","requestId":"r"}"#,
        r#"{"type":"setChatPreferences","requestId":"r","allowPrivateMessages":true,"ignoredParticipantIds":[]}"#,
    ] {
        assert_eq!(message(json).social_budget(), SocialBudget::None, "{json}");
    }
}

#[test]
fn room_wide_social_mutations_reserve_the_matching_budget() {
    assert_eq!(
        message(r#"{"type":"changeNickname","requestId":"r","nickname":"n"}"#).social_budget(),
        SocialBudget::ChatBroadcast
    );
    assert_eq!(
            message(r#"{"type":"setMemberRole","requestId":"r","targetUserId":"af63e84c-8602-00f0-78a1-05398d44ae24","role":"user"}"#)
                .social_budget(),
            SocialBudget::AdminMutation
        );
}

#[test]
fn report_cooldown_follows_identity_across_sessions() {
    let mut social = RoomSocial::default();
    let t0 = std::time::Instant::now();
    let later = |secs: u64| t0 + std::time::Duration::from_secs(secs);
    assert!(!social.report_cooldown_active("user:a", t0));
    social.note_report("user:a".into(), t0);
    assert!(social.report_cooldown_active("user:a", later(29)));
    assert!(!social.report_cooldown_active("user:b", later(29)));
    assert!(!social.report_cooldown_active("user:a", later(30)));

    let (base, _rx) = participant("guest");
    let guest = Participant {
        authenticated: false,
        ip: Some("203.0.113.9".parse().unwrap()),
        ..base
    };
    assert_eq!(reporter_key(&guest), "ip:203.0.113.9");
    let (member, _rx) = participant("member");
    assert_eq!(reporter_key(&member), format!("user:{}", member.id));
}

#[test]
fn private_messages_do_not_consume_the_public_chat_window() {
    let mut room = Room::new(rid("room"), None, false, None);
    let (alice, _alice_rx) = participant("alice");
    let (bob, _bob_rx) = participant("bob");
    let (alice_id, alice_sender) = (alice.id, alice.sender.clone());
    let bob_id = bob.id;
    room.participants.insert(alice_id, alice);
    room.participants.insert(bob_id, bob);

    for index in 0..MAX_ROOM_CHAT_MESSAGES_PER_WINDOW {
        RoomManager::process_social_chat(
            &mut room,
            &alice_id,
            &alice_sender,
            format!("private {index}"),
            None,
            Some(&bob_id),
        )
        .unwrap();
    }
    assert!(room.chat_broadcasts.is_empty());
    RoomManager::process_social_chat(
        &mut room,
        &alice_id,
        &alice_sender,
        "public".to_string(),
        None,
        None,
    )
    .unwrap();
    assert_eq!(room.chat_broadcasts.len(), 1);
}

fn participant(name: &str) -> (Participant, mpsc::Receiver<crate::OutboundJson>) {
    let (sender, receiver) = mpsc::channel(16);
    (
        Participant {
            id: ParticipantId::random(),
            name: name.to_string(),
            sender,
            media_session_id: Uuid::new_v4(),
            producers: HashMap::new(),
            role: roles::Role::User,
            punitive: moderation::PunitiveState::default(),
            authenticated: true,
            ip: None,
            social: ParticipantSocial::new(0),
            chat_style: Default::default(),
        },
        receiver,
    )
}

fn fixture() -> (
    Room,
    Participant,
    Participant,
    Participant,
    Vec<mpsc::Receiver<crate::OutboundJson>>,
) {
    let (alice, arx) = participant("Alice");
    let (bob, brx) = participant("Bob");
    let (carol, crx) = participant("Carol");
    let mut room = Room::new(rid("social"), None, false, None);
    for participant in [&alice, &bob, &carol] {
        room.participants
            .insert(participant.id, participant.clone());
    }
    (room, alice, bob, carol, vec![arx, brx, crx])
}

fn chat(
    room: &mut Room,
    sender: &Participant,
    id: &str,
    target: Option<&ParticipantId>,
) -> Result<()> {
    RoomManager::process_social_chat(
        room,
        &sender.id,
        &sender.sender,
        "hello".into(),
        Some(id.into()),
        target,
    )
}

#[test]
fn typing_reaches_the_conversations_recipients_only_and_is_throttled() {
    let (mut room, alice, bob, carol, mut receivers) = fixture();
    room.participants
        .get_mut(&carol.id)
        .unwrap()
        .social
        .ignored
        .insert(alice.id);
    room.relay_typing(&alice.id, &alice.sender, None).unwrap();
    let event: Value = serde_json::from_str(&receivers[1].try_recv().unwrap()).unwrap();
    assert_eq!(event["type"], "participantTyping");
    assert_eq!(event["participantId"], alice.id.to_string());
    assert!(event.get("targetParticipantId").is_none());
    assert!(receivers[0].try_recv().is_err(), "never back to the sender");
    assert!(
        receivers[2].try_recv().is_err(),
        "an ignoring peer hears nothing"
    );
    room.relay_typing(&alice.id, &alice.sender, None).unwrap();
    assert!(
        receivers[1].try_recv().is_err(),
        "a second notice within the interval is dropped"
    );

    let rearm = |room: &mut Room| {
        room.participants
            .get_mut(&alice.id)
            .unwrap()
            .social
            .last_typing = None;
    };
    rearm(&mut room);
    room.relay_typing(&alice.id, &alice.sender, Some(&bob.id))
        .unwrap();
    let event: Value = serde_json::from_str(&receivers[1].try_recv().unwrap()).unwrap();
    assert_eq!(event["targetParticipantId"], bob.id.to_string());
    assert!(
        receivers[2].try_recv().is_err(),
        "private typing stays private"
    );
    room.participants
        .get_mut(&bob.id)
        .unwrap()
        .social
        .allow_private_messages = false;
    rearm(&mut room);
    room.relay_typing(&alice.id, &alice.sender, Some(&bob.id))
        .unwrap();
    assert!(
        receivers[1].try_recv().is_err(),
        "a closed inbox hears nothing"
    );
    rearm(&mut room);
    room.relay_typing(&alice.id, &alice.sender, Some(&pid("nobody")))
        .unwrap();
    room.participants
        .get_mut(&alice.id)
        .unwrap()
        .punitive
        .text_muted = true;
    rearm(&mut room);
    room.relay_typing(&alice.id, &alice.sender, None).unwrap();
    assert!(
        receivers[1].try_recv().is_err(),
        "a muted sender relays nothing"
    );
    assert!(
        room.relay_typing(&alice.id, &bob.sender, None).is_err(),
        "another socket cannot type as the sender"
    );
}

#[test]
fn public_delivery_is_acknowledged_once_and_ignores_apply_to_replay() {
    let (mut room, alice, bob, carol, mut receivers) = fixture();
    room.participants
        .get_mut(&bob.id)
        .unwrap()
        .social
        .ignored
        .insert(alice.id);
    chat(&mut room, &alice, "message-1", None).unwrap();
    let ack: Value = serde_json::from_str(&receivers[0].try_recv().unwrap()).unwrap();
    assert_eq!(ack["type"], "messageAck");
    assert!(
        ack["message"]["messageId"]
            .as_str()
            .unwrap()
            .parse::<Uuid>()
            .is_ok()
    );
    assert!(
        chrono::DateTime::parse_from_rfc3339(ack["message"]["sentAt"].as_str().unwrap()).is_ok()
    );
    assert!(receivers[0].try_recv().is_err());
    assert!(receivers[1].try_recv().is_err());
    let public: Value = serde_json::from_str(&receivers[2].try_recv().unwrap()).unwrap();
    assert_eq!(public["type"], "chatReceived");
    assert_eq!(public["messageId"], ack["message"]["messageId"]);
    let entry = room.social.history.front().unwrap();
    assert!(!visible(entry, &room.participants[&bob.id]));
    assert!(visible(entry, &carol));
}

#[test]
fn private_delivery_and_replay_never_reach_other_members() {
    let (mut room, alice, bob, carol, mut receivers) = fixture();
    chat(&mut room, &alice, "private-1", Some(&bob.id)).unwrap();
    assert!(receivers[0].try_recv().unwrap().contains("messageAck"));
    assert!(
        receivers[1]
            .try_recv()
            .unwrap()
            .contains("privateMessageReceived")
    );
    assert!(receivers[2].try_recv().is_err());
    let entry = room.social.history.front().unwrap();
    assert!(visible(entry, &alice));
    assert!(visible(entry, &bob));
    assert!(!visible(entry, &carol));
    assert_eq!(entry.message.recipient_name.as_deref(), Some("Bob"));
    let mut replacement = bob.clone();
    replacement.media_session_id = Uuid::new_v4();
    assert!(!visible(entry, &replacement));
}

#[test]
fn private_optout_ignore_and_missing_targets_share_no_delivery() {
    let (mut room, alice, bob, _, mut receivers) = fixture();
    room.participants
        .get_mut(&bob.id)
        .unwrap()
        .social
        .allow_private_messages = false;
    assert!(chat(&mut room, &alice, "one", Some(&bob.id)).is_err());
    room.participants
        .get_mut(&bob.id)
        .unwrap()
        .social
        .allow_private_messages = true;
    room.participants
        .get_mut(&bob.id)
        .unwrap()
        .social
        .ignored
        .insert(alice.id);
    assert!(chat(&mut room, &alice, "two", Some(&bob.id)).is_err());
    room.participants
        .get_mut(&bob.id)
        .unwrap()
        .social
        .ignored
        .clear();
    room.participants
        .get_mut(&alice.id)
        .unwrap()
        .social
        .ignored
        .insert(bob.id);
    assert!(chat(&mut room, &alice, "three", Some(&bob.id)).is_err());
    assert!(chat(&mut room, &alice, "four", Some(&ParticipantId::random())).is_err());
    assert!(room.social.history.is_empty());
    for receiver in &mut receivers {
        assert!(receiver.try_recv().is_err());
    }
}

#[test]
fn retry_is_idempotent_and_conflicting_message_id_is_rejected() {
    let (mut room, alice, bob, _, mut receivers) = fixture();
    chat(&mut room, &alice, "retry", Some(&bob.id)).unwrap();
    let first = receivers[0].try_recv().unwrap();
    chat(&mut room, &alice, "retry", Some(&bob.id)).unwrap();
    assert_eq!(first, receivers[0].try_recv().unwrap());
    receivers[1].try_recv().unwrap();
    assert!(receivers[1].try_recv().is_err());
    assert_eq!(room.social.history.len(), 1);
    assert!(chat(&mut room, &alice, "retry", None).is_err());
    assert!(
        RoomManager::process_social_chat(
            &mut room,
            &alice.id,
            &alice.sender,
            "changed".into(),
            Some("retry".into()),
            Some(&bob.id)
        )
        .is_err()
    );
}

fn sequenced_chat(
    room: &mut Room,
    sender: &Participant,
    id: &str,
    sequence: u64,
    target: Option<&ParticipantId>,
    retry: Option<String>,
) -> Result<()> {
    RoomManager::process_chat_attempt(
        room,
        &sender.id,
        &sender.sender,
        ChatAttempt {
            attachment_ids: Vec::new(),
            content: "hello".into(),
            client_message_id: Some(id.into()),
            recipient_id: target.copied(),
            sequence: Some(sequence),
            retry_session: retry,
            reply_to: None,
        },
    )
}

#[test]
fn chat_rejects_hidden_formatting_but_keeps_line_breaks_and_tabs() {
    let (mut room, alice, _, _, _) = fixture();
    let attempt = |content: &str| ChatAttempt {
        attachment_ids: Vec::new(),
        content: content.into(),
        client_message_id: None,
        recipient_id: None,
        sequence: None,
        retry_session: None,
        reply_to: None,
    };
    RoomManager::process_chat_attempt(
        &mut room,
        &alice.id,
        &alice.sender,
        attempt("line one\nline\ttwo \u{200F}عربي 👩🏽\u{200D}💻"),
    )
    .unwrap();
    for content in [
        "moc.elpmaxe\u{202E}",
        "https://ex\u{200B}ample.com",
        "\u{2066}aside\u{2069}",
        "\u{FEFF}bom",
    ] {
        let error = RoomManager::process_chat_attempt(
            &mut room,
            &alice.id,
            &alice.sender,
            attempt(content),
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("hidden formatting"),
            "{content:?}: {error}"
        );
    }
}

#[test]
fn receipts_survive_history_eviction_and_reack_without_redelivery() {
    let (mut room, alice, _, _, mut receivers) = fixture();
    let session = alice.social.chat_session_id.to_string();
    sequenced_chat(&mut room, &alice, "original", 1, None, None).unwrap();
    let ack = receivers[0].try_recv().unwrap();
    for receiver in receivers.iter_mut().skip(1) {
        receiver.try_recv().unwrap();
    }
    room.social.history.clear();
    room.social.history_bytes = 0;
    sequenced_chat(&mut room, &alice, "original", 1, None, Some(session)).unwrap();
    assert_eq!(receivers[0].try_recv().unwrap(), ack);
    for receiver in receivers.iter_mut().skip(1) {
        assert!(receiver.try_recv().is_err());
    }
    assert!(room.social.history.is_empty());
    assert_eq!(room.social.receipts.len(), 1);
}

#[test]
fn expired_receipts_keep_the_watermark_and_never_rebroadcast() {
    let (mut room, alice, _, _, mut receivers) = fixture();
    let session = alice.social.chat_session_id.to_string();
    sequenced_chat(&mut room, &alice, "original", 1, None, None).unwrap();
    for receiver in &mut receivers {
        receiver.try_recv().unwrap();
    }
    room.social
        .prune_receipts(std::time::Instant::now() + CHAT_RECEIPT_TTL);
    assert!(room.social.receipts.is_empty());
    assert_eq!(room.social.receipt_bytes, 0);
    sequenced_chat(&mut room, &alice, "original", 1, None, Some(session)).unwrap();
    let result: Value = serde_json::from_str(&receivers[0].try_recv().unwrap()).unwrap();
    assert_eq!(result["outcome"], "unknown");
    assert_eq!(result["reason"], "receipt_expired");
    for receiver in receivers.iter_mut().skip(1) {
        assert!(receiver.try_recv().is_err());
    }
    assert_eq!(room.social.history.len(), 1);
}

#[test]
fn an_unreceived_latest_public_attempt_can_be_retried_once() {
    let (mut room, alice, _, _, mut receivers) = fixture();
    let session = alice.social.chat_session_id.to_string();
    sequenced_chat(
        &mut room,
        &alice,
        "unreceived",
        1,
        None,
        Some(session.clone()),
    )
    .unwrap();
    sequenced_chat(
        &mut room,
        &alice,
        "unreceived",
        1,
        None,
        Some(session.clone()),
    )
    .unwrap();
    let first = receivers[0].try_recv().unwrap();
    assert_eq!(first, receivers[0].try_recv().unwrap());
    for receiver in receivers.iter_mut().skip(1) {
        receiver.try_recv().unwrap();
        assert!(receiver.try_recv().is_err());
    }
    sequenced_chat(&mut room, &alice, "later", 3, None, None).unwrap();
    for receiver in &mut receivers {
        receiver.try_recv().unwrap();
    }
    sequenced_chat(
        &mut room,
        &alice,
        "earlier-unreceived",
        2,
        None,
        Some(session),
    )
    .unwrap();
    let result: Value = serde_json::from_str(&receivers[0].try_recv().unwrap()).unwrap();
    assert_eq!(result["reason"], "sequence_superseded");
    for receiver in receivers.iter_mut().skip(1) {
        assert!(receiver.try_recv().is_err());
    }
}

#[test]
fn retry_is_bound_to_membership_but_survives_sender_rebinding() {
    let (mut room, alice, _, _, mut receivers) = fixture();
    let session = alice.social.chat_session_id.to_string();
    sequenced_chat(&mut room, &alice, "original", 1, None, None).unwrap();
    let original = receivers[0].try_recv().unwrap();
    for receiver in receivers.iter_mut().skip(1) {
        receiver.try_recv().unwrap();
    }
    let (replacement_sender, mut replacement_rx) = mpsc::channel(16);
    let actor = room.participants.get_mut(&alice.id).unwrap();
    actor.sender = replacement_sender;
    let rebound = actor.clone();
    sequenced_chat(
        &mut room,
        &rebound,
        "original",
        1,
        None,
        Some(session.clone()),
    )
    .unwrap();
    assert_eq!(original, replacement_rx.try_recv().unwrap());
    let actor = room.participants.get_mut(&alice.id).unwrap();
    actor.media_session_id = Uuid::new_v4();
    actor.social = ParticipantSocial::new(room.social.next_sequence);
    let fresh = actor.clone();
    sequenced_chat(&mut room, &fresh, "original", 1, None, Some(session)).unwrap();
    let result: Value = serde_json::from_str(&replacement_rx.try_recv().unwrap()).unwrap();
    assert_eq!(result["reason"], "session_changed");
    for receiver in receivers.iter_mut().skip(1) {
        assert!(receiver.try_recv().is_err());
    }
}

#[test]
fn private_retry_only_reconciles_a_receipt_never_a_replacement_recipient() {
    let (mut room, alice, bob, _, mut receivers) = fixture();
    let session = alice.social.chat_session_id.to_string();
    sequenced_chat(&mut room, &alice, "private", 1, Some(&bob.id), None).unwrap();
    let original = receivers[0].try_recv().unwrap();
    receivers[1].try_recv().unwrap();
    room.participants.get_mut(&bob.id).unwrap().media_session_id = Uuid::new_v4();
    sequenced_chat(
        &mut room,
        &alice,
        "private",
        1,
        Some(&bob.id),
        Some(session.clone()),
    )
    .unwrap();
    assert_eq!(original, receivers[0].try_recv().unwrap());
    sequenced_chat(
        &mut room,
        &alice,
        "missing-private",
        2,
        Some(&bob.id),
        Some(session),
    )
    .unwrap();
    let result: Value = serde_json::from_str(&receivers[0].try_recv().unwrap()).unwrap();
    assert_eq!(result["reason"], "recipient_unconfirmed");
    for receiver in receivers.iter_mut().skip(1) {
        assert!(receiver.try_recv().is_err());
    }
}

fn fill_receipt_cache(room: &mut Room, sender_session: Uuid, count: usize, bytes: usize) {
    let template = room.social.receipts.front().unwrap().message.clone();
    for index in 1..count {
        let mut message = template.clone();
        message.client_message_id = format!("cached-{index}");
        room.social.receipts.push_back(ChatReceipt {
            sender_session: if count > CHAT_RECEIPTS_PER_MEMBER {
                Uuid::new_v4()
            } else {
                sender_session
            },
            sequence: None,
            accepted_at: std::time::Instant::now(),
            bytes,
            message,
        });
        room.social.receipt_bytes += bytes;
    }
}

#[test]
fn full_receipt_cache_admits_new_chat_without_redelivering_evicted_attempts() {
    let (mut room, alice, _, _, mut receivers) = fixture();
    sequenced_chat(&mut room, &alice, "original", 1, None, None).unwrap();
    for receiver in &mut receivers {
        receiver.try_recv().unwrap();
    }
    let original_bytes = room.social.receipts.front().unwrap().bytes;
    // These are accepted entries from other memberships. The oldest room
    // entry belongs to Alice, while her own receipt budget still has space.
    fill_receipt_cache(&mut room, Uuid::new_v4(), CHAT_RECEIPTS, original_bytes);
    sequenced_chat(&mut room, &alice, "new", 2, None, None).unwrap();
    assert_eq!(room.social.receipts.len(), CHAT_RECEIPTS);
    assert_eq!(
        room.social
            .receipts
            .back()
            .unwrap()
            .message
            .client_message_id,
        "new"
    );
    assert!(room.social.receipt_bytes <= CHAT_RECEIPT_BYTES);
    for receiver in &mut receivers {
        receiver.try_recv().unwrap();
    }
    sequenced_chat(
        &mut room,
        &alice,
        "original",
        1,
        None,
        Some(alice.social.chat_session_id.to_string()),
    )
    .unwrap();
    let result: Value = serde_json::from_str(&receivers[0].try_recv().unwrap()).unwrap();
    assert_eq!(result["reason"], "sequence_superseded");
    for receiver in receivers.iter_mut().skip(1) {
        assert!(receiver.try_recv().is_err());
    }
    assert_eq!(room.social.history.len(), 2);
}

#[test]
fn member_and_byte_receipt_budgets_evict_oldest_before_new_confirmation() {
    let (mut room, alice, _, _, mut receivers) = fixture();
    sequenced_chat(&mut room, &alice, "original", 1, None, None).unwrap();
    for receiver in &mut receivers {
        receiver.try_recv().unwrap();
    }
    let original_bytes = room.social.receipts.front().unwrap().bytes;
    fill_receipt_cache(
        &mut room,
        alice.media_session_id,
        CHAT_RECEIPTS_PER_MEMBER,
        original_bytes,
    );
    sequenced_chat(&mut room, &alice, "new", 2, None, None).unwrap();
    assert_eq!(room.social.receipts.len(), CHAT_RECEIPTS_PER_MEMBER);
    assert_eq!(
        room.social
            .receipts
            .front()
            .unwrap()
            .message
            .client_message_id,
        "cached-1"
    );
    assert_eq!(
        room.social
            .receipts
            .back()
            .unwrap()
            .message
            .client_message_id,
        "new"
    );
    assert_eq!(
        room.social.receipt_bytes,
        room.social
            .receipts
            .iter()
            .map(|entry| entry.bytes)
            .sum::<usize>()
    );

    // Test byte pressure independently of count pressure. Bookkeeping here
    // represents serialized bounded messages without allocating their text.
    room.social.receipts.truncate(2);
    for entry in &mut room.social.receipts {
        entry.bytes = CHAT_RECEIPT_BYTES / 2;
    }
    room.social.receipt_bytes = CHAT_RECEIPT_BYTES;
    sequenced_chat(&mut room, &alice, "newer", 3, None, None).unwrap();
    assert_eq!(room.social.receipts.len(), 2);
    assert_eq!(
        room.social
            .receipts
            .front()
            .unwrap()
            .message
            .client_message_id,
        "cached-2"
    );
    assert_eq!(
        room.social
            .receipts
            .back()
            .unwrap()
            .message
            .client_message_id,
        "newer"
    );
    assert!(room.social.receipt_bytes <= CHAT_RECEIPT_BYTES);
    assert_eq!(
        room.social.receipt_bytes,
        room.social
            .receipts
            .iter()
            .map(|entry| entry.bytes)
            .sum::<usize>()
    );
}

#[test]
fn full_receipt_cache_preserves_existing_ack_and_conflict_without_eviction() {
    let (mut room, alice, _, _, mut receivers) = fixture();
    sequenced_chat(&mut room, &alice, "original", 1, None, None).unwrap();
    let ack = receivers[0].try_recv().unwrap();
    for receiver in receivers.iter_mut().skip(1) {
        receiver.try_recv().unwrap();
    }
    let original_bytes = room.social.receipts.front().unwrap().bytes;
    fill_receipt_cache(
        &mut room,
        alice.media_session_id,
        CHAT_RECEIPTS_PER_MEMBER,
        original_bytes,
    );
    let original_total = room.social.receipt_bytes;
    let session = alice.social.chat_session_id.to_string();
    sequenced_chat(
        &mut room,
        &alice,
        "original",
        1,
        None,
        Some(session.clone()),
    )
    .unwrap();
    assert_eq!(receivers[0].try_recv().unwrap(), ack);
    sequenced_chat(&mut room, &alice, "original", 2, None, Some(session)).unwrap();
    let result: Value = serde_json::from_str(&receivers[0].try_recv().unwrap()).unwrap();
    assert_eq!(result["reason"], "conflict");
    assert_eq!(room.social.receipts.len(), CHAT_RECEIPTS_PER_MEMBER);
    assert_eq!(room.social.receipt_bytes, original_total);
    assert_eq!(
        room.social
            .receipts
            .front()
            .unwrap()
            .message
            .client_message_id,
        "original"
    );
    for receiver in receivers.iter_mut().skip(1) {
        assert!(receiver.try_recv().is_err());
    }
}

#[test]
fn retry_schema_and_sequences_are_bounded() {
    let valid = json!({"type":"retryChatMessage","clientMessageId":"original","sequence":1,"chatSessionId":Uuid::new_v4().to_string(),"content":"hello"});
    assert!(serde_json::from_value::<ClientMessage>(valid.clone()).is_ok());
    let mut extra = valid;
    extra["unexpected"] = json!(true);
    assert!(serde_json::from_value::<ClientMessage>(extra).is_err());
    let (mut room, alice, _, _, mut receivers) = fixture();
    for sequence in [0, MAX_CHAT_SEQUENCE + 1] {
        assert!(sequenced_chat(&mut room, &alice, "invalid", sequence, None, None).is_err());
    }
    assert!(room.social.receipts.is_empty());
    for receiver in &mut receivers {
        assert!(receiver.try_recv().is_err());
    }
}

#[test]
fn history_is_bounded_and_new_members_do_not_receive_prejoin_text() {
    let (mut room, alice, _, carol, _) = fixture();
    for index in 0..400 {
        let message = ChatEntry {
            attachments: Vec::new(),
            message_id: MessageId::random(),
            client_message_id: index.to_string(),
            participant_id: alice.id,
            participant_name: alice.name.clone(),
            recipient_id: None,
            account_conversation: None,
            recipient_name: None,
            content: "x".repeat(4096),
            sent_at: chrono::Utc::now(),
            chat_style: Default::default(),
            reply_to: None,
            removed_at: None,
            revision: 0,
            edited_at: None,
            reactions: Vec::new(),
        };
        room.social
            .remember(alice.media_session_id, None, true, message);
    }
    assert!(room.social.history.len() < HISTORY_MESSAGES);
    assert!(room.social.history_bytes <= HISTORY_BYTES);
    assert!(
        room.social
            .history
            .iter()
            .all(|entry| visible(entry, &carol))
    );
    let mut newcomer = carol.clone();
    newcomer.social = ParticipantSocial::new(room.social.next_sequence);
    assert!(
        room.social
            .history
            .iter()
            .all(|entry| !visible(entry, &newcomer))
    );
}

#[test]
fn stale_sender_and_chat_restrictions_prevent_public_and_private_delivery() {
    let (mut room, alice, bob, _, mut receivers) = fixture();
    let (other_sender, _) = mpsc::channel(2);
    assert!(
        RoomManager::process_social_chat(
            &mut room,
            &alice.id,
            &other_sender,
            "hello".into(),
            Some("stale".into()),
            None
        )
        .is_err()
    );
    room.participants
        .get_mut(&alice.id)
        .unwrap()
        .punitive
        .text_muted = true;
    assert!(chat(&mut room, &alice, "muted", None).is_err());
    assert!(chat(&mut room, &alice, "private-muted", Some(&bob.id)).is_err());
    room.participants
        .get_mut(&alice.id)
        .unwrap()
        .punitive
        .text_muted = false;
    let mut settings = crate::room::RoomSettings::defaults(&rid("social"));
    settings.moderated = true;
    room.settings = Some(settings);
    assert!(chat(&mut room, &alice, "moderated", None).is_err());
    room.participants.get_mut(&alice.id).unwrap().role = roles::Role::Member;
    chat(&mut room, &alice, "voiced", None).unwrap();
    assert!(receivers[0].try_recv().is_ok());
}

#[test]
fn full_private_recipient_queue_does_not_claim_delivery() {
    let (mut room, alice, bob, _, mut receivers) = fixture();
    for _ in 0..16 {
        bob.sender
            .try_send(crate::OutboundJson::from("queued"))
            .unwrap();
    }
    assert!(chat(&mut room, &alice, "busy", Some(&bob.id)).is_err());
    assert!(room.social.history.is_empty());
    assert!(receivers[0].try_recv().is_err());
    assert!(
        room.metrics
            .render_prometheus(0, 0, 0)
            .lines()
            .any(|line| line == "simplestchat_outbound_queue_full_total 1")
    );
}

#[test]
fn closed_private_queue_is_counted_without_claiming_delivery() {
    let (mut room, alice, bob, _, mut receivers) = fixture();
    drop(receivers.remove(1));
    assert!(chat(&mut room, &alice, "closed", Some(&bob.id)).is_err());
    assert!(room.social.history.is_empty());
    assert!(receivers[0].try_recv().is_err());
    let metrics = room.metrics.render_prometheus(0, 0, 0);
    assert!(
        metrics
            .lines()
            .any(|line| line == "simplestchat_outbound_queue_closed_total 1")
    );
    assert!(
        metrics
            .lines()
            .any(|line| line == "simplestchat_outbound_queue_full_total 0")
    );
}

#[test]
fn public_queue_rejections_count_only_eligible_recipients() {
    let (mut room, alice, bob, _, mut receivers) = fixture();
    for _ in 0..16 {
        bob.sender
            .try_send(crate::OutboundJson::from("queued"))
            .unwrap();
    }
    drop(receivers.pop());
    chat(&mut room, &alice, "first", None).unwrap();
    room.participants
        .get_mut(&bob.id)
        .unwrap()
        .social
        .ignored
        .insert(alice.id);
    chat(&mut room, &alice, "ignored", None).unwrap();
    let metrics = room.metrics.render_prometheus(0, 0, 0);
    assert!(
        metrics
            .lines()
            .any(|line| line == "simplestchat_outbound_queue_full_total 1")
    );
    assert!(
        metrics
            .lines()
            .any(|line| line == "simplestchat_outbound_queue_closed_total 2")
    );
    assert_eq!(room.social.history.len(), 2);
}

#[test]
fn failed_ack_retries_are_counted_without_rebroadcasting_chat() {
    let (mut room, alice, _, _, mut receivers) = fixture();
    for _ in 0..16 {
        alice
            .sender
            .try_send(crate::OutboundJson::from("queued"))
            .unwrap();
    }
    chat(&mut room, &alice, "same-id", None).unwrap();
    chat(&mut room, &alice, "same-id", None).unwrap();
    assert_eq!(room.social.history.len(), 1);
    for receiver in receivers.iter_mut().skip(1) {
        assert!(receiver.try_recv().is_ok());
        assert!(receiver.try_recv().is_err());
    }
    assert!(
        room.metrics
            .render_prometheus(0, 0, 0)
            .lines()
            .any(|line| line == "simplestchat_outbound_queue_full_total 2")
    );
}

#[test]
fn ban_listing_shape_never_serializes_guest_network_identity() {
    let mut social = RoomSocial::default();
    social.record_ban(
        "Guest".into(),
        false,
        vec![ParticipantId::random()],
        Some("203.0.113.42".parse().unwrap()),
        Some("reason"),
        Some(60),
    );
    let ban = social.bans.values().next().unwrap();
    let serialized = serde_json::to_string(&ban.entry).unwrap();
    assert!(!serialized.contains("203.0.113"));
    assert!(!serialized.contains("ip"));
    assert!(ban.entry.ban_id.parse::<Uuid>().is_ok());
}

#[test]
fn social_wire_correlates_errors_and_bounds_identifiers() {
    let command:ClientMessage=serde_json::from_value(json!({"type":"privateMessage","targetParticipantId":Uuid::new_v4().to_string(),"content":"hello","clientMessageId":"draft-1"})).unwrap();
    let error = serde_json::to_value(
        command
            .social_error(ErrorCode::Unavailable, "Unavailable")
            .unwrap(),
    )
    .unwrap();
    assert_eq!(error["clientMessageId"], "draft-1");
    assert_eq!(error["type"], "socialError");
    assert!(error.get("requestId").is_none());
    assert!(valid_correlation_id("request_123"));
    assert!(!valid_correlation_id(&"x".repeat(65)));
    assert!(!valid_correlation_id("unsafe\n"));
}

fn reply(
    room: &mut Room,
    sender: &Participant,
    id: &str,
    target: Option<&ParticipantId>,
    content: &str,
    reply_to: Option<&str>,
) -> Result<()> {
    RoomManager::process_chat_attempt(
        room,
        &sender.id,
        &sender.sender,
        ChatAttempt {
            attachment_ids: Vec::new(),
            content: content.into(),
            client_message_id: Some(id.into()),
            recipient_id: target.copied(),
            sequence: None,
            retry_session: None,
            reply_to: reply_to.map(mid),
        },
    )
}

fn drain(receivers: &mut [mpsc::Receiver<crate::OutboundJson>]) {
    for receiver in receivers {
        while receiver.try_recv().is_ok() {}
    }
}

#[test]
fn replies_quote_what_they_answer_only_within_its_conversation() {
    let (mut room, alice, bob, carol, mut receivers) = fixture();
    let long = format!("line one\n{}", "word ".repeat(40));
    reply(&mut room, &alice, "public-1", None, &long, None).unwrap();
    let original = room.social.history.back().unwrap().message.message_id;
    drain(&mut receivers);
    reply(
        &mut room,
        &bob,
        "reply-1",
        None,
        "agreed",
        Some(&original.to_string()),
    )
    .unwrap();
    let quoted = room
        .social
        .history
        .back()
        .unwrap()
        .message
        .reply_to
        .clone()
        .unwrap();
    assert_eq!(quoted.message_id, original);
    assert_eq!(quoted.participant_id, alice.id);
    assert_eq!(quoted.participant_name, "Alice");
    assert!(quoted.excerpt.starts_with("line one word word"), "one line");
    assert!(quoted.excerpt.ends_with('…'));
    assert_eq!(quoted.excerpt.chars().count(), REPLY_EXCERPT_CHARS + 1);
    let public: Value = serde_json::from_str(&receivers[2].try_recv().unwrap()).unwrap();
    assert_eq!(public["type"], "chatReceived");
    assert_eq!(public["replyTo"]["messageId"], original.to_string());
    // A private reply cannot quote public text, nor anything it cannot see.
    assert!(
        reply(
            &mut room,
            &bob,
            "reply-2",
            Some(&carol.id),
            "psst",
            Some(&original.to_string())
        )
        .is_err()
    );
    assert!(
        reply(
            &mut room,
            &bob,
            "reply-3",
            None,
            "?",
            Some(&Uuid::new_v4().to_string())
        )
        .is_err()
    );
    reply(
        &mut room,
        &alice,
        "private-1",
        Some(&bob.id),
        "just us",
        None,
    )
    .unwrap();
    let private = room.social.history.back().unwrap().message.message_id;
    assert!(
        reply(
            &mut room,
            &carol,
            "reply-4",
            None,
            "me too",
            Some(&private.to_string())
        )
        .is_err()
    );
    reply(
        &mut room,
        &bob,
        "reply-5",
        Some(&alice.id),
        "ok",
        Some(&private.to_string()),
    )
    .unwrap();
    assert!(
        room.social
            .history
            .back()
            .unwrap()
            .message
            .reply_to
            .is_some()
    );
    // The same message ID cannot later claim a different reply.
    assert!(reply(&mut room, &bob, "reply-1", None, "agreed", None).is_err());
}

#[test]
fn reactions_toggle_reach_only_viewers_and_stay_within_budget() {
    let (mut room, alice, bob, carol, mut receivers) = fixture();
    reply(&mut room, &alice, "public-1", None, "party?", None).unwrap();
    let message = room.social.history.back().unwrap().message.message_id;
    drain(&mut receivers);
    let emojis = |reactions: &[ChatReaction]| -> Vec<(String, Vec<ParticipantId>)> {
        reactions
            .iter()
            .map(|r| (r.emoji.clone(), r.participant_ids.clone()))
            .collect()
    };
    let added = react_to_message(&mut room, &bob.id, &message, "🎉").unwrap();
    assert_eq!(emojis(&added), vec![("🎉".to_string(), vec![bob.id])]);
    for receiver in &mut receivers {
        let event: Value = serde_json::from_str(&receiver.try_recv().unwrap()).unwrap();
        assert_eq!(event["type"], "messageReactions");
        assert_eq!(event["messageId"], message.to_string());
    }
    react_to_message(&mut room, &carol.id, &message, "🎉").unwrap();
    let taken_back = react_to_message(&mut room, &bob.id, &message, "🎉").unwrap();
    assert_eq!(
        emojis(&taken_back),
        vec![("🎉".to_string(), vec![carol.id])]
    );
    assert!(react_to_message(&mut room, &bob.id, &message, "🍕").is_err());
    assert!(react_to_message(&mut room, &bob.id, &mid("missing"), "🎉").is_err());
    // Private text takes reactions only from its two participants.
    reply(
        &mut room,
        &alice,
        "private-1",
        Some(&bob.id),
        "just us",
        None,
    )
    .unwrap();
    let private = room.social.history.back().unwrap().message.message_id;
    drain(&mut receivers);
    assert!(react_to_message(&mut room, &carol.id, &private, "👍").is_err());
    react_to_message(&mut room, &bob.id, &private, "👍").unwrap();
    assert!(receivers[0].try_recv().is_ok());
    assert!(receivers[1].try_recv().is_ok());
    assert!(
        receivers[2].try_recv().is_err(),
        "the third member never hears of it"
    );
    // History bytes follow the reactions exactly.
    let counted: usize = room.social.history.iter().map(|entry| entry.bytes).sum();
    assert_eq!(room.social.history_bytes, counted);
    for entry in &room.social.history {
        assert_eq!(
            entry.bytes,
            serde_json::to_vec(&entry.message).unwrap().len()
        );
    }
    // One message holds at most MESSAGE_REACTIONS reaction records.
    let mut extra = Vec::new();
    for index in 0..MESSAGE_REACTIONS / REACTIONS.len() {
        let (member, receiver) = participant(&format!("Member {index}"));
        room.participants.insert(member.id, member.clone());
        extra.push((member, receiver));
    }
    // Carol takes hers back, leaving the message bare before it fills up.
    assert!(
        react_to_message(&mut room, &carol.id, &message, "🎉")
            .unwrap()
            .is_empty()
    );
    for (member, _) in &extra {
        for emoji in REACTIONS {
            react_to_message(&mut room, &member.id, &message, emoji).unwrap();
        }
    }
    assert!(react_to_message(&mut room, &alice.id, &message, "👍").is_err());
}

#[test]
fn saved_public_replies_authorize_prejoin_rows_without_exposing_private_or_ignored_text() {
    let (mut room, alice, bob, _, _) = fixture();
    reply(
        &mut room,
        &alice,
        "saved-question",
        None,
        "Saved question",
        None,
    )
    .unwrap();
    let original = room.social.history.back().unwrap().message.clone();
    let next_sequence = room.social.next_sequence;
    room.participants
        .get_mut(&bob.id)
        .unwrap()
        .social
        .joined_sequence = next_sequence;
    let attempt = |id: &str| ChatAttempt {
        content: "Saved answer".into(),
        attachment_ids: Vec::new(),
        client_message_id: Some(Uuid::new_v4().to_string()),
        recipient_id: None,
        sequence: None,
        retry_session: None,
        reply_to: Some(mid(id)),
    };
    assert!(
        RoomManager::prepare_chat_attempt(
            &mut room,
            &bob.id,
            &bob.sender,
            attempt(&original.message_id.to_string())
        )
        .is_err()
    );
    let prepared = RoomManager::prepare_chat_attempt_with_saved_reply(
        &mut room,
        &bob.id,
        &bob.sender,
        attempt(&original.message_id.to_string()),
        Some(&original),
    )
    .unwrap()
    .unwrap();
    assert_eq!(prepared.message.reply_to.unwrap().excerpt, "Saved question");
    assert!(
        RoomManager::prepare_chat_attempt_with_saved_reply(
            &mut room,
            &bob.id,
            &bob.sender,
            attempt(&Uuid::new_v4().to_string()),
            Some(&original),
        )
        .is_err()
    );
    room.participants
        .get_mut(&bob.id)
        .unwrap()
        .social
        .ignored
        .insert(alice.id);
    assert!(
        RoomManager::prepare_chat_attempt_with_saved_reply(
            &mut room,
            &bob.id,
            &bob.sender,
            attempt(&original.message_id.to_string()),
            Some(&original),
        )
        .is_err()
    );
    room.participants
        .get_mut(&bob.id)
        .unwrap()
        .social
        .ignored
        .clear();
    let mut private = original.clone();
    private.recipient_id = Some(bob.id);
    assert!(saved_public_reply(&private, &bob).is_err());
    let mut removed = original.clone();
    removed.removed_at = Some(chrono::Utc::now());
    assert!(saved_public_reply(&removed, &bob).is_err());
}

#[test]
fn explicit_reaction_presence_retries_do_not_toggle_or_duplicate() {
    let reactor = ParticipantId::random();
    let first = set_reaction(&[], &reactor, "👍", true).unwrap();
    let repeated = set_reaction(&first, &reactor, "👍", true).unwrap();
    assert_eq!(repeated[0].participant_ids, vec![reactor]);
    let removed = set_reaction(&repeated, &reactor, "👍", false).unwrap();
    assert!(removed.is_empty());
    assert!(
        set_reaction(&removed, &reactor, "👍", false)
            .unwrap()
            .is_empty()
    );
    assert!(set_reaction(&removed, &reactor, "🍕", true).is_err());
}

#[test]
fn first_account_conversation_requires_admitted_account_peers_and_room_chat_authority() {
    let (mut room, alice, bob, _, _receivers) = fixture();
    assert!(can_start_account_conversation(&room, &alice.id, &bob.id));
    assert!(!can_start_account_conversation(
        &room,
        &alice.id,
        &ParticipantId::random()
    ));
    room.participants.get_mut(&bob.id).unwrap().authenticated = false;
    assert!(!can_start_account_conversation(&room, &alice.id, &bob.id));
    room.participants.get_mut(&bob.id).unwrap().authenticated = true;
    room.participants
        .get_mut(&alice.id)
        .unwrap()
        .punitive
        .text_muted = true;
    assert!(!can_start_account_conversation(&room, &alice.id, &bob.id));
    room.participants
        .get_mut(&alice.id)
        .unwrap()
        .punitive
        .text_muted = false;
    let mut settings = crate::room::RoomSettings::defaults(&rid("social"));
    settings.allow_chat = false;
    room.settings = Some(settings);
    assert!(!can_start_account_conversation(&room, &alice.id, &bob.id));
}

#[tokio::test]
#[ignore = "requires disposable TEST_DATABASE_URL and mediasoup worker"]
async fn database_first_account_pm_uses_room_authority_and_retries_after_departure() {
    let pool =
        sqlx::PgPool::connect(&std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL"))
            .await
            .unwrap();
    let (mut room, alice, bob, _carol, _receivers) = fixture();
    let own: Uuid = alice.id.as_uuid();
    let peer: Uuid = bob.id.as_uuid();
    for person in [&alice, &bob] {
        sqlx::query("INSERT INTO users(id,email,display_name) VALUES($1,$2,$3)")
            .bind(person.id.as_uuid())
            .bind(format!("{}@firstpm.invalid", person.id))
            .bind(&person.name)
            .execute(&pool)
            .await
            .unwrap();
    }
    let sid = crate::auth::session::create_session(
        &pool,
        &own,
        &crate::auth::session::generate_refresh_token().unwrap(),
    )
    .await
    .unwrap();
    let claims = crate::auth::types::Claims {
        sub: own.to_string(),
        name: "Alice".into(),
        iss: "fixture".into(),
        aud: "fixture".into(),
        exp: (chrono::Utc::now().timestamp() + 3600) as usize,
        auth_version: 0,
        sid,
    };
    let room_id = rid(&format!("first-pm-{}", Uuid::new_v4()));
    room.id.clone_from(&room_id);
    let mut config = MediaConfig::default();
    config.worker_config.num_workers = 1;
    config.webrtc_server_port_base = 0;
    let metrics = ServerMetrics::new();
    let manager = Arc::new(
        RoomManager::new(
            crate::room::ManagerSettings::default(),
            config,
            metrics.clone(),
            Some(pool.clone()),
        )
        .await
        .unwrap(),
    );
    let room = Arc::new(TokioRwLock::new(room));
    manager
        .rooms
        .write()
        .unwrap()
        .insert(room_id.clone(), room.clone());
    let server = crate::signaling::SignalingServer::new(
        manager.clone(),
        metrics,
        Some(pool.clone()),
        &crate::config::Config::for_tests(),
    )
    .unwrap();
    let attempt = Uuid::new_v4().to_string();
    let request = |context: Option<String>, content: &str, reply_to| history::SendRequest {
        client_message_id: attempt.clone(),
        content: content.into(),
        attachment_ids: vec![],
        reply_to,
        room_id: context,
    };
    assert!(
        history::save_private_message(
            &server,
            &claims,
            peer,
            request(None, "First account PM", None)
        )
        .await
        .is_err()
    );
    room.write()
        .await
        .participants
        .get_mut(&alice.id)
        .unwrap()
        .punitive
        .text_muted = true;
    assert!(
        history::save_private_message(
            &server,
            &claims,
            peer,
            request(Some(room_id.to_string()), "First account PM", None)
        )
        .await
        .is_err()
    );
    room.write()
        .await
        .participants
        .get_mut(&alice.id)
        .unwrap()
        .punitive
        .text_muted = false;
    let (saved, created) = history::save_private_message(
        &server,
        &claims,
        peer,
        request(Some(room_id.to_string()), "First account PM", None),
    )
    .await
    .unwrap();
    assert!(created);
    assert_eq!(saved.account_conversation, Some(true));
    room.write().await.participants.remove(&bob.id);
    let (retry, created) = history::save_private_message(
        &server,
        &claims,
        peer,
        request(Some("departed-room".into()), "First account PM", None),
    )
    .await
    .unwrap();
    assert!(!created);
    assert_eq!(retry.message_id, saved.message_id);
    assert!(
        history::save_private_message(
            &server,
            &claims,
            peer,
            request(None, "Changed content", None)
        )
        .await
        .is_err()
    );
    assert!(
        history::save_private_message(
            &server,
            &claims,
            peer,
            request(None, "First account PM", Some(saved.message_id.as_uuid()))
        )
        .await
        .is_err()
    );
    let reply = history::SendRequest {
        client_message_id: Uuid::new_v4().to_string(),
        content: "Offline answer".into(),
        attachment_ids: vec![],
        reply_to: Some(saved.message_id.as_uuid()),
        room_id: None,
    };
    let (reply, created) = history::save_private_message(&server, &claims, peer, reply)
        .await
        .unwrap();
    assert!(created);
    assert_eq!(reply.reply_to.unwrap().excerpt, "First account PM");
    let mut blocking = pool.begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1::text,17))")
        .bind(own.to_string())
        .execute(&mut *blocking)
        .await
        .unwrap();
    let mut expiring = claims.clone();
    expiring.exp = (chrono::Utc::now().timestamp() + 1) as usize;
    let server_copy = server.clone();
    let expired_write = tokio::spawn(async move {
        history::save_private_message(
            &server_copy,
            &expiring,
            peer,
            history::SendRequest {
                client_message_id: Uuid::new_v4().to_string(),
                content: "Must expire while queued".into(),
                attachment_ids: vec![],
                reply_to: None,
                room_id: None,
            },
        )
        .await
    });
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    blocking.commit().await.unwrap();
    assert!(matches!(
        expired_write.await.unwrap(),
        Err(crate::auth::types::AuthError::TokenExpired)
    ));
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM chat_messages WHERE sender_account=$1 AND recipient_account=$2",
    )
    .bind(own)
    .bind(peer)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        count, 2,
        "same-ID retry and expired admission never create another row"
    );
    manager.rooms.write().unwrap().remove(&room_id);
    manager.shutdown().await.unwrap();
    manager.media_server().shutdown().await.unwrap();
    sqlx::query("DELETE FROM chat_messages WHERE sender_account=$1 AND recipient_account=$2")
        .bind(own)
        .bind(peer)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM users WHERE id=ANY($1)")
        .bind(vec![own, peer])
        .execute(&pool)
        .await
        .unwrap();
}

#[test]
fn account_policy_refresh_preserves_only_separate_temporary_guest_ignores() {
    let mut social = ParticipantSocial::new(0);
    let guest = ParticipantId::random();
    let account = ParticipantId::random();
    social.guest_ignored.insert(guest);
    social.ignored.insert(ParticipantId::random());
    let mut preferences = crate::auth::account::ChatPreferences {
        allow_private_messages: false,
        ignored: vec![crate::auth::account::IgnoredPerson {
            id: account.to_string(),
            name: "Account".into(),
        }],
        ..Default::default()
    };
    social.apply_account_preferences(&preferences);
    assert!(!social.allow_private_messages);
    assert_eq!(social.ignored, HashSet::from([guest, account]));
    preferences.allow_private_messages = true;
    preferences.ignored.clear();
    social.apply_account_preferences(&preferences);
    assert!(social.allow_private_messages);
    assert_eq!(social.ignored, HashSet::from([guest]));
}

#[test]
fn saved_reaction_publication_reaches_prejoin_viewers_without_rebuilding_replay() {
    let (mut room, alice, bob, carol, mut receivers) = fixture();
    reply(
        &mut room,
        &alice,
        "saved-reaction",
        None,
        "Saved question",
        None,
    )
    .unwrap();
    let mut saved = room.social.history.back().unwrap().message.clone();
    let next_sequence = room.social.next_sequence;
    room.participants
        .get_mut(&bob.id)
        .unwrap()
        .social
        .joined_sequence = next_sequence;
    room.participants
        .get_mut(&carol.id)
        .unwrap()
        .social
        .ignored
        .insert(alice.id);
    saved.reactions = toggle_reactions(&saved.reactions, &bob.id, "👍").unwrap();
    drain(&mut receivers);
    publish_saved_reaction(&mut room, &saved).unwrap();
    assert_eq!(
        room.social.history[0].message.reactions[0].participant_ids,
        vec![bob.id]
    );
    assert_eq!(
        room.social.history_bytes,
        room.social
            .history
            .iter()
            .map(|entry| entry.bytes)
            .sum::<usize>()
    );
    assert!(receivers[0].try_recv().is_ok());
    assert!(
        receivers[1].try_recv().is_ok(),
        "saved public access permits the newcomer"
    );
    assert!(
        receivers[2].try_recv().is_err(),
        "an ignored sender stays hidden"
    );
    room.social.history.clear();
    room.social.history_bytes = 0;
    publish_saved_reaction(&mut room, &saved).unwrap();
    assert!(
        room.social.history.is_empty(),
        "saved publication does not reinsert old text"
    );
    assert_eq!(room.social.history_bytes, 0);
}

#[test]
fn removing_public_chat_scrubs_quotes_reactions_and_retry_receipts() {
    let (mut room, alice, bob, _carol, mut receivers) = fixture();
    reply(
        &mut room,
        &alice,
        "original",
        None,
        "remove this text",
        None,
    )
    .unwrap();
    let id = room.social.history.back().unwrap().message.message_id;
    reply(
        &mut room,
        &bob,
        "quote",
        None,
        "a reply",
        Some(&id.to_string()),
    )
    .unwrap();
    reply(
        &mut room,
        &alice,
        "private",
        Some(&bob.id),
        "private remains",
        None,
    )
    .unwrap();
    react_to_message(&mut room, &bob.id, &id, "👍").unwrap();
    let removed_at = chrono::DateTime::parse_from_rfc3339("2026-10-07T12:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    room.social.remove_message(&id, removed_at);
    for entry in &room.social.history {
        assert!(
            !serde_json::to_string(&entry.message)
                .unwrap()
                .contains("remove this text")
        );
    }
    for receipt in &room.social.receipts {
        assert!(
            !serde_json::to_string(&receipt.message)
                .unwrap()
                .contains("remove this text")
        );
    }
    let original = &room.social.history[0].message;
    assert_eq!(original.removed_at, Some(removed_at));
    assert!(original.content.is_empty() && original.reactions.is_empty());
    assert_eq!(
        room.social.history[1]
            .message
            .reply_to
            .as_ref()
            .unwrap()
            .excerpt,
        "Message removed"
    );
    assert_eq!(room.social.history[2].message.content, "private remains");
    assert!(react_to_message(&mut room, &bob.id, &id, "👍").is_err());
    assert!(
        reply(
            &mut room,
            &bob,
            "late-reply",
            None,
            "late",
            Some(&id.to_string())
        )
        .is_err()
    );
    while receivers[0].try_recv().is_ok() {}
    reply(
        &mut room,
        &alice,
        "original",
        None,
        "remove this text",
        None,
    )
    .unwrap();
    let ack: Value = serde_json::from_str(&receivers[0].try_recv().unwrap()).unwrap();
    assert_eq!(ack["message"]["removedAt"], serde_json::json!(removed_at));
    assert_eq!(ack["message"]["content"], "");
    assert_eq!(
        room.social.history.len(),
        3,
        "retry never recreates removed text"
    );
    assert_eq!(
        room.social.history_bytes,
        room.social.history.iter().map(|e| e.bytes).sum::<usize>()
    );
    assert_eq!(
        room.social.receipt_bytes,
        room.social.receipts.iter().map(|e| e.bytes).sum::<usize>()
    );
}

#[test]
fn reaction_completion_never_targets_a_reused_history_slot() {
    let (mut room, alice, bob, _, _) = fixture();
    reply(&mut room, &alice, "First", None, "reaction-first", None).unwrap();
    let first = room.social.history.back().unwrap().message.message_id;
    let reactions = prepare_reaction(&room, &bob.id, &first, "👍").unwrap();
    let removed = room.social.history.pop_front().unwrap();
    room.social.history_bytes -= removed.bytes;
    reply(
        &mut room,
        &alice,
        "Replacement",
        None,
        "reaction-second",
        None,
    )
    .unwrap();
    assert!(publish_reaction(&mut room, &first, reactions).is_err());
    assert!(room.social.history[0].message.reactions.is_empty());
}

#[test]
fn author_edits_update_quotes_receipts_and_keep_private_events_private() {
    let (mut room, alice, bob, carol, mut receivers) = fixture();
    reply(&mut room, &alice, "Original", None, "editable", None).unwrap();
    let original = room.social.history.back().unwrap().message.clone();
    reply(
        &mut room,
        &bob,
        "Answer",
        None,
        "answer",
        Some(&original.message_id.to_string()),
    )
    .unwrap();
    let request = history::EditRequest {
        content: "Corrected".into(),
        expected_revision: 0,
    };
    assert!(prepare_edit(&room, &bob, &original.message_id, &request).is_err());
    let edited = prepare_edit(&room, &alice, &original.message_id, &request)
        .unwrap()
        .unwrap();
    publish_edit(&mut room, &edited).unwrap();
    assert_eq!(room.social.history[0].message.revision, 1);
    assert_eq!(
        room.social.history[1]
            .message
            .reply_to
            .as_ref()
            .unwrap()
            .excerpt,
        "Corrected"
    );
    assert_eq!(room.social.receipts[0].message.content, "Corrected");
    assert_eq!(
        room.social.history_bytes,
        room.social
            .history
            .iter()
            .map(|entry| entry.bytes)
            .sum::<usize>()
    );
    // An out-of-order HTTP completion cannot revert runtime history or its quotes.
    room.social.edit_message(&original);
    assert_eq!(room.social.history[0].message.content, "Corrected");
    assert_eq!(
        room.social.history[1]
            .message
            .reply_to
            .as_ref()
            .unwrap()
            .excerpt,
        "Corrected"
    );
    reply(
        &mut room,
        &alice,
        "Private",
        Some(&bob.id),
        "editable-pm",
        None,
    )
    .unwrap();
    let private = room.social.history.back().unwrap().message.clone();
    assert!(prepare_edit(&room, &carol, &private.message_id, &request).is_err());
    for rx in &mut receivers {
        while rx.try_recv().is_ok() {}
    }
    let edited = prepare_edit(&room, &alice, &private.message_id, &request)
        .unwrap()
        .unwrap();
    publish_edit(&mut room, &edited).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&receivers[0].try_recv().unwrap()).unwrap()["type"],
        "chatMessageEdited"
    );
    assert_eq!(
        serde_json::from_str::<Value>(&receivers[1].try_recv().unwrap()).unwrap()["type"],
        "chatMessageEdited"
    );
    assert!(receivers[2].try_recv().is_err());
    room.social
        .remove_message(&original.message_id, chrono::Utc::now());
    room.social.edit_message(&edited);
    assert!(room.social.history[0].message.content.is_empty());
}

struct InboxEditFixture {
    manager: RoomManager,
    room: Arc<TokioRwLock<Room>>,
    alice: Participant,
    bob: Participant,
    receivers: Vec<mpsc::Receiver<crate::OutboundJson>>,
    original: ChatEntry,
    edited: ChatEntry,
}

/// Only chat state and channels are exercised. The coordinator's required
/// idle native worker never creates a router, transport or media producer.
async fn with_inbox_edit_fixture<F, Fut>(test: F)
where
    F: FnOnce(InboxEditFixture) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    use futures_util::FutureExt as _;
    let manager = RoomManager::new_for_connection_tests(ServerMetrics::new()).await;
    let (mut room, alice, bob, _, mut receivers) = fixture();
    reply(
        &mut room,
        &alice,
        "editable-private",
        Some(&bob.id),
        "Original private text",
        None,
    )
    .unwrap();
    let original = room.social.history.back().unwrap().message.clone();
    reply(
        &mut room,
        &bob,
        "private-quote",
        Some(&alice.id),
        "Private reply",
        Some(&original.message_id.to_string()),
    )
    .unwrap();
    let mut edited = original.clone();
    history::apply_edit(
        &mut edited,
        &history::EditRequest {
            content: "Corrected private text".into(),
            expected_revision: 0,
        },
    )
    .unwrap();
    drain(&mut receivers);
    let room_id = room.id.clone();
    let room = Arc::new(TokioRwLock::new(room));
    manager.rooms.write().unwrap().insert(room_id, room.clone());
    let test = test(InboxEditFixture {
        manager: manager.clone(),
        room,
        alice,
        bob,
        receivers,
        original,
        edited,
    });
    let outcome = std::panic::AssertUnwindSafe(tokio::time::timeout(
        std::time::Duration::from_secs(20),
        test,
    ))
    .catch_unwind()
    .await;
    let rooms = manager.shutdown().await;
    let media = manager.media_server.shutdown().await;
    match outcome {
        Err(panic) => std::panic::resume_unwind(panic),
        Ok(result) => result.expect("chat-only inbox edit fixture timed out"),
    }
    rooms.expect("chat-only room fixture cleanup failed");
    media.expect("idle native worker cleanup failed");
}

#[tokio::test]
async fn inbox_edit_waits_for_busy_room_and_reconciles_private_runtime() {
    with_inbox_edit_fixture(|mut fixture| async move {
        let held = fixture.room.write().await;
        let mut delivery = std::pin::pin!(fixture.manager.deliver_inbox_edit(&fixture.edited));
        assert!(
            futures_util::poll!(&mut delivery).is_pending(),
            "a committed edit must wait for the busy room instead of dropping it"
        );
        drop(held);
        tokio::time::timeout(std::time::Duration::from_secs(2), delivery)
            .await
            .expect("edit reconciliation must resume after contention ends");

        let room = fixture.room.read().await;
        assert_eq!(
            room.social.history[0].message.message_id,
            fixture.original.message_id
        );
        assert_eq!(
            room.social.history[0].message.content,
            fixture.edited.content
        );
        assert_eq!(room.social.history[0].message.revision, 1);
        assert_eq!(
            room.social.history[1]
                .message
                .reply_to
                .as_ref()
                .unwrap()
                .excerpt,
            fixture.edited.content
        );
        assert_eq!(
            room.social.receipts[0].message.content,
            fixture.edited.content
        );
        let snapshot = fixture.manager.room_snapshot(&room, &fixture.bob);
        assert_eq!(
            fixture
                .manager
                .room_snapshot(&room, &fixture.alice)
                .messages
                .len(),
            snapshot.messages.len()
        );
        assert_eq!(snapshot.messages[0].content, fixture.edited.content);
        assert_eq!(
            snapshot.messages[1].reply_to.as_ref().unwrap().excerpt,
            fixture.edited.content
        );
        for receiver in &mut fixture.receivers[..2] {
            let event: Value = serde_json::from_str(&receiver.try_recv().unwrap()).unwrap();
            assert_eq!(event["type"], "chatMessageEdited");
            assert_eq!(
                event["message"]["messageId"],
                fixture.edited.message_id.to_string()
            );
            assert_eq!(event["message"]["content"], fixture.edited.content);
            assert!(receiver.try_recv().is_err(), "one edit event per party");
        }
        assert!(
            fixture.receivers[2].try_recv().is_err(),
            "private edits never reach another room participant"
        );
    })
    .await;
}

#[tokio::test]
async fn inbox_edit_survives_observer_cancellation_and_long_control_contention() {
    with_inbox_edit_fixture(|mut fixture| async move {
        struct ResumeTime;
        impl Drop for ResumeTime {
            fn drop(&mut self) {
                tokio::time::resume();
            }
        }
        tokio::time::pause();
        let _resume_time = ResumeTime;
        let held = control::lock_room(&fixture.room).await;
        {
            let mut delivery = std::pin::pin!(fixture.manager.deliver_inbox_edit(&fixture.edited));
            assert!(futures_util::poll!(&mut delivery).is_pending());
        }
        // Let owned completion queue, then outlast ordinary new-write admission.
        tokio::task::yield_now().await;
        tokio::time::advance(control::ADMISSION_TIMEOUT + std::time::Duration::from_secs(1)).await;
        assert!(fixture.receivers[1].try_recv().is_err());
        assert_eq!(
            fixture.room.read().await.social.history[0].message.revision,
            0
        );
        drop(held);
        let event = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            fixture.receivers[1].recv(),
        )
        .await
        .expect("committed completion must survive both cancellation and admission expiry")
        .unwrap();
        let event: Value = serde_json::from_str(&event).unwrap();
        assert_eq!(event["message"]["content"], fixture.edited.content);
        assert_eq!(
            fixture.room.read().await.social.history[0].message.revision,
            1
        );
        assert!(fixture.receivers[2].try_recv().is_err());
    })
    .await;
}

#[tokio::test]
async fn inbox_edit_rechecks_recipient_consent_after_control_contention() {
    with_inbox_edit_fixture(|mut fixture| async move {
        let held = control::lock_room(&fixture.room).await;
        let mut delivery = std::pin::pin!(fixture.manager.deliver_inbox_edit(&fixture.edited));
        assert!(futures_util::poll!(&mut delivery).is_pending());
        fixture
            .room
            .write()
            .await
            .participants
            .get_mut(&fixture.bob.id)
            .unwrap()
            .social
            .ignored
            .insert(fixture.alice.id);
        drop(held);
        tokio::time::timeout(std::time::Duration::from_secs(2), delivery)
            .await
            .expect("consent recheck must complete after control is released");
        assert_eq!(
            fixture.room.read().await.social.history[0].message.revision,
            1
        );
        assert!(fixture.receivers[0].try_recv().is_ok());
        assert!(
            fixture.receivers[1].try_recv().is_err(),
            "ignored sender gets no live delivery"
        );
        assert!(fixture.receivers[2].try_recv().is_err());
    })
    .await;
}

#[tokio::test]
async fn inbox_edit_never_targets_a_replaced_or_deleting_room() {
    with_inbox_edit_fixture(|mut fixture| async move {
        for replace in [false, true] {
            let held = control::lock_room(&fixture.room).await;
            let mut delivery = std::pin::pin!(fixture.manager.deliver_inbox_edit(&fixture.edited));
            assert!(futures_util::poll!(&mut delivery).is_pending());
            let room_id = fixture.room.read().await.id.clone();
            if replace {
                let (mut replacement, _, _, _, _) = self::fixture();
                replacement.id.clone_from(&room_id);
                // Same account IDs make an incorrect retarget observable.
                replacement
                    .participants
                    .insert(fixture.alice.id, fixture.alice.clone());
                fixture
                    .manager
                    .rooms
                    .write()
                    .unwrap()
                    .insert(room_id.clone(), Arc::new(TokioRwLock::new(replacement)));
            } else {
                fixture.room.write().await.deleting = true;
            }
            drop(held);
            tokio::time::timeout(std::time::Duration::from_secs(2), delivery)
                .await
                .expect("retired room completion must finish without publishing");
            assert_eq!(
                fixture.room.read().await.social.history[0].message.revision,
                0
            );
            assert!(
                fixture
                    .receivers
                    .iter_mut()
                    .all(|receiver| receiver.try_recv().is_err())
            );
            fixture.room.write().await.deleting = false;
            fixture
                .manager
                .rooms
                .write()
                .unwrap()
                .insert(room_id, fixture.room.clone());
        }
    })
    .await;
}

#[tokio::test]
async fn inbox_edit_drain_cancels_completion_without_waiting_for_room_control() {
    with_inbox_edit_fixture(|mut fixture| async move {
        let held = control::lock_room(&fixture.room).await;
        let mut delivery = std::pin::pin!(fixture.manager.deliver_inbox_edit(&fixture.edited));
        assert!(futures_util::poll!(&mut delivery).is_pending());
        fixture.manager.drain.begin_draining();
        tokio::time::timeout(std::time::Duration::from_secs(2), delivery)
            .await
            .expect("shutdown must cancel queued completion while the gate is held");
        assert_eq!(
            fixture.room.read().await.social.history[0].message.revision,
            0
        );
        assert!(
            fixture
                .receivers
                .iter_mut()
                .all(|receiver| receiver.try_recv().is_err())
        );
        drop(held);
    })
    .await;
}

#[tokio::test]
async fn inbox_edit_reconciles_other_rooms_even_when_the_origin_response_fails() {
    with_inbox_edit_fixture(|fixture| async move {
        let _origin_receivers = fixture.receivers;
        let (mut remote_bob, mut remote_events) = participant("Bob in another room");
        remote_bob.id.clone_from(&fixture.bob.id);
        let mut remote = Room::new(rid("other-edit-room"), None, false, None);
        remote
            .participants
            .insert(remote_bob.id, remote_bob.clone());
        remote.social.remember(
            fixture.alice.media_session_id,
            Some(remote_bob.media_session_id),
            true,
            fixture.original.clone(),
        );
        let remote = Arc::new(TokioRwLock::new(remote));
        fixture
            .manager
            .rooms
            .write()
            .unwrap()
            .insert(rid("other-edit-room"), remote.clone());
        while fixture.alice.sender.capacity() > 0 {
            fixture
                .alice
                .sender
                .try_send(crate::OutboundJson::from("queued".to_owned()))
                .unwrap();
        }
        let room_id = fixture.room.read().await.id.clone();
        let result = fixture
            .manager
            .handle_social_request(
                &room_id,
                &fixture.alice.id,
                &fixture.alice.sender,
                &ClientMessage::EditChatMessage {
                    request_id: "edit-with-full-response-queue".into(),
                    message_id: fixture.original.message_id,
                    content: fixture.edited.content.clone(),
                    expected_revision: 0,
                },
            )
            .await;
        assert!(
            result.is_err(),
            "the final response cannot enter the full origin queue"
        );
        let event = tokio::time::timeout(std::time::Duration::from_secs(2), remote_events.recv())
            .await
            .expect("an origin response failure must not lose the published edit")
            .unwrap();
        let event: Value = serde_json::from_str(&event).unwrap();
        assert_eq!(event["type"], "chatMessageEdited");
        assert_eq!(event["message"]["content"], fixture.edited.content);
        assert_eq!(remote.read().await.social.history[0].message.revision, 1);
    })
    .await;
}

#[tokio::test]
async fn message_removal_requires_moderation_and_never_targets_private_messages() {
    let (mut room, alice, mut moderator, _carol, mut receivers) = fixture();
    moderator.role = roles::Role::Moderator;
    room.participants.insert(moderator.id, moderator.clone());
    reply(&mut room, &alice, "public", None, "remove", None).unwrap();
    let public = room.social.history.back().unwrap().message.message_id;
    reply(
        &mut room,
        &alice,
        "private",
        Some(&moderator.id),
        "private",
        None,
    )
    .unwrap();
    let private = room.social.history.back().unwrap().message.message_id;
    for receiver in &mut receivers {
        while receiver.try_recv().is_ok() {}
    }
    let mut config = MediaConfig::default();
    config.worker_config.num_workers = 1;
    config.webrtc_server_port_base = 0;
    let manager = RoomManager::new(
        crate::room::ManagerSettings::default(),
        config,
        ServerMetrics::new(),
        None,
    )
    .await
    .unwrap();
    let room_id = room.id.clone();
    let room_lock = Arc::new(TokioRwLock::new(room));
    manager
        .rooms
        .write()
        .unwrap()
        .insert(room_id.clone(), room_lock.clone());
    let command = |message_id: &MessageId| ClientMessage::RemoveChatMessage {
        request_id: "remove".into(),
        message_id: *message_id,
    };
    let pin = |message_id: &MessageId| ClientMessage::SetPinnedMessage {
        request_id: "pin".into(),
        message_id: *message_id,
        pinned: true,
    };
    assert!(
        manager
            .handle_social_request(&room_id, &alice.id, &alice.sender, &pin(&public))
            .await
            .is_err()
    );
    assert!(
        manager
            .handle_social_request(&room_id, &moderator.id, &moderator.sender, &pin(&private))
            .await
            .is_err()
    );
    manager
        .handle_social_request(&room_id, &moderator.id, &moderator.sender, &pin(&public))
        .await
        .unwrap();
    assert_eq!(room_lock.read().await.social.pins.len(), 1);
    for rx in &mut receivers {
        while rx.try_recv().is_ok() {}
    }

    assert!(
        manager
            .handle_social_request(&room_id, &alice.id, &alice.sender, &command(&public))
            .await
            .is_err()
    );
    assert!(
        manager
            .handle_social_request(
                &room_id,
                &moderator.id,
                &moderator.sender,
                &command(&private)
            )
            .await
            .is_err()
    );
    manager
        .handle_social_request(
            &room_id,
            &moderator.id,
            &moderator.sender,
            &command(&public),
        )
        .await
        .unwrap();
    let room = room_lock.read().await;
    assert!(room.social.history[0].message.removed_at.is_some());
    assert_eq!(room.social.history[1].message.content, "private");
    assert_eq!(room.social.moderation_events.len(), 1);
    assert!(room.social.pins.is_empty());
    assert_eq!(
        room.social.moderation_events[0].action,
        ModerationAction::MessageRemoved
    );
    assert_eq!(room.social.moderation_events[0].target_id, alice.id.into());
    let event: Value = serde_json::from_str(&receivers[0].try_recv().unwrap()).unwrap();
    assert_eq!(event["type"], "chatMessageRemoved");
    assert_eq!(event["messageId"], public.to_string());
}

#[tokio::test]
#[ignore = "requires disposable TEST_DATABASE_URL and mediasoup worker"]
async fn database_an_account_keeps_its_chat_style_across_joins() {
    let database_url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL");
    let pool = sqlx::PgPool::connect(&database_url).await.unwrap();
    let user = Uuid::new_v4();
    sqlx::query("INSERT INTO users(id,email,display_name) VALUES($1,$2,$3)")
        .bind(user)
        .bind(format!("{user}@style.invalid"))
        .bind("Styled")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        load_chat_style(&pool, user).await.unwrap(),
        Some(ChatStyle::default())
    );
    let mut config = MediaConfig::default();
    config.worker_config.num_workers = 1;
    config.webrtc_server_port_base = 0;
    let mut manager = RoomManager::new(
        crate::room::ManagerSettings::default(),
        config,
        ServerMetrics::new(),
        Some(pool.clone()),
    )
    .await
    .unwrap();
    manager.allow_ad_hoc_rooms = true;
    let room_id = rid(&format!("style-{}", Uuid::new_v4()));
    let join = |sent: ChatStyle| {
        let (tx, rx) = mpsc::channel(64);
        let manager = &manager;
        let room_id = room_id.clone();
        async move {
            let joined = manager
                .add_participant(
                    &room_id,
                    ParticipantId::from(user),
                    "Styled".into(),
                    tx.clone(),
                    true,
                    Arc::new(std::sync::atomic::AtomicBool::new(false)),
                    None,
                    "styled-token",
                    None,
                    Some(sent),
                )
                .await
                .unwrap();
            let crate::room::JoinResult::Joined { chat_style, .. } = joined else {
                panic!("joined");
            };
            (chat_style, tx, rx)
        }
    };
    // The account's saved look, not what the client sent.
    let pink = ChatStyle {
        color: Some("pink".into()),
        style: ChatStyleKind::Bubble,
    };
    let (style, tx, _rx) = join(pink.clone()).await;
    assert_eq!(style, ChatStyle::default());
    let violet = ChatStyle {
        color: Some("violet".into()),
        style: ChatStyleKind::Text,
    };
    manager
        .handle_social_request(
            &room_id,
            &ParticipantId::from(user),
            &tx,
            &ClientMessage::SetChatStyle {
                request_id: "look".into(),
                chat_style: violet.clone(),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        load_chat_style(&pool, user).await.unwrap(),
        Some(violet.clone())
    );
    manager
        .remove_participant_for_sender(&room_id, &ParticipantId::from(user), &tx)
        .await
        .unwrap();
    let (style, _tx, _rx) = join(pink).await;
    assert_eq!(style, violet, "the next join brings the saved look");
    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(user)
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "requires disposable TEST_DATABASE_URL and mediasoup worker"]
async fn database_public_message_removal_scrubs_durable_history_without_runtime_copy() {
    let pool =
        sqlx::PgPool::connect(&std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL"))
            .await
            .unwrap();
    let (mut room, alice, mut moderator, _carol, _receivers) = fixture();
    moderator.role = roles::Role::Moderator;
    room.participants.insert(moderator.id, moderator.clone());
    let room_id = rid(&format!("remove-{}", Uuid::new_v4()));
    let other_room = rid(&format!("remove-other-{}", Uuid::new_v4()));
    for person in [&alice, &moderator] {
        sqlx::query("INSERT INTO users(id,email,display_name) VALUES($1,$2,$3)")
            .bind(person.id.as_uuid())
            .bind(format!("{}@remove.invalid", person.id))
            .bind(&person.name)
            .execute(&pool)
            .await
            .unwrap();
    }
    for id in [&room_id, &other_room] {
        sqlx::query(
            "INSERT INTO rooms(id,owner_id,display_name,history_retention_days) VALUES($1,$2,$1,7)",
        )
        .bind(id)
        .bind(moderator.id.as_uuid())
        .execute(&pool)
        .await
        .unwrap();
    }
    let mut settings = crate::room::RoomSettings::defaults(&room_id);
    settings.owner_id = moderator.id.as_uuid();
    settings.history_retention_days = 7;
    room.id = room_id.clone();
    room.persisted = true;
    room.settings = Some(settings);
    reply(
        &mut room,
        &alice,
        "original",
        None,
        "durable removed text",
        None,
    )
    .unwrap();
    let original = room.social.history.back().unwrap().message.clone();
    reply(
        &mut room,
        &moderator,
        "quote",
        None,
        "durable reply",
        Some(&original.message_id.to_string()),
    )
    .unwrap();
    let quoted = room.social.history.back().unwrap().message.clone();
    reply(
        &mut room,
        &alice,
        "private",
        Some(&moderator.id),
        "private stays",
        None,
    )
    .unwrap();
    let private = room.social.history.back().unwrap().message.clone();
    for message in [&original, &quoted] {
        super::super::history::persist_message(
            &pool,
            Some(room_id.as_str()),
            alice.media_session_id,
            true,
            7,
            message,
            &[],
        )
        .await
        .unwrap();
    }
    super::super::history::persist_message(
        &pool,
        None,
        alice.media_session_id,
        true,
        90,
        &private,
        &[],
    )
    .await
    .unwrap();
    let foreign = ChatEntry {
        message_id: MessageId::random(),
        client_message_id: "foreign".into(),
        ..original.clone()
    };
    super::super::history::persist_message(
        &pool,
        Some(other_room.as_str()),
        alice.media_session_id,
        true,
        7,
        &foreign,
        &[],
    )
    .await
    .unwrap();
    room.social.history.clear();
    room.social.history_bytes = 0;
    room.social.receipts.clear();
    room.social.receipt_bytes = 0;
    let mut config = MediaConfig::default();
    config.worker_config.num_workers = 1;
    config.webrtc_server_port_base = 0;
    let manager = RoomManager::new(
        crate::room::ManagerSettings::default(),
        config,
        ServerMetrics::new(),
        Some(pool.clone()),
    )
    .await
    .unwrap();
    manager
        .rooms
        .write()
        .unwrap()
        .insert(room_id.clone(), Arc::new(TokioRwLock::new(room)));
    let command = |id: &MessageId| ClientMessage::RemoveChatMessage {
        request_id: "remove".into(),
        message_id: *id,
    };
    for denied in [&foreign.message_id, &private.message_id] {
        assert!(
            manager
                .handle_social_request(&room_id, &moderator.id, &moderator.sender, &command(denied))
                .await
                .is_err()
        );
    }
    for _ in 0..2 {
        manager
            .handle_social_request(
                &room_id,
                &moderator.id,
                &moderator.sender,
                &command(&original.message_id),
            )
            .await
            .unwrap();
    }
    let bodies: Vec<serde_json::Value> =
        sqlx::query_scalar("SELECT body FROM chat_messages WHERE room_id=$1")
            .bind(&room_id)
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(bodies.len(), 2);
    assert!(
        bodies
            .iter()
            .all(|body| !body.to_string().contains("durable removed text"))
    );
    let removed = bodies
        .iter()
        .find(|body| body["messageId"] == original.message_id.to_string())
        .unwrap();
    assert_eq!(removed["content"], "");
    assert!(removed["removedAt"].is_string());
    let quote = bodies
        .iter()
        .find(|body| body["messageId"] == quoted.message_id.to_string())
        .unwrap();
    assert_eq!(quote["replyTo"]["excerpt"], "Message removed");
    let events: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM moderation_events WHERE room_id=$1 AND action='message_removed'",
    )
    .bind(&room_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(events, 1, "repeated removal is idempotent");
    let private_body: serde_json::Value =
        sqlx::query_scalar("SELECT body FROM chat_messages WHERE id=$1")
            .bind(private.message_id.as_uuid())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(private_body["content"], "private stays");
    for id in [&room_id, &other_room] {
        sqlx::query("DELETE FROM rooms WHERE id=$1")
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
    }
    for person in [&alice, &moderator] {
        sqlx::query("DELETE FROM users WHERE id=$1")
            .bind(person.id.as_uuid())
            .execute(&pool)
            .await
            .unwrap();
    }
}

#[tokio::test]
#[ignore = "requires disposable TEST_DATABASE_URL and mediasoup worker"]
async fn database_reports_offline_roles_and_opaque_bans_are_room_scoped() {
    let database_url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL");
    let pool = sqlx::PgPool::connect(&database_url).await.unwrap();
    sqlx::raw_sql(include_str!("../../migrations/014_create_room_reports.sql"))
        .execute(&pool)
        .await
        .unwrap();
    let (mut room, mut owner, bob, mut absent, mut receivers) = fixture();
    let room_id = rid(&format!("social-{}", Uuid::new_v4()));
    let other_room_id = rid(&format!("other-{}", Uuid::new_v4()));
    owner.role = roles::Role::Owner;
    absent.role = roles::Role::Member;
    let owner_id: Uuid = owner.id.as_uuid();
    let absent_id: Uuid = absent.id.as_uuid();
    let user_ids: Vec<Uuid> = [&owner, &bob, &absent]
        .iter()
        .map(|p| p.id.as_uuid())
        .collect();
    for participant in [&owner, &bob, &absent] {
        sqlx::query("INSERT INTO users(id,email,display_name) VALUES($1,$2,$3)")
            .bind(participant.id.as_uuid())
            .bind(format!("{}@social.invalid", participant.id))
            .bind(&participant.name)
            .execute(&pool)
            .await
            .unwrap();
    }
    for id in [&room_id, &other_room_id] {
        sqlx::query("INSERT INTO rooms(id,owner_id,display_name) VALUES($1,$2,$1)")
            .bind(id)
            .bind(owner_id)
            .execute(&pool)
            .await
            .unwrap();
    }
    roles::set_role(&pool, &room_id, &absent_id, roles::Role::Member, &owner_id)
        .await
        .unwrap();
    let mut settings = crate::room::RoomSettings::defaults(&room_id);
    settings.owner_id = owner_id;
    room.id = room_id.clone();
    room.persisted = true;
    room.settings = Some(settings);
    room.participants.insert(owner.id, owner.clone());
    room.participants.remove(&absent.id);
    let mut config = MediaConfig::default();
    config.worker_config.num_workers = 1;
    config.webrtc_server_port_base = 0;
    let manager = RoomManager::new(
        crate::room::ManagerSettings::default(),
        config,
        ServerMetrics::new(),
        Some(pool.clone()),
    )
    .await
    .unwrap();
    manager
        .rooms
        .write()
        .unwrap()
        .insert(room_id.clone(), Arc::new(TokioRwLock::new(room)));

    manager
        .handle_social_request(
            &room_id,
            &owner.id,
            &owner.sender,
            &ClientMessage::ListRoomMembers {
                request_id: "members".into(),
                offset: None,
            },
        )
        .await
        .unwrap();
    let response: Value = serde_json::from_str(&receivers[0].try_recv().unwrap()).unwrap();
    let members = response["data"]["members"].as_array().unwrap();
    assert!(members.iter().any(|m| m["userId"] == absent.id.to_string()
        && m["online"] == false
        && m["role"] == "member"));
    manager
        .handle_social_request(
            &room_id,
            &owner.id,
            &owner.sender,
            &ClientMessage::SetMemberRole {
                request_id: "promote".into(),
                target_user_id: UserId::from(absent.id.as_uuid()),
                role: roles::Role::Moderator,
            },
        )
        .await
        .unwrap();
    receivers[0].try_recv().unwrap();
    let role: roles::Role =
        sqlx::query_scalar("SELECT role FROM room_roles WHERE room_id=$1 AND user_id=$2")
            .bind(&room_id)
            .bind(absent_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(role, roles::Role::Moderator);
    assert!(
        manager
            .handle_social_request(
                &room_id,
                &bob.id,
                &bob.sender,
                &ClientMessage::ListRoomMembers {
                    request_id: "denied".into(),
                    offset: None
                }
            )
            .await
            .is_err()
    );
    assert!(
        manager
            .handle_social_request(
                &room_id,
                &owner.id,
                &owner.sender,
                &ClientMessage::SetMemberRole {
                    request_id: "unrelated".into(),
                    target_user_id: UserId::random(),
                    role: roles::Role::Member
                }
            )
            .await
            .is_err()
    );

    manager
        .handle_social_request(
            &room_id,
            &bob.id,
            &bob.sender,
            &ClientMessage::ReportParticipant {
                request_id: "report".into(),
                target_participant_id: owner.id,
                reason: "Needs moderator review".into(),
            },
        )
        .await
        .unwrap();
    let response: Value = serde_json::from_str(&receivers[1].try_recv().unwrap()).unwrap();
    let report_id = response["data"]["reportId"].as_str().unwrap().to_string();
    assert!(
        manager
            .handle_social_request(
                &room_id,
                &bob.id,
                &bob.sender,
                &ClientMessage::ListRoomReports {
                    request_id: "private".into(),
                    offset: None
                }
            )
            .await
            .is_err()
    );
    manager
        .handle_social_request(
            &room_id,
            &owner.id,
            &owner.sender,
            &ClientMessage::ListRoomReports {
                request_id: "reports".into(),
                offset: None,
            },
        )
        .await
        .unwrap();
    let response: Value = serde_json::from_str(&receivers[0].try_recv().unwrap()).unwrap();
    assert_eq!(response["data"]["reports"][0]["reportId"], report_id);
    manager
        .handle_social_request(
            &room_id,
            &owner.id,
            &owner.sender,
            &ClientMessage::ResolveRoomReport {
                request_id: "resolve".into(),
                report_id: report_id.clone(),
                status: crate::room::moderation::ReportResolution::Resolved,
            },
        )
        .await
        .unwrap();
    receivers[0].try_recv().unwrap();
    let status: String = sqlx::query_scalar("SELECT status FROM room_reports WHERE id=$1")
        .bind(report_id.parse::<Uuid>().unwrap())
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(status, "resolved");

    let ban_id = Uuid::new_v4();
    let other_ban_id = Uuid::new_v4();
    for (id, room) in [(ban_id, &room_id), (other_ban_id, &other_room_id)] {
        sqlx::query("INSERT INTO room_states(id,room_id,state,ip_address,reason,applied_by) VALUES($1,$2,'banned','203.0.113.42'::inet,'test ban',$3)").bind(id).bind(room).bind(owner_id).execute(&pool).await.unwrap();
    }
    manager
        .handle_social_request(
            &room_id,
            &owner.id,
            &owner.sender,
            &ClientMessage::ListRoomBans {
                request_id: "bans".into(),
                offset: None,
            },
        )
        .await
        .unwrap();
    let response = receivers[0].try_recv().unwrap();
    assert!(!response.contains("203.0.113"));
    let response: Value = serde_json::from_str(&response).unwrap();
    assert_eq!(response["data"]["bans"][0]["banId"], ban_id.to_string());
    assert!(
        manager
            .handle_social_request(
                &room_id,
                &owner.id,
                &owner.sender,
                &ClientMessage::RemoveRoomBan {
                    request_id: "cross-room".into(),
                    ban_id: other_ban_id.to_string()
                }
            )
            .await
            .is_err()
    );
    manager
        .handle_social_request(
            &room_id,
            &owner.id,
            &owner.sender,
            &ClientMessage::RemoveRoomBan {
                request_id: "unban".into(),
                ban_id: ban_id.to_string(),
            },
        )
        .await
        .unwrap();
    receivers[0].try_recv().unwrap();
    let remaining: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM room_states WHERE id=ANY($1)")
        .bind(vec![ban_id, other_ban_id])
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(remaining, 1);
    // Account and guest sanctions remain separate even on the same network.
    let user_ban = Uuid::new_v4();
    sqlx::query("INSERT INTO room_states(id,room_id,user_id,state,ip_address,applied_by) VALUES($1,$2,$3,'banned','203.0.113.42'::inet,$4)")
            .bind(user_ban).bind(&room_id).bind(absent_id).bind(owner_id).execute(&pool).await.unwrap();
    let guest_ip: IpAddr = "203.0.113.42".parse().unwrap();
    manager
        .get_room(&room_id)
        .unwrap()
        .write()
        .await
        .banned_guest_ips
        .insert(guest_ip, None);
    manager
        .handle_social_request(
            &room_id,
            &owner.id,
            &owner.sender,
            &ClientMessage::RemoveRoomBan {
                request_id: "account-unban".into(),
                ban_id: user_ban.to_string(),
            },
        )
        .await
        .unwrap();
    receivers[0].try_recv().unwrap();
    assert!(
        manager
            .get_room(&room_id)
            .unwrap()
            .read()
            .await
            .banned_guest_ips
            .contains_key(&guest_ip)
    );
    // Resolved reports rotate out at the cap; open reports are never silently lost.
    sqlx::query("INSERT INTO room_reports(room_id,reporter_id,reporter_name,target_participant_id,target_name,reason) SELECT $1,$2,'Owner',$3,'Bob','fixture' FROM generate_series(1,499)")
            .bind(&room_id).bind(owner.id).bind(bob.id).execute(&pool).await.unwrap();
    manager
        .handle_social_request(
            &room_id,
            &owner.id,
            &owner.sender,
            &ClientMessage::ReportParticipant {
                request_id: "bounded".into(),
                target_participant_id: bob.id,
                reason: "Another report".into(),
            },
        )
        .await
        .unwrap();
    receivers[0].try_recv().unwrap();
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM room_reports WHERE room_id=$1")
        .bind(&room_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, MAX_REPORTS as i64);
    manager
        .get_room(&room_id)
        .unwrap()
        .write()
        .await
        .social
        .report_cooldowns
        .clear();
    assert!(
        manager
            .handle_social_request(
                &room_id,
                &bob.id,
                &bob.sender,
                &ClientMessage::ReportParticipant {
                    request_id: "full".into(),
                    target_participant_id: owner.id,
                    reason: "Capacity check".into()
                }
            )
            .await
            .is_err()
    );
    sqlx::query("UPDATE users SET preferences=$2 WHERE id=$1")
            .bind(owner.id.as_uuid())
            .bind(sqlx::types::Json(json!({"allowPrivateMessages": false, "ignored": [{"id": absent.id, "name": absent.name}]})))
            .execute(&pool).await.unwrap();
    manager
        .handle_social_request(
            &room_id,
            &owner.id,
            &owner.sender,
            &ClientMessage::SetChatPreferences {
                request_id: "prefs".into(),
                allow_private_messages: true,
                ignored_participant_ids: vec![],
            },
        )
        .await
        .unwrap();
    receivers[0].try_recv().unwrap();
    let snapshot_room = manager.get_room(&room_id).unwrap();
    let snapshot_read = snapshot_room.read().await;
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        manager.handle_social_request(
            &room_id,
            &owner.id,
            &owner.sender,
            &ClientMessage::GetRoomSnapshot {
                request_id: "snapshot".into(),
            },
        ),
    )
    .await
    .expect("snapshots share room read access")
    .unwrap();
    drop(snapshot_read);
    let snapshot: Value = serde_json::from_str(&receivers[0].try_recv().unwrap()).unwrap();
    assert_eq!(snapshot["data"]["allowPrivateMessages"], false);
    assert_eq!(
        snapshot["data"]["ignoredParticipantIds"][0],
        absent.id.to_string()
    );
    assert_eq!(snapshot["data"]["canChat"], true);
    assert!(
        snapshot["data"]["participants"]
            .as_array()
            .unwrap()
            .iter()
            .all(|p| p["id"] != owner.id.to_string() && p["authenticated"] == true)
    );
    manager.shutdown().await.unwrap();
    manager.media_server().shutdown().await.unwrap();
    sqlx::query("DELETE FROM rooms WHERE id=ANY($1)")
        .bind(vec![room_id, other_room_id])
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM users WHERE id=ANY($1)")
        .bind(user_ids)
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
}
