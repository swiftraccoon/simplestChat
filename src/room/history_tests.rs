//! Tests of `src/room/history.rs`.

use super::*;
use crate::ids::fixtures::{mid, rid};

#[test]
fn read_commit_receipts_keep_the_existing_wire_contract() {
    for advanced in [false, true] {
        let result = ReadResult {
            read_message_id: mid("00000000-0000-4000-8000-000000000005"),
            advanced,
        };
        assert_eq!(
            serde_json::to_value(result).unwrap(),
            serde_json::json!({"readMessageId":"00000000-0000-4000-8000-000000000005"})
        );
    }
}
#[test]
fn malformed_saved_account_policy_never_allows_private_delivery() {
    let mut account = ChatAccount {
        id: Uuid::new_v4(),
        display_name: "Account".into(),
        preferences: serde_json::json!({}),
        chat_color: None,
        chat_style: "accent".into(),
        auth_version: 0,
    };
    let peer = Uuid::new_v4();
    assert!(account.allows_private_messages_from(peer));
    for policy in [
        serde_json::json!({"allowPrivateMessages": "true"}),
        serde_json::json!({"ignored": null}),
        serde_json::json!({"ignored": [{"id": "invalid", "name": "Invalid"}]}),
        serde_json::json!({"timestamps": "unknown"}),
    ] {
        account.preferences = policy;
        assert!(!account.allows_private_messages_from(peer));
    }
    account.preferences =
        serde_json::json!({"ignored": [{"id": peer.to_string().to_uppercase(), "name": "Peer"}]});
    assert!(!account.allows_private_messages_from(peer));
}

#[test]
fn history_queries_are_bounded_and_cursors_round_trip() {
    let at = Utc::now();
    let id = Uuid::new_v4();
    let encoded = encode_cursor(at, id);
    let decoded = decode_cursor(Some(&encoded)).unwrap().unwrap();
    assert_eq!(decoded.id, id);
    assert_eq!(decoded.at, at);
    assert!(decode_cursor(Some("bad cursor")).is_err());
    for limit in [0, 51, u32::MAX] {
        assert!(
            HistoryQuery {
                limit: Some(limit),
                ..Default::default()
            }
            .validate()
            .is_err()
        );
    }
    for query in ["a".to_string(), "x".repeat(129), "abc\n".to_string() + "x"] {
        assert!(
            HistoryQuery {
                q: Some(query),
                ..Default::default()
            }
            .validate()
            .is_err()
        );
    }
}
#[test]
fn private_conversation_is_symmetric_and_disjoint_from_room() {
    let a = Uuid::new_v4();
    let b = Uuid::new_v4();
    assert_eq!(private_conversation(a, b), private_conversation(b, a));
    assert_ne!(
        private_conversation(a, b),
        private_conversation(a, Uuid::new_v4())
    );
    assert_ne!(
        private_conversation(a, b),
        public_conversation(&a.to_string())
    );
}
fn entry(content: &str) -> ChatEntry {
    ChatEntry {
        attachments: Vec::new(),
        message_id: MessageId::random(),
        client_message_id: Uuid::new_v4().to_string(),
        participant_id: ParticipantId::random(),
        participant_name: "Author".into(),
        recipient_id: None,
        account_conversation: None,
        recipient_name: None,
        content: content.into(),
        sent_at: Utc::now(),
        removed_at: None,
        revision: 0,
        edited_at: None,
        chat_style: Default::default(),
        reply_to: None,
        reactions: vec![],
    }
}

#[test]
fn message_edits_are_revision_checked_idempotent_and_removal_is_terminal() {
    let mut message = entry("Original");
    let original_id = message.message_id;
    let request = EditRequest {
        content: "Corrected".into(),
        expected_revision: 0,
    };
    apply_edit(&mut message, &request).unwrap();
    assert_eq!(message.revision, 1);
    assert!(message.edited_at.is_some());
    assert_eq!(message.message_id, original_id);
    let edited_at = message.edited_at;
    apply_edit(&mut message, &request).unwrap();
    assert_eq!(message.edited_at, edited_at);
    assert!(
        apply_edit(
            &mut message,
            &EditRequest {
                content: "Stale tab".into(),
                expected_revision: 0
            }
        )
        .is_err()
    );
    assert_eq!(message.content, "Corrected");
    for invalid in [
        "".into(),
        " ".into(),
        "x".repeat(4097),
        "bad\0text".into(),
        "moc.elpmaxe\u{202E}".into(),
        "ex\u{200B}ample".into(),
    ] {
        assert!(
            apply_edit(
                &mut message,
                &EditRequest {
                    content: invalid,
                    expected_revision: 1
                }
            )
            .is_err()
        );
    }
    message.removed_at = Some(Utc::now());
    message.content.clear();
    assert!(
        apply_edit(
            &mut message,
            &EditRequest {
                content: "Restored".into(),
                expected_revision: 1
            }
        )
        .is_err()
    );
    assert!(message.content.is_empty());
}

#[test]
fn history_positions_are_exclusive_and_message_ids_validated() {
    for query in [
        HistoryQuery {
            around: Some(MessageId::random()),
            resume: Some(true),
            ..Default::default()
        },
        HistoryQuery {
            around: Some(MessageId::random()),
            q: Some("query".into()),
            ..Default::default()
        },
        HistoryQuery {
            after: Some("invalid".into()),
            ..Default::default()
        },
    ] {
        assert!(query.validate().is_err());
    }
}

#[tokio::test]
#[ignore = "requires a migrated disposable TEST_DATABASE_URL"]
async fn database_private_reactions_are_idempotent_and_conversation_scoped() {
    let pool = PgPool::connect(&std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL"))
        .await
        .unwrap();
    let author = Uuid::new_v4();
    let peer = Uuid::new_v4();
    for account in [author, peer] {
        sqlx::query(
            "INSERT INTO users(id,email,display_name) VALUES($1,$2,'Private reaction tester')",
        )
        .bind(account)
        .bind(format!("{account}@privatereactions.invalid"))
        .execute(&pool)
        .await
        .unwrap();
    }
    let mut original = entry("Private original");
    original.participant_id = ParticipantId::from(author);
    original.recipient_id = Some(ParticipantId::from(peer));
    original.recipient_name = Some("Peer".into());
    let original = persist_message(&pool, None, author, true, 90, &original, &[])
        .await
        .unwrap();
    assert_eq!(original.account_conversation, Some(true));
    let id = original.message_id.as_uuid();
    let conversation = private_conversation(author, peer);
    for _ in 0..2 {
        let mut tx = pool.begin().await.unwrap();
        let changed = set_private_reaction_in_transaction(
            &mut tx,
            &conversation,
            id,
            &ParticipantId::from(author),
            &SetReactionRequest {
                emoji: "👍".into(),
                present: true,
            },
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(
            changed.reactions[0].participant_ids,
            vec![ParticipantId::from(author)]
        );
    }
    let mut tx = pool.begin().await.unwrap();
    let changed = set_private_reaction_in_transaction(
        &mut tx,
        &conversation,
        id,
        &ParticipantId::from(peer),
        &SetReactionRequest {
            emoji: "👍".into(),
            present: true,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(changed.reactions[0].participant_ids.len(), 2);
    for _ in 0..2 {
        let mut tx = pool.begin().await.unwrap();
        let changed = set_private_reaction_in_transaction(
            &mut tx,
            &conversation,
            id,
            &ParticipantId::from(author),
            &SetReactionRequest {
                emoji: "👍".into(),
                present: false,
            },
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(
            changed.reactions[0].participant_ids,
            vec![ParticipantId::from(peer)]
        );
    }
    let mut tx = pool.begin().await.unwrap();
    assert!(
        set_private_reaction_in_transaction(
            &mut tx,
            &private_conversation(author, Uuid::new_v4()),
            id,
            &ParticipantId::from(author),
            &SetReactionRequest {
                emoji: "👍".into(),
                present: true
            }
        )
        .await
        .is_err()
    );
    tx.rollback().await.unwrap();
    let current = private_message(&pool, author, peer, id)
        .await
        .unwrap()
        .unwrap();
    let edited = edit_saved(
        &pool,
        None,
        author,
        None,
        &current,
        &EditRequest {
            content: "Corrected private original".into(),
            expected_revision: 0,
        },
    )
    .await
    .unwrap();
    assert_eq!(
        edited.reactions[0].participant_ids,
        vec![ParticipantId::from(peer)]
    );
    sqlx::query("UPDATE chat_messages SET body=body-'accountConversation' WHERE id=$1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    let page = page(
        &pool,
        &conversation,
        Some(author),
        90,
        &HistoryQuery::default(),
    )
    .await
    .unwrap();
    assert_eq!(
        page.messages[0].account_conversation,
        Some(true),
        "capability derives from durable conversation authority"
    );
    sqlx::query("UPDATE chat_messages SET expires_at=now()-interval '1 second' WHERE id=$1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        private_message(&pool, author, peer, id)
            .await
            .unwrap()
            .is_none()
    );
    let mut tx = pool.begin().await.unwrap();
    assert!(
        set_private_reaction_in_transaction(
            &mut tx,
            &conversation,
            id,
            &ParticipantId::from(author),
            &SetReactionRequest {
                emoji: "👍".into(),
                present: true
            }
        )
        .await
        .is_err()
    );
    tx.rollback().await.unwrap();
    sqlx::query("DELETE FROM users WHERE id=ANY($1)")
        .bind(vec![author, peer])
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "requires a migrated disposable TEST_DATABASE_URL"]
async fn database_saved_public_reactions_serialize_and_preserve_authoritative_edits() {
    let pool = PgPool::connect(&std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL"))
        .await
        .unwrap();
    let author = Uuid::new_v4();
    sqlx::query("INSERT INTO users(id,email,display_name) VALUES($1,$2,'Saved reaction tester')")
        .bind(author)
        .bind(format!("{author}@reactions.invalid"))
        .execute(&pool)
        .await
        .unwrap();
    let room = rid(&format!("reaction-{}", Uuid::new_v4()));
    sqlx::query(
        "INSERT INTO rooms(id,owner_id,display_name,history_retention_days) VALUES($1,$2,$1,7)",
    )
    .bind(&room)
    .bind(author)
    .execute(&pool)
    .await
    .unwrap();
    let mut original = entry("Original retained question");
    original.participant_id = ParticipantId::from(author);
    persist_message(
        &pool,
        Some(room.as_str()),
        Uuid::new_v4(),
        true,
        7,
        &original,
        &[],
    )
    .await
    .unwrap();
    let first = ParticipantId::random();
    let second = ParticipantId::random();
    let (a, b) = tokio::join!(
        react_public(
            &pool,
            room.as_str(),
            &original.message_id,
            &first,
            &[],
            "👍"
        ),
        react_public(
            &pool,
            room.as_str(),
            &original.message_id,
            &second,
            &[],
            "👍"
        ),
    );
    assert!(a.unwrap().is_some());
    assert!(b.unwrap().is_some());
    let (saved, _) = lookup_public(&pool, room.as_str(), &original.message_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        saved.reactions[0].participant_ids.len(),
        2,
        "concurrent toggles cannot replace one another"
    );
    let corrected = edit_saved(
        &pool,
        Some(room.as_str()),
        author,
        None,
        &saved,
        &EditRequest {
            content: "Corrected retained question".into(),
            expected_revision: 0,
        },
    )
    .await
    .unwrap();
    let toggled = react_public(
        &pool,
        room.as_str(),
        &original.message_id,
        &first,
        &[],
        "👍",
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(toggled.content, corrected.content);
    assert_eq!(toggled.revision, corrected.revision);
    assert_eq!(toggled.reactions[0].participant_ids, vec![second]);
    assert!(
        react_public(
            &pool,
            "another-room",
            &original.message_id,
            &first,
            &[],
            "👍"
        )
        .await
        .unwrap()
        .is_none()
    );
    assert!(
        react_public(
            &pool,
            room.as_str(),
            &original.message_id,
            &first,
            &[ParticipantId::from(author)],
            "👍"
        )
        .await
        .unwrap()
        .is_none()
    );
    assert!(
        react_public(
            &pool,
            room.as_str(),
            &original.message_id,
            &first,
            &[],
            "🍕"
        )
        .await
        .is_err()
    );
    let mut tx = pool.begin().await.unwrap();
    remove_public(&mut tx, room.as_str(), &original.message_id, Utc::now())
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert!(
        react_public(
            &pool,
            room.as_str(),
            &original.message_id,
            &first,
            &[],
            "👍"
        )
        .await
        .unwrap()
        .is_none()
    );
    let mut expired = entry("Expired question");
    expired.participant_id = ParticipantId::from(author);
    expired.sent_at = Utc::now() - chrono::Duration::days(8);
    persist_message(
        &pool,
        Some(room.as_str()),
        Uuid::new_v4(),
        true,
        7,
        &expired,
        &[],
    )
    .await
    .unwrap();
    assert!(
        react_public(&pool, room.as_str(), &expired.message_id, &first, &[], "👍")
            .await
            .unwrap()
            .is_none()
    );
    sqlx::query("DELETE FROM rooms WHERE id=$1")
        .bind(&room)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM users WHERE id=$1")
        .bind(author)
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "requires a migrated disposable TEST_DATABASE_URL"]
async fn database_chat_edits_pins_context_and_unread_preserve_visibility_and_retention() {
    let pool = PgPool::connect(&std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL"))
        .await
        .unwrap();
    let author = Uuid::new_v4();
    let reader = Uuid::new_v4();
    for user in [author, reader] {
        sqlx::query("INSERT INTO users(id,email,display_name) VALUES($1,$2,'Chat update tester')")
            .bind(user)
            .bind(format!("{user}@chatupdates.invalid"))
            .execute(&pool)
            .await
            .unwrap();
    }
    let room = rid(&format!("updates-{}", Uuid::new_v4()));
    sqlx::query(
        "INSERT INTO rooms(id,owner_id,display_name,history_retention_days) VALUES($1,$2,$1,7)",
    )
    .bind(&room)
    .bind(author)
    .execute(&pool)
    .await
    .unwrap();
    let session = Uuid::new_v4();
    let mut messages = vec![];
    for n in 0..6 {
        let mut message = entry(&format!("Message {n}"));
        message.participant_id = ParticipantId::from(author);
        message.sent_at = Utc::now() - chrono::Duration::seconds(100 - n);
        if n == 1 {
            let first: &ChatEntry = &messages[0];
            message.reply_to = Some(crate::signaling::protocol::ChatReplyRef {
                message_id: first.message_id,
                participant_id: ParticipantId::from(author),
                participant_name: "Author".into(),
                excerpt: "Message 0".into(),
            });
        }
        persist_message(&pool, Some(room.as_str()), session, true, 7, &message, &[])
            .await
            .unwrap();
        messages.push(message);
    }
    let conversation = public_conversation(room.as_str());
    mark_read(
        &pool,
        &conversation,
        reader,
        messages[1].message_id.as_uuid(),
    )
    .await
    .unwrap();
    let resumed = page(
        &pool,
        &conversation,
        Some(reader),
        7,
        &HistoryQuery {
            resume: Some(true),
            limit: Some(4),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        resumed.first_unread_message_id,
        Some(messages[2].message_id)
    );
    assert_eq!(
        resumed
            .messages
            .iter()
            .map(|m| m.message_id)
            .collect::<Vec<_>>(),
        messages[..4]
            .iter()
            .map(|m| m.message_id)
            .collect::<Vec<_>>()
    );
    let newer = page(
        &pool,
        &conversation,
        Some(reader),
        7,
        &HistoryQuery {
            after: resumed.newer_cursor,
            limit: Some(4),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(newer.messages.len(), 2);
    assert_eq!(newer.messages[0].message_id, messages[4].message_id);
    assert!(
        page(
            &pool,
            &private_conversation(author, reader),
            Some(reader),
            90,
            &HistoryQuery {
                around: Some(messages[0].message_id),
                ..Default::default()
            }
        )
        .await
        .is_err()
    );
    let request = EditRequest {
        content: "Corrected original".into(),
        expected_revision: 0,
    };
    assert!(
        edit_saved(
            &pool,
            Some(room.as_str()),
            reader,
            None,
            &messages[0],
            &request
        )
        .await
        .is_err()
    );
    let edited = edit_saved(
        &pool,
        Some(room.as_str()),
        author,
        None,
        &messages[0],
        &request,
    )
    .await
    .unwrap();
    assert_eq!(edited.revision, 1);
    assert_eq!(
        edit_saved(
            &pool,
            Some(room.as_str()),
            author,
            None,
            &messages[0],
            &request
        )
        .await
        .unwrap()
        .revision,
        1
    );
    let stored = page(&pool, &conversation, None, 7, &HistoryQuery::default())
        .await
        .unwrap();
    assert_eq!(
        stored.messages[1].reply_to.as_ref().unwrap().excerpt,
        "Corrected original"
    );
    // A retry of the original send returns its edited receipt, without redelivery or rollback.
    assert_eq!(
        persist_message(
            &pool,
            Some(room.as_str()),
            session,
            true,
            7,
            &messages[0],
            &[]
        )
        .await
        .unwrap()
        .content,
        "Corrected original"
    );
    for message in messages.iter().take(3) {
        set_pin(&pool, room.as_str(), &message.message_id, true)
            .await
            .unwrap();
    }
    set_pin(&pool, room.as_str(), &messages[0].message_id, true)
        .await
        .unwrap();
    assert_eq!(
        pinned_messages(&pool, room.as_str()).await.unwrap().len(),
        3
    );
    assert!(
        set_pin(&pool, room.as_str(), &messages[3].message_id, true)
            .await
            .is_err()
    );
    let mut tx = pool.begin().await.unwrap();
    remove_public(&mut tx, room.as_str(), &messages[0].message_id, Utc::now())
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        pinned_messages(&pool, room.as_str()).await.unwrap().len(),
        2
    );
    assert!(
        edit_saved(
            &pool,
            Some(room.as_str()),
            author,
            None,
            &edited,
            &EditRequest {
                content: "Restore".into(),
                expected_revision: 1
            }
        )
        .await
        .is_err()
    );
    assert!(
        set_pin(&pool, room.as_str(), &messages[0].message_id, true)
            .await
            .is_err()
    );
    let removed = page(&pool, &conversation, None, 7, &HistoryQuery::default())
        .await
        .unwrap();
    assert_eq!(
        removed.messages[1].reply_to.as_ref().unwrap().excerpt,
        "Message removed"
    );
    // An expired pin disappears on reads even before the retention sweep runs.
    sqlx::query("UPDATE chat_messages SET expires_at=now()-interval '1 second' WHERE id=$1")
        .bind(messages[2].message_id.as_uuid())
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        pinned_messages(&pool, room.as_str()).await.unwrap().len(),
        1
    );
    set_retention(&pool, room.as_str(), 0).await.unwrap();
    assert!(
        pinned_messages(&pool, room.as_str())
            .await
            .unwrap()
            .is_empty()
    );
    sqlx::query("DELETE FROM rooms WHERE id=$1")
        .bind(&room)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM users WHERE id=ANY($1)")
        .bind(vec![author, reader])
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
}

#[tokio::test]
#[ignore = "requires a migrated disposable TEST_DATABASE_URL"]
async fn database_history_retention_pagination_private_isolation_and_monotonic_reads() {
    let pool = PgPool::connect(&std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL"))
        .await
        .unwrap();
    let alice = Uuid::new_v4();
    let bob = Uuid::new_v4();
    let stranger = Uuid::new_v4();
    for user in [alice, bob, stranger] {
        sqlx::query("INSERT INTO users(id,email,display_name) VALUES($1,$2,'History tester')")
            .bind(user)
            .bind(format!("{user}@history.invalid"))
            .execute(&pool)
            .await
            .unwrap();
    }
    let room = rid(&format!("history-{}", Uuid::new_v4()));
    sqlx::query("INSERT INTO rooms(id,owner_id,display_name) VALUES($1,$2,'History room')")
        .bind(&room)
        .bind(alice)
        .execute(&pool)
        .await
        .unwrap();
    let retention: i32 =
        sqlx::query_scalar("SELECT history_retention_days::integer FROM rooms WHERE id=$1")
            .bind(&room)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(retention, 0, "existing/new rooms begin ephemeral");
    set_retention(&pool, room.as_str(), 7).await.unwrap();
    let session = Uuid::new_v4();
    let base = Utc::now() - chrono::Duration::seconds(100);
    let make = |n: i64, recipient: Option<Uuid>| ChatEntry {
        attachments: Vec::new(),
        message_id: MessageId::random(),
        client_message_id: format!("attempt-{n}"),
        participant_id: ParticipantId::from(alice),
        participant_name: "Alice".into(),
        recipient_id: recipient.map(ParticipantId::from),
        account_conversation: None,
        recipient_name: recipient.map(|_| "Bob".into()),
        content: format!("Message needle%_{n}"),
        sent_at: base + chrono::Duration::seconds(n),
        removed_at: None,
        revision: 0,
        edited_at: None,
        chat_style: Default::default(),
        reply_to: None,
        reactions: vec![],
    };
    let first = make(1, None);
    let second = make(2, None);
    let third = make(3, None);
    for message in [&first, &second, &third] {
        persist_message(&pool, Some(room.as_str()), session, true, 7, message, &[])
            .await
            .unwrap();
    }
    let public = public_conversation(room.as_str());
    let page_one = page(
        &pool,
        &public,
        Some(alice),
        7,
        &HistoryQuery {
            limit: Some(2),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        page_one
            .messages
            .iter()
            .map(|m| m.message_id)
            .collect::<Vec<_>>(),
        vec![second.message_id, third.message_id]
    );
    let page_two = page(
        &pool,
        &public,
        Some(alice),
        7,
        &HistoryQuery {
            before: page_one.next_cursor,
            limit: Some(2),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(page_two.messages.len(), 1);
    assert_eq!(page_two.messages[0].message_id, first.message_id);
    assert!(page_two.next_cursor.is_none());
    let found = page(
        &pool,
        &public,
        None,
        7,
        &HistoryQuery {
            q: Some("needle%_2".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(found.messages.len(), 1);
    assert_eq!(
        found.messages[0].message_id, second.message_id,
        "search treats SQL wildcard characters literally"
    );
    let advance = mark_read(&pool, &public, alice, third.message_id.as_uuid())
        .await
        .unwrap();
    assert!(advance.advanced);
    let repeat = mark_read(&pool, &public, alice, third.message_id.as_uuid())
        .await
        .unwrap();
    assert!(!repeat.advanced);
    let read = mark_read(&pool, &public, alice, first.message_id.as_uuid())
        .await
        .unwrap();
    assert!(!read.advanced);
    assert_eq!(
        read.read_message_id, third.message_id,
        "older device cannot rewind read cursor"
    );
    assert!(
        mark_read(&pool, &public, alice, Uuid::new_v4())
            .await
            .is_err()
    );
    let private = make(4, Some(bob));
    let saved = persist_message(&pool, None, session, true, PM_RETENTION_DAYS, &private, &[])
        .await
        .unwrap();
    let retried = persist_message(&pool, None, session, true, PM_RETENTION_DAYS, &private, &[])
        .await
        .unwrap();
    assert_eq!(
        saved.message_id, retried.message_id,
        "duplicate durable attempt keeps original ID"
    );
    let mut conflict = private.clone();
    conflict.content = "different".into();
    assert!(
        persist_message(
            &pool,
            None,
            session,
            true,
            PM_RETENTION_DAYS,
            &conflict,
            &[]
        )
        .await
        .is_err()
    );
    for (user, preferences) in [
        (bob, serde_json::json!({"allowPrivateMessages":false})),
        (
            bob,
            serde_json::json!({"ignored":[{"id":alice.to_string(),"name":"Alice"}]}),
        ),
        (
            alice,
            serde_json::json!({"ignored":[{"id":bob.to_string(),"name":"Bob"}]}),
        ),
    ] {
        sqlx::query("UPDATE users SET preferences='{}'::jsonb WHERE id=ANY($1)")
            .bind(vec![alice, bob])
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("UPDATE users SET preferences=$2 WHERE id=$1")
            .bind(user)
            .bind(preferences)
            .execute(&pool)
            .await
            .unwrap();
        let rejected = persist_message(
            &pool,
            None,
            session,
            true,
            PM_RETENTION_DAYS,
            &make(5, Some(bob)),
            &[],
        )
        .await;
        assert!(
            matches!(rejected, Err(RoomError::Refused(_))),
            "room PM persistence observes the account preference saved by another device"
        );
    }
    sqlx::query("UPDATE users SET preferences='{}'::jsonb WHERE id=ANY($1)")
        .bind(vec![alice, bob])
        .execute(&pool)
        .await
        .unwrap();
    let private_page = page(
        &pool,
        &private_conversation(alice, bob),
        Some(bob),
        90,
        &HistoryQuery::default(),
    )
    .await
    .unwrap();
    assert_eq!(private_page.messages.len(), 1);
    assert!(
        page(
            &pool,
            &private_conversation(alice, stranger),
            Some(stranger),
            90,
            &HistoryQuery::default()
        )
        .await
        .unwrap()
        .messages
        .is_empty()
    );
    assert!(
        mark_read(
            &pool,
            &private_conversation(alice, stranger),
            stranger,
            private.message_id.as_uuid()
        )
        .await
        .is_err()
    );
    let inbox_rows: i64 =
        sqlx::query_scalar("SELECT count(*) FROM chat_inbox WHERE user_id=ANY($1)")
            .bind(vec![alice, bob])
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        inbox_rows, 2,
        "both accounts discover the conversation after reconnect/restart"
    );
    sqlx::query("UPDATE chat_messages SET expires_at=now()-interval '1 second' WHERE id=$1")
        .bind(first.message_id.as_uuid())
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        page(&pool, &public, None, 7, &HistoryQuery::default())
            .await
            .unwrap()
            .messages
            .len(),
        2,
        "expired content is hidden before physical cleanup"
    );
    set_retention(&pool, room.as_str(), 0).await.unwrap();
    let remaining: i64 = sqlx::query_scalar("SELECT count(*) FROM chat_messages WHERE room_id=$1")
        .bind(&room)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        remaining, 0,
        "turning history off immediately erases public data"
    );
    let markers: i64 =
        sqlx::query_scalar("SELECT count(*) FROM chat_read_cursors WHERE conversation=$1")
            .bind(&public)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(markers, 0);
    assert_eq!(
        page(
            &pool,
            &private_conversation(alice, bob),
            Some(bob),
            90,
            &HistoryQuery::default()
        )
        .await
        .unwrap()
        .messages
        .len(),
        1,
        "public history settings never erase PMs"
    );
    sqlx::query("DELETE FROM rooms WHERE id=$1")
        .bind(&room)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM users WHERE id=ANY($1)")
        .bind(vec![alice, bob, stranger])
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
}
