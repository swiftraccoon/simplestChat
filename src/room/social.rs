//! Room-session chat and owner/moderator community tools.
//! Private text is retained only in bounded memory, never in reports or logs.
use super::*;
use crate::room::moderation::{
    Actor, MAX_MODERATION_EVENTS, ModerationAction, ModerationEvent, Target, record_event,
};
use crate::signaling::protocol::{
    ChatEntry, ChatReaction, ChatReplyRef, ChatRetryOutcome, ChatRetryReason, ChatStyle,
    ChatStyleKind, ClientMessage, REACTIONS, valid_correlation_id,
};
use serde::Serialize;
use serde_json::{Value, json};
use uuid::Uuid;

const HISTORY_MESSAGES: usize = 300;
/// Reaction records one message keeps, so a popular message stays within budget.
const MESSAGE_REACTIONS: usize = 64;
/// Characters a reply quotes from the message it answers.
const REPLY_EXCERPT_CHARS: usize = 140;
const HISTORY_BYTES: usize = 256 * 1024;
const MAX_IGNORED: usize = 100;
const MAX_REPORTS: usize = 500;
const MAX_RUNTIME_BANS: usize = 2000;
/// History entries an unpersisted room keeps in memory.
const MAX_RUNTIME_MODERATION_EVENTS: usize = 200;
const PAGE_SIZE: usize = 100;
/// One typing notice per sender per conversation this often, at most.
const TYPING_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);
const CHAT_RECEIPT_TTL: std::time::Duration = std::time::Duration::from_secs(300);
const CHAT_RECEIPTS: usize = 512;
const CHAT_RECEIPT_BYTES: usize = 512 * 1024;
const CHAT_RECEIPTS_PER_MEMBER: usize = 128;
const MAX_CHAT_SEQUENCE: u64 = 9_007_199_254_740_991;

type BanRow = (
    Uuid,
    Option<String>,
    bool,
    Option<String>,
    Option<chrono::DateTime<chrono::Utc>>,
);
type ReportRow = (
    Uuid,
    String,
    String,
    String,
    String,
    String,
    String,
    chrono::DateTime<chrono::Utc>,
    Option<chrono::DateTime<chrono::Utc>>,
    Option<String>,
    Option<chrono::DateTime<chrono::Utc>>,
);

/// Owned projection: capture one consistent room view under a read lock, then
/// serialize it after releasing the lock so snapshot encoding cannot block writes.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RoomSnapshot {
    participants: Vec<ParticipantInfo>,
    messages: Vec<ChatEntry>,
    lobby: Vec<SnapshotLobbyEntry>,
    your_role: &'static str,
    chat_session_id: Uuid,
    room_settings: Option<settings::RoomSettings>,
    nickname: String,
    allow_private_messages: bool,
    ignored_participant_ids: Vec<String>,
    paused_producer_ids: Vec<String>,
    text_muted: bool,
    cam_banned: bool,
    can_chat: bool,
    local_producer_ids: Vec<String>,
    can_broadcast: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SnapshotLobbyEntry {
    participant_id: String,
    display_name: String,
    authenticated: bool,
}

#[derive(Clone)]
pub(crate) struct ParticipantSocial {
    pub(super) connected: bool,
    chat_session_id: Uuid,
    chat_high_water: u64,
    joined_sequence: u64,
    allow_private_messages: bool,
    ignored: HashSet<String>,
    last_typing: Option<std::time::Instant>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(json: &str) -> ClientMessage {
        serde_json::from_str(json).unwrap()
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
            message(r#"{"type":"setMemberRole","requestId":"r","targetUserId":"u","role":1}"#)
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
        let mut room = Room::new("room".to_string(), "router".to_string(), None, false, None);
        let (alice, _alice_rx) = participant("alice");
        let (bob, _bob_rx) = participant("bob");
        let (alice_id, alice_sender) = (alice.id.clone(), alice.sender.clone());
        let bob_id = bob.id.clone();
        room.participants.insert(alice_id.clone(), alice);
        room.participants.insert(bob_id.clone(), bob);

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
        assert!(room.recent_chat_broadcasts.is_empty());
        RoomManager::process_social_chat(
            &mut room,
            &alice_id,
            &alice_sender,
            "public".to_string(),
            None,
            None,
        )
        .unwrap();
        assert_eq!(room.recent_chat_broadcasts.len(), 1);
    }

    fn participant(name: &str) -> (Participant, mpsc::Receiver<crate::OutboundJson>) {
        let (sender, receiver) = mpsc::channel(16);
        (
            Participant {
                id: Uuid::new_v4().to_string(),
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
        let mut room = Room::new("social".into(), "router".into(), None, false, None);
        for participant in [&alice, &bob, &carol] {
            room.participants
                .insert(participant.id.clone(), participant.clone());
        }
        (room, alice, bob, carol, vec![arx, brx, crx])
    }

    fn chat(room: &mut Room, sender: &Participant, id: &str, target: Option<&str>) -> Result<()> {
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
            .insert(alice.id.clone());
        room.relay_typing(&alice.id, &alice.sender, None).unwrap();
        let event: Value = serde_json::from_str(&receivers[1].try_recv().unwrap()).unwrap();
        assert_eq!(event["type"], "participantTyping");
        assert_eq!(event["participantId"], alice.id);
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
        assert_eq!(event["targetParticipantId"], bob.id);
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
        room.relay_typing(&alice.id, &alice.sender, Some("nobody"))
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
            .insert(alice.id.clone());
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
            chrono::DateTime::parse_from_rfc3339(ack["message"]["sentAt"].as_str().unwrap())
                .is_ok()
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
            .insert(alice.id.clone());
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
            .insert(bob.id.clone());
        assert!(chat(&mut room, &alice, "three", Some(&bob.id)).is_err());
        assert!(chat(&mut room, &alice, "four", Some(&Uuid::new_v4().to_string())).is_err());
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
        target: Option<&str>,
        retry: Option<String>,
    ) -> Result<()> {
        RoomManager::process_chat_attempt(
            room,
            &sender.id,
            &sender.sender,
            ChatAttempt {
                content: "hello".into(),
                client_message_id: Some(id.into()),
                recipient_id: target.map(String::from),
                sequence: Some(sequence),
                retry_session: retry,
                reply_to: None,
            },
        )
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
                message_id: Uuid::new_v4().to_string(),
                client_message_id: index.to_string(),
                participant_id: alice.id.clone(),
                participant_name: alice.name.clone(),
                recipient_id: None,
                recipient_name: None,
                content: "x".repeat(4096),
                sent_at: chrono::Utc::now().to_rfc3339(),
                chat_style: Default::default(),
                reply_to: None,
                reactions: Vec::new(),
            };
            room.social.remember(alice.media_session_id, None, message);
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
        let mut settings = RoomManager::default_room_settings("social");
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
            .insert(alice.id.clone());
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
            vec![Uuid::new_v4().to_string()],
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
        let error = serde_json::to_value(command.social_error("Unavailable").unwrap()).unwrap();
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
        target: Option<&str>,
        content: &str,
        reply_to: Option<&str>,
    ) -> Result<()> {
        RoomManager::process_chat_attempt(
            room,
            &sender.id,
            &sender.sender,
            ChatAttempt {
                content: content.into(),
                client_message_id: Some(id.into()),
                recipient_id: target.map(String::from),
                sequence: None,
                retry_session: None,
                reply_to: reply_to.map(String::from),
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
        let original = room
            .social
            .history
            .back()
            .unwrap()
            .message
            .message_id
            .clone();
        drain(&mut receivers);
        reply(&mut room, &bob, "reply-1", None, "agreed", Some(&original)).unwrap();
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
        assert_eq!(public["replyTo"]["messageId"], original.as_str());
        // A private reply cannot quote public text, nor anything it cannot see.
        assert!(
            reply(
                &mut room,
                &bob,
                "reply-2",
                Some(&carol.id),
                "psst",
                Some(&original)
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
        let private = room
            .social
            .history
            .back()
            .unwrap()
            .message
            .message_id
            .clone();
        assert!(reply(&mut room, &carol, "reply-4", None, "me too", Some(&private)).is_err());
        reply(
            &mut room,
            &bob,
            "reply-5",
            Some(&alice.id),
            "ok",
            Some(&private),
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
        let message = room
            .social
            .history
            .back()
            .unwrap()
            .message
            .message_id
            .clone();
        drain(&mut receivers);
        let emojis = |reactions: &[ChatReaction]| -> Vec<(String, Vec<String>)> {
            reactions
                .iter()
                .map(|r| (r.emoji.clone(), r.participant_ids.clone()))
                .collect()
        };
        let added = react_to_message(&mut room, &bob.id, &message, "🎉").unwrap();
        assert_eq!(
            emojis(&added),
            vec![("🎉".to_string(), vec![bob.id.clone()])]
        );
        for receiver in &mut receivers {
            let event: Value = serde_json::from_str(&receiver.try_recv().unwrap()).unwrap();
            assert_eq!(event["type"], "messageReactions");
            assert_eq!(event["messageId"], message.as_str());
        }
        react_to_message(&mut room, &carol.id, &message, "🎉").unwrap();
        let taken_back = react_to_message(&mut room, &bob.id, &message, "🎉").unwrap();
        assert_eq!(
            emojis(&taken_back),
            vec![("🎉".to_string(), vec![carol.id.clone()])]
        );
        assert!(react_to_message(&mut room, &bob.id, &message, "🍕").is_err());
        assert!(react_to_message(&mut room, &bob.id, "missing", "🎉").is_err());
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
        let private = room
            .social
            .history
            .back()
            .unwrap()
            .message
            .message_id
            .clone();
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
            room.participants.insert(member.id.clone(), member.clone());
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
        let mut manager = RoomManager::new(config, ServerMetrics::new(), Some(pool.clone()))
            .await
            .unwrap();
        manager.allow_ad_hoc_rooms = true;
        let room_id = format!("style-{}", Uuid::new_v4());
        let join = |sent: ChatStyle| {
            let (tx, rx) = mpsc::channel(64);
            let manager = &manager;
            let room_id = room_id.clone();
            async move {
                let joined = manager
                    .add_participant(
                        &room_id,
                        user.to_string(),
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
                &user.to_string(),
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
            .remove_participant_for_sender(&room_id, &user.to_string(), &tx)
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
    async fn database_reports_offline_roles_and_opaque_bans_are_room_scoped() {
        let database_url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL");
        let pool = sqlx::PgPool::connect(&database_url).await.unwrap();
        sqlx::raw_sql(include_str!("../../migrations/014_create_room_reports.sql"))
            .execute(&pool)
            .await
            .unwrap();
        let (mut room, mut owner, bob, mut absent, mut receivers) = fixture();
        let room_id = format!("social-{}", Uuid::new_v4());
        let other_room_id = format!("other-{}", Uuid::new_v4());
        owner.role = roles::Role::Owner;
        absent.role = roles::Role::Member;
        let owner_id: Uuid = owner.id.parse().unwrap();
        let absent_id: Uuid = absent.id.parse().unwrap();
        let user_ids: Vec<Uuid> = [&owner, &bob, &absent]
            .iter()
            .map(|p| p.id.parse().unwrap())
            .collect();
        for participant in [&owner, &bob, &absent] {
            sqlx::query("INSERT INTO users(id,email,display_name) VALUES($1,$2,$3)")
                .bind(participant.id.parse::<Uuid>().unwrap())
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
        let mut settings = RoomManager::default_room_settings(&room_id);
        settings.owner_id = owner_id;
        room.id = room_id.clone();
        room.persisted = true;
        room.settings = Some(settings);
        room.participants.insert(owner.id.clone(), owner.clone());
        room.participants.remove(&absent.id);
        let mut config = MediaConfig::default();
        config.worker_config.num_workers = 1;
        config.webrtc_server_port_base = 0;
        let manager = RoomManager::new(config, ServerMetrics::new(), Some(pool.clone()))
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
        assert!(
            members
                .iter()
                .any(|m| m["userId"] == absent.id && m["online"] == false && m["role"] == "member")
        );
        manager
            .handle_social_request(
                &room_id,
                &owner.id,
                &owner.sender,
                &ClientMessage::SetMemberRole {
                    request_id: "promote".into(),
                    target_user_id: absent.id.clone(),
                    role: 3,
                },
            )
            .await
            .unwrap();
        receivers[0].try_recv().unwrap();
        let role: i16 =
            sqlx::query_scalar("SELECT role FROM room_roles WHERE room_id=$1 AND user_id=$2")
                .bind(&room_id)
                .bind(absent_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(role, 2);
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
                        target_user_id: Uuid::new_v4().to_string(),
                        role: 2
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
                    target_participant_id: owner.id.clone(),
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
                    status: "resolved".into(),
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
        let remaining: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM room_states WHERE id=ANY($1)")
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
            .bind(&room_id).bind(&owner.id).bind(&bob.id).execute(&pool).await.unwrap();
        manager
            .handle_social_request(
                &room_id,
                &owner.id,
                &owner.sender,
                &ClientMessage::ReportParticipant {
                    request_id: "bounded".into(),
                    target_participant_id: bob.id.clone(),
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
                        target_participant_id: owner.id.clone(),
                        reason: "Capacity check".into()
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
                &ClientMessage::SetChatPreferences {
                    request_id: "prefs".into(),
                    allow_private_messages: false,
                    ignored_participant_ids: vec![absent.id.clone()],
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
        assert_eq!(snapshot["data"]["ignoredParticipantIds"][0], absent.id);
        assert_eq!(snapshot["data"]["canChat"], true);
        assert!(
            snapshot["data"]["participants"]
                .as_array()
                .unwrap()
                .iter()
                .all(|p| p["id"] != owner.id && p["authenticated"] == true)
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
}
impl ParticipantSocial {
    pub(crate) fn new(joined_sequence: u64) -> Self {
        Self {
            connected: true,
            chat_session_id: Uuid::new_v4(),
            chat_high_water: 0,
            joined_sequence,
            allow_private_messages: true,
            ignored: HashSet::new(),
            last_typing: None,
        }
    }
}

struct HistoryEntry {
    sequence: u64,
    sender_session: Uuid,
    recipient_session: Option<Uuid>,
    message: ChatEntry,
    /// Serialized size counted against `HISTORY_BYTES`; reactions change it.
    bytes: usize,
}

struct ChatReceipt {
    sender_session: Uuid,
    sequence: Option<u64>,
    accepted_at: std::time::Instant,
    bytes: usize,
    message: ChatEntry,
}

struct ChatAttempt {
    content: String,
    client_message_id: Option<String>,
    recipient_id: Option<String>,
    sequence: Option<u64>,
    retry_session: Option<String>,
    reply_to: Option<String>,
}

const REPORT_COOLDOWN: std::time::Duration = std::time::Duration::from_secs(30);

/// Cooldown identity for abuse reports: the account for signed-in members,
/// the canonical guest address otherwise. A session-scoped cooldown reset on
/// every rejoin and let one identity fill the room's report capacity.
pub(crate) fn reporter_key(participant: &Participant) -> String {
    if participant.authenticated {
        format!("user:{}", participant.id)
    } else if let Some(ip) = participant.ip {
        format!("ip:{ip}")
    } else {
        format!("session:{}", participant.id)
    }
}

#[derive(Default)]
pub(crate) struct RoomSocial {
    pub(crate) next_sequence: u64,
    report_cooldowns: HashMap<String, std::time::Instant>,
    history: VecDeque<HistoryEntry>,
    history_bytes: usize,
    receipts: VecDeque<ChatReceipt>,
    receipt_bytes: usize,
    bans: HashMap<String, RuntimeBan>,
    reports: VecDeque<ReportEntry>,
    moderation_events: VecDeque<ModerationEvent>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BanEntry {
    ban_id: String,
    display_name: String,
    authenticated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    expires_at: Option<String>,
}
struct RuntimeBan {
    entry: BanEntry,
    participant_ids: Vec<String>,
    guest_ip: Option<IpAddr>,
    expiry: Option<std::time::Instant>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ReportEntry {
    report_id: String,
    reporter_id: String,
    reporter_name: String,
    target_participant_id: String,
    target_name: String,
    reason: String,
    status: String,
    created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    resolved_at: Option<String>,
    /// The newest history entry that answered this report, if any did.
    #[serde(skip_serializing_if = "Option::is_none")]
    outcome: Option<ReportOutcome>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ReportOutcome {
    action: String,
    created_at: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct MemberEntry {
    user_id: String,
    display_name: String,
    role: String,
    online: bool,
    authenticated: bool,
}

#[derive(Debug)]
pub(crate) struct SocialFailure(pub String);
impl std::fmt::Display for SocialFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for SocialFailure {}
/// An account's saved chat look, or `None` for an unknown account. A color the
/// palette has since dropped falls back to the automatic one.
pub(crate) async fn load_chat_style(
    pool: &sqlx::PgPool,
    user: Uuid,
) -> Result<Option<ChatStyle>, sqlx::Error> {
    let row: Option<(Option<String>, String)> =
        sqlx::query_as("SELECT chat_color, chat_style FROM users WHERE id = $1")
            .bind(user)
            .fetch_optional(pool)
            .await?;
    Ok(row.map(|(color, style)| {
        let style = match style.as_str() {
            "text" => ChatStyleKind::Text,
            "bubble" => ChatStyleKind::Bubble,
            _ => ChatStyleKind::Accent,
        };
        ChatStyle { color, style }
            .validated()
            .unwrap_or(ChatStyle { color: None, style })
    }))
}

async fn save_chat_style(
    pool: &sqlx::PgPool,
    user: Uuid,
    style: &ChatStyle,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE users SET chat_color = $2, chat_style = $3, updated_at = now() WHERE id = $1",
    )
    .bind(user)
    .bind(style.color.as_deref())
    .bind(style.style.as_str())
    .execute(pool)
    .await?;
    Ok(())
}

fn rejected(message: &str) -> anyhow::Error {
    SocialFailure(message.to_string()).into()
}

/// Which room-wide budget a social request must reserve before it runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SocialBudget {
    None,
    ChatBroadcast,
    AdminMutation,
}

impl ClientMessage {
    /// Read-only queries and self-scoped preferences never draw on a room-wide
    /// window, so ordinary members cannot starve moderation or public chat.
    /// A nickname change fans out like a chat message; durable moderation
    /// writes share the administration window with kick, ban and settings.
    pub(crate) fn social_budget(&self) -> SocialBudget {
        match self {
            Self::GetRoomSnapshot { .. }
            | Self::ListRoomBans { .. }
            | Self::ListRoomMembers { .. }
            | Self::ListRoomReports { .. }
            | Self::ListModerationEvents { .. }
            | Self::SetChatPreferences { .. } => SocialBudget::None,
            Self::ChangeNickname { .. }
            | Self::SetChatStyle { .. }
            | Self::ReactToMessage { .. } => SocialBudget::ChatBroadcast,
            _ => SocialBudget::AdminMutation,
        }
    }

    pub(crate) fn social_request(&self) -> Option<(&str, &'static str)> {
        match self {
            Self::SetChatPreferences { request_id, .. } => Some((request_id, "setChatPreferences")),
            Self::ChangeNickname { request_id, .. } => Some((request_id, "changeNickname")),
            Self::SetChatStyle { request_id, .. } => Some((request_id, "setChatStyle")),
            Self::ReactToMessage { request_id, .. } => Some((request_id, "reactToMessage")),
            Self::GetRoomSnapshot { request_id } => Some((request_id, "getRoomSnapshot")),
            Self::ListRoomBans { request_id, .. } => Some((request_id, "listRoomBans")),
            Self::RemoveRoomBan { request_id, .. } => Some((request_id, "removeRoomBan")),
            Self::ListRoomMembers { request_id, .. } => Some((request_id, "listRoomMembers")),
            Self::SetMemberRole { request_id, .. } => Some((request_id, "setMemberRole")),
            Self::ReportParticipant { request_id, .. } => Some((request_id, "reportParticipant")),
            Self::ListRoomReports { request_id, .. } => Some((request_id, "listRoomReports")),
            Self::ResolveRoomReport { request_id, .. } => Some((request_id, "resolveRoomReport")),
            Self::ListModerationEvents { request_id, .. } => {
                Some((request_id, "listModerationEvents"))
            }
            _ => None,
        }
    }
    pub(crate) fn social_error(&self, message: &str) -> Option<ServerMessage> {
        let request_id = self.social_request().map(|(id, _)| id.to_string());
        let client_message_id = match self {
            Self::ChatMessage {
                client_message_id, ..
            } => client_message_id.clone(),
            Self::PrivateMessage {
                client_message_id, ..
            } => Some(client_message_id.clone()),
            Self::RetryChatMessage(message) => Some(message.client_message_id.clone()),
            _ => None,
        };
        if request_id.is_none() && client_message_id.is_none() {
            return None;
        }
        Some(ServerMessage::SocialError {
            request_id,
            client_message_id,
            message: message.to_string(),
        })
    }
}

fn send(
    metrics: &ServerMetrics,
    sender: &mpsc::Sender<crate::OutboundJson>,
    message: &ServerMessage,
) -> Result<()> {
    try_send_essential(
        metrics,
        sender,
        crate::OutboundJson::from(serde_json::to_string(message)?),
    )
    .map_err(|_| rejected("Connection is busy; please retry"))
}

fn acknowledge_chat(
    metrics: &ServerMetrics,
    sender: &mpsc::Sender<crate::OutboundJson>,
    message: ChatEntry,
) -> Result<()> {
    let json = crate::OutboundJson::from(serde_json::to_string(&ServerMessage::MessageAck {
        client_message_id: message.client_message_id.clone(),
        message,
    })?);
    // Acceptance already happened. Queue rejection is counted, but must not
    // become a SocialError claiming the message failed. The sender retains an
    // unconfirmed attempt and can reconcile its receipt after reconnecting.
    let _ = try_send_essential(metrics, sender, json);
    Ok(())
}
fn page_offset(offset: Option<u32>) -> Result<usize> {
    let offset = offset.unwrap_or(0) as usize;
    if offset > 10_000 {
        return Err(rejected("Invalid page"));
    }
    Ok(offset)
}
fn require_role(actual: roles::Role, required: roles::Role) -> Result<()> {
    if actual < required {
        return Err(rejected("This action requires a room moderator"));
    }
    Ok(())
}
fn visible(entry: &HistoryEntry, viewer: &Participant) -> bool {
    if entry.sequence < viewer.social.joined_sequence {
        return false;
    }
    if entry.message.recipient_id.is_some() {
        entry.sender_session == viewer.media_session_id
            || entry.recipient_session == Some(viewer.media_session_id)
    } else {
        !viewer
            .social
            .ignored
            .contains(&entry.message.participant_id)
    }
}
fn replied_to(message: &ChatEntry) -> Option<&str> {
    message
        .reply_to
        .as_ref()
        .map(|reply| reply.message_id.as_str())
}
/// Quotes the retained message a reply answers. The replier must be able to see it,
/// and it must belong to the same conversation: public with public, or the same pair.
fn quote_reply(
    history: &VecDeque<HistoryEntry>,
    sender: &Participant,
    recipient_id: Option<&str>,
    message_id: &str,
) -> Result<ChatReplyRef> {
    let original = history
        .iter()
        .rev()
        .find(|entry| entry.message.message_id == message_id && visible(entry, sender))
        .ok_or_else(|| rejected("That message is no longer available to reply to"))?;
    let message = &original.message;
    let same_conversation = match (recipient_id, message.recipient_id.as_deref()) {
        (None, None) => true,
        (Some(to), Some(other)) => {
            let pair = [message.participant_id.as_str(), other];
            pair.contains(&to) && pair.contains(&sender.id.as_str())
        }
        _ => false,
    };
    if !same_conversation {
        return Err(rejected(
            "Reply within the conversation the message belongs to",
        ));
    }
    let flat = message
        .content
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let mut chars = flat.chars();
    let head: String = chars.by_ref().take(REPLY_EXCERPT_CHARS).collect();
    let excerpt = if chars.next().is_some() {
        format!("{}…", head.trim_end())
    } else {
        head
    };
    Ok(ChatReplyRef {
        message_id: message.message_id.clone(),
        participant_id: message.participant_id.clone(),
        participant_name: message.participant_name.clone(),
        excerpt,
    })
}
/// Adds `participant_id`'s reaction to a retained message it can see, or takes it
/// back, and tells everyone who can see the message, the reactor included.
fn react_to_message(
    room: &mut Room,
    participant_id: &str,
    message_id: &str,
    emoji: &str,
) -> Result<Vec<ChatReaction>> {
    if !REACTIONS.contains(&emoji) {
        return Err(rejected("Choose one of the offered reactions"));
    }
    let actor = room
        .participants
        .get(participant_id)
        .ok_or_else(|| rejected("Participant not found"))?;
    let index = room
        .social
        .history
        .iter()
        .position(|entry| entry.message.message_id == message_id && visible(entry, actor))
        .ok_or_else(|| rejected("That message is no longer available"))?;
    let reactor = participant_id.to_string();
    let reactions = &mut room.social.history[index].message.reactions;
    match reactions
        .iter()
        .position(|reaction| reaction.emoji == emoji)
    {
        Some(at) if reactions[at].participant_ids.contains(&reactor) => {
            reactions[at].participant_ids.retain(|id| *id != reactor);
            if reactions[at].participant_ids.is_empty() {
                reactions.remove(at);
            }
        }
        found => {
            let total: usize = reactions.iter().map(|r| r.participant_ids.len()).sum();
            if total >= MESSAGE_REACTIONS {
                return Err(rejected("This message has all the reactions it can hold"));
            }
            match found {
                Some(at) => reactions[at].participant_ids.push(reactor),
                None => reactions.push(ChatReaction {
                    emoji: emoji.to_string(),
                    participant_ids: vec![reactor],
                }),
            }
        }
    }
    let reactions = reactions.clone();
    let event =
        crate::OutboundJson::from(serde_json::to_string(&ServerMessage::MessageReactions {
            message_id: message_id.to_string(),
            reactions: reactions.clone(),
        })?);
    let entry = &room.social.history[index];
    for viewer in room.participants.values().filter(|p| visible(entry, p)) {
        let _ = try_send_essential(&room.metrics, &viewer.sender, event.clone());
    }
    room.social.recount(index);
    Ok(reactions)
}
fn can_private_message(sender: &Participant, recipient: &Participant) -> bool {
    sender.id != recipient.id
        && recipient.social.allow_private_messages
        && !recipient.social.ignored.contains(&sender.id)
        && !sender.social.ignored.contains(&recipient.id)
}
impl RoomSocial {
    fn prune_receipts(&mut self, now: std::time::Instant) {
        while self
            .receipts
            .front()
            .is_some_and(|entry| now.duration_since(entry.accepted_at) >= CHAT_RECEIPT_TTL)
        {
            if let Some(entry) = self.receipts.pop_front() {
                self.receipt_bytes = self.receipt_bytes.saturating_sub(entry.bytes);
            }
        }
    }

    fn make_receipt_room(&mut self, sender_session: Uuid, bytes: usize) {
        // Accepted newer messages must not stall on a confirmation cache. The
        // membership watermark still prevents evicted sequenced attempts from
        // being delivered twice; their status simply becomes unconfirmed.
        while self
            .receipts
            .iter()
            .filter(|entry| entry.sender_session == sender_session)
            .count()
            >= CHAT_RECEIPTS_PER_MEMBER
        {
            let index = self
                .receipts
                .iter()
                .position(|entry| entry.sender_session == sender_session)
                .expect("member receipt count checked");
            let entry = self.receipts.remove(index).expect("receipt index checked");
            self.receipt_bytes -= entry.bytes;
        }
        while self.receipts.len() >= CHAT_RECEIPTS
            || bytes > CHAT_RECEIPT_BYTES.saturating_sub(self.receipt_bytes)
        {
            let Some(entry) = self.receipts.pop_front() else {
                break;
            };
            self.receipt_bytes -= entry.bytes;
        }
    }

    /// Whether this identity reported within the cooldown. Expired entries
    /// are dropped here, so the map never outgrows one cooldown of reporters.
    pub(crate) fn report_cooldown_active(&mut self, key: &str, now: std::time::Instant) -> bool {
        self.report_cooldowns
            .retain(|_, at| now.duration_since(*at) < REPORT_COOLDOWN);
        self.report_cooldowns
            .get(key)
            .is_some_and(|at| now.duration_since(*at) < REPORT_COOLDOWN)
    }

    pub(crate) fn note_report(&mut self, key: String, now: std::time::Instant) {
        self.report_cooldowns.insert(key, now);
    }

    /// Drop an account's runtime ban and say what name it was recorded under.
    pub(crate) fn forget_user_ban(&mut self, participant_id: &str) -> Option<String> {
        let mut name = None;
        self.bans.retain(|_, ban| {
            let matches = ban.entry.authenticated
                && ban.participant_ids.iter().any(|id| id == participant_id);
            if matches {
                name = Some(ban.entry.display_name.clone());
            }
            !matches
        });
        name
    }

    pub(crate) fn record_moderation_event(&mut self, event: ModerationEvent) {
        if self.moderation_events.len() >= MAX_RUNTIME_MODERATION_EVENTS {
            self.moderation_events.pop_front();
        }
        self.moderation_events.push_back(event);
    }

    /// Resolve the open runtime report a sanction answers; a report this room
    /// never received is refused.
    pub(crate) fn link_report(&mut self, report_id: &str) -> Result<()> {
        let report = self
            .reports
            .iter_mut()
            .find(|report| report.report_id == report_id)
            .ok_or_else(|| rejected("Report not found"))?;
        if report.status == "open" {
            report.status = "resolved".to_string();
            report.resolved_at = Some(chrono::Utc::now().to_rfc3339());
        }
        Ok(())
    }

    fn report_outcome(&self, report_id: &str) -> Option<ReportOutcome> {
        self.moderation_events
            .iter()
            .rev()
            .find(|event| event.report_id.as_deref() == Some(report_id))
            .map(|event| ReportOutcome {
                action: event.action.as_str().to_string(),
                created_at: event.created_at.to_rfc3339(),
            })
    }
    fn remember(
        &mut self,
        sender_session: Uuid,
        recipient_session: Option<Uuid>,
        message: ChatEntry,
    ) {
        // Names and identifiers also count, keeping retention bounded even with short text.
        let bytes = serde_json::to_vec(&message).map_or(HISTORY_BYTES, |v| v.len());
        self.history_bytes += bytes;
        self.history.push_back(HistoryEntry {
            sequence: self.next_sequence,
            sender_session,
            recipient_session,
            message,
            bytes,
        });
        self.next_sequence = self.next_sequence.saturating_add(1);
        self.trim_history();
    }
    /// Re-counts an entry whose reactions changed, then trims to the budget.
    fn recount(&mut self, index: usize) {
        if let Some(entry) = self.history.get_mut(index) {
            let bytes = serde_json::to_vec(&entry.message).map_or(HISTORY_BYTES, |v| v.len());
            self.history_bytes = self.history_bytes.saturating_sub(entry.bytes) + bytes;
            entry.bytes = bytes;
        }
        self.trim_history();
    }
    fn trim_history(&mut self) {
        while self.history.len() > HISTORY_MESSAGES || self.history_bytes > HISTORY_BYTES {
            match self.history.pop_front() {
                Some(old) => self.history_bytes = self.history_bytes.saturating_sub(old.bytes),
                None => break,
            }
        }
    }
    pub(crate) fn reserve_ban(&mut self) -> Result<()> {
        let now = std::time::Instant::now();
        self.bans
            .retain(|_, ban| ban.expiry.is_none_or(|expiry| expiry > now));
        if self.bans.len() >= MAX_RUNTIME_BANS {
            return Err(rejected("Room ban capacity reached"));
        }
        Ok(())
    }
    pub(crate) fn record_ban(
        &mut self,
        name: String,
        authenticated: bool,
        participant_ids: Vec<String>,
        ip: Option<IpAddr>,
        reason: Option<&str>,
        duration: Option<u64>,
    ) {
        let id = Uuid::new_v4().to_string();
        self.bans.insert(
            id.clone(),
            RuntimeBan {
                entry: BanEntry {
                    ban_id: id,
                    display_name: name,
                    authenticated,
                    reason: reason.map(String::from),
                    expires_at: duration.map(|s| {
                        (chrono::Utc::now() + chrono::Duration::seconds(s as i64)).to_rfc3339()
                    }),
                },
                participant_ids,
                guest_ip: (!authenticated)
                    .then_some(ip)
                    .flatten()
                    .map(moderation::canonical_guest_ip),
                expiry: duration.and_then(|s| {
                    std::time::Instant::now().checked_add(std::time::Duration::from_secs(s))
                }),
            },
        );
    }
}

impl Room {
    /// Tell a conversation's recipients that `sender_id` is composing: everyone
    /// who would receive the message and does not ignore the sender, at most
    /// once per `TYPING_INTERVAL`. A sender who cannot chat is silently dropped.
    pub(crate) fn relay_typing(
        &mut self,
        sender_id: &str,
        expected_sender: &mpsc::Sender<crate::OutboundJson>,
        target: Option<&str>,
    ) -> Result<()> {
        let now = std::time::Instant::now();
        let moderated = self.settings.as_ref().is_some_and(|s| s.moderated);
        let chat_allowed = !self.settings.as_ref().is_some_and(|s| !s.allow_chat);
        let (relay, target_accepts) = {
            let sender = RoomManager::participant_for_sender(self, sender_id, expected_sender)?;
            let allowed = chat_allowed
                && moderation::can_chat(&sender.punitive, sender.role, moderated)
                && !sender
                    .social
                    .last_typing
                    .is_some_and(|last| now.duration_since(last) < TYPING_INTERVAL);
            let target_accepts = target.is_some_and(|target| {
                target != sender_id
                    && !sender.social.ignored.contains(target)
                    && self.participants.get(target).is_some_and(|recipient| {
                        recipient.social.allow_private_messages
                            && !recipient.social.ignored.contains(sender_id)
                    })
            });
            (allowed, target_accepts)
        };
        if !relay {
            return Ok(());
        }
        if let Some(sender) = self.participants.get_mut(sender_id) {
            sender.social.last_typing = Some(now);
        }
        let event = ServerMessage::ParticipantTyping {
            participant_id: sender_id.to_string(),
            target_participant_id: target.map(String::from),
        };
        let json = crate::OutboundJson::from(serde_json::to_string(&event)?);
        match target {
            Some(target) => {
                if target_accepts && let Some(recipient) = self.participants.get(target) {
                    self.try_send_broadcast(&recipient.sender, json, &event);
                }
            }
            None => {
                for participant in self.participants.values() {
                    if participant.id != sender_id
                        && !participant.social.ignored.contains(sender_id)
                    {
                        self.try_send_broadcast(&participant.sender, json.clone(), &event);
                    }
                }
            }
        }
        Ok(())
    }
}

impl RoomManager {
    pub async fn relay_typing(
        &self,
        room_id: &str,
        sender_id: &str,
        expected_sender: &mpsc::Sender<crate::OutboundJson>,
        target: Option<&str>,
    ) -> Result<()> {
        let room_lock = self.get_room(room_id)?;
        let mut room = room_lock.write().await;
        room.relay_typing(sender_id, expected_sender, target)
    }

    pub async fn handle_chat_command(
        &self,
        room_id: &str,
        sender_id: &str,
        expected_sender: &mpsc::Sender<crate::OutboundJson>,
        command: &ClientMessage,
    ) -> Result<()> {
        let attempt = match command {
            ClientMessage::ChatMessage {
                content,
                client_message_id,
                sequence,
                reply_to,
            } => ChatAttempt {
                content: content.clone(),
                client_message_id: client_message_id.clone(),
                recipient_id: None,
                sequence: *sequence,
                retry_session: None,
                reply_to: reply_to.clone(),
            },
            ClientMessage::PrivateMessage {
                content,
                client_message_id,
                target_participant_id,
                sequence,
                reply_to,
            } => ChatAttempt {
                content: content.clone(),
                client_message_id: Some(client_message_id.clone()),
                recipient_id: Some(target_participant_id.clone()),
                sequence: *sequence,
                retry_session: None,
                reply_to: reply_to.clone(),
            },
            ClientMessage::RetryChatMessage(message) => ChatAttempt {
                content: message.content.clone(),
                client_message_id: Some(message.client_message_id.clone()),
                recipient_id: message.target_participant_id.clone(),
                sequence: Some(message.sequence),
                retry_session: Some(message.chat_session_id.clone()),
                reply_to: message.reply_to.clone(),
            },
            _ => return Err(rejected("Invalid chat command")),
        };
        let room_lock = self.get_room(room_id)?;
        let mut room = room_lock.write().await;
        Self::process_chat_attempt(&mut room, sender_id, expected_sender, attempt)
    }

    pub async fn send_social_chat(
        &self,
        room_id: &str,
        sender_id: &str,
        expected_sender: &mpsc::Sender<crate::OutboundJson>,
        content: String,
        client_message_id: Option<String>,
        recipient_id: Option<&str>,
    ) -> Result<()> {
        let room_lock = self.get_room(room_id)?;
        let mut room = room_lock.write().await;
        Self::process_social_chat(
            &mut room,
            sender_id,
            expected_sender,
            content,
            client_message_id,
            recipient_id,
        )
    }

    fn process_social_chat(
        room: &mut Room,
        sender_id: &str,
        expected_sender: &mpsc::Sender<crate::OutboundJson>,
        content: String,
        client_message_id: Option<String>,
        recipient_id: Option<&str>,
    ) -> Result<()> {
        Self::process_chat_attempt(
            room,
            sender_id,
            expected_sender,
            ChatAttempt {
                content,
                client_message_id,
                recipient_id: recipient_id.map(String::from),
                sequence: None,
                retry_session: None,
                reply_to: None,
            },
        )
    }

    fn process_chat_attempt(
        room: &mut Room,
        sender_id: &str,
        expected_sender: &mpsc::Sender<crate::OutboundJson>,
        attempt: ChatAttempt,
    ) -> Result<()> {
        let ChatAttempt {
            content,
            client_message_id,
            recipient_id,
            sequence,
            retry_session,
            reply_to,
        } = attempt;
        let reply_to = reply_to.as_deref();
        let recipient_id = recipient_id.as_deref();
        if content.trim().is_empty()
            || content.len() > 4096
            || content
                .chars()
                .any(|c| c.is_control() && c != '\n' && c != '\t')
        {
            return Err(rejected("Message must contain 1–4096 bytes of text"));
        }
        let client_message_id = client_message_id.unwrap_or_else(|| Uuid::new_v4().to_string());
        if !valid_correlation_id(&client_message_id) {
            return Err(rejected("Invalid message ID"));
        }
        if sequence.is_some_and(|value| value == 0 || value > MAX_CHAT_SEQUENCE)
            || recipient_id.is_some_and(|id| id.parse::<Uuid>().is_err())
            || reply_to.is_some_and(|id| !valid_correlation_id(id))
        {
            return Err(rejected("Invalid chat attempt"));
        }
        room.ensure_live()?;
        let now = std::time::Instant::now();
        room.social.prune_receipts(now);
        let sender = Self::participant_for_sender(room, sender_id, expected_sender)?;
        let metrics = room.metrics.clone();
        let unknown = |reason| {
            send(
                &metrics,
                expected_sender,
                &ServerMessage::MessageRetryResult {
                    client_message_id: client_message_id.clone(),
                    outcome: ChatRetryOutcome::Unknown,
                    reason,
                },
            )
        };
        if retry_session.as_ref().is_some_and(|session| {
            session.parse::<Uuid>().ok() != Some(sender.social.chat_session_id)
        }) {
            return unknown(ChatRetryReason::SessionChanged);
        }
        let sender_session = sender.media_session_id;
        if let Some(existing) = room.social.receipts.iter().find(|entry| {
            entry.sender_session == sender_session
                && entry.message.client_message_id == client_message_id
        }) {
            if existing.message.content != content
                || existing.message.recipient_id.as_deref() != recipient_id
                || replied_to(&existing.message) != reply_to
                || existing.sequence != sequence
            {
                if retry_session.is_none() {
                    return Err(rejected("Message ID was already used"));
                }
                return unknown(ChatRetryReason::Conflict);
            }
            return acknowledge_chat(&room.metrics, expected_sender, existing.message.clone());
        }
        // This watermark survives receipt expiry and history eviction for the
        // exact membership. Missing older attempts must never be broadcast again.
        if let Some(sequence) = sequence
            && sequence <= sender.social.chat_high_water
        {
            return unknown(if sequence == sender.social.chat_high_water {
                ChatRetryReason::ReceiptExpired
            } else {
                ChatRetryReason::SequenceSuperseded
            });
        }
        // Without a receipt the original recipient's membership is unknown.
        // Never deliver retained private text into a replacement membership.
        if retry_session.is_some() && recipient_id.is_some() {
            return unknown(ChatRetryReason::RecipientUnconfirmed);
        }
        let moderated = room.settings.as_ref().is_some_and(|s| s.moderated);
        if room.settings.as_ref().is_some_and(|s| !s.allow_chat)
            || !moderation::can_chat(&sender.punitive, sender.role, moderated)
        {
            return Err(rejected("You are not allowed to chat"));
        }
        let sender_name = sender.name.clone();
        let sender_style = sender.chat_style.clone();
        if let Some(existing) = room.social.history.iter().find(|entry| {
            entry.sender_session == sender_session
                && entry.message.client_message_id == client_message_id
        }) {
            if existing.message.content != content
                || existing.message.recipient_id.as_deref() != recipient_id
                || replied_to(&existing.message) != reply_to
            {
                return Err(rejected("Message ID was already used"));
            }
            return acknowledge_chat(&room.metrics, expected_sender, existing.message.clone());
        }
        let recipient = if let Some(id) = recipient_id {
            let recipient = room
                .participants
                .get(id)
                .ok_or_else(|| rejected("Private message unavailable"))?;
            if !can_private_message(sender, recipient) {
                return Err(rejected("Private message unavailable"));
            }
            Some((
                recipient.name.clone(),
                recipient.media_session_id,
                recipient.sender.clone(),
            ))
        } else {
            None
        };
        let reply_to = match reply_to {
            Some(id) => Some(quote_reply(&room.social.history, sender, recipient_id, id)?),
            None => None,
        };
        let message = ChatEntry {
            message_id: Uuid::new_v4().to_string(),
            client_message_id: client_message_id.clone(),
            participant_id: sender_id.to_string(),
            participant_name: sender_name,
            recipient_id: recipient_id.map(String::from),
            recipient_name: recipient.as_ref().map(|p| p.0.clone()),
            content,
            sent_at: chrono::Utc::now().to_rfc3339(),
            chat_style: sender_style,
            reply_to,
            reactions: Vec::new(),
        };
        let receipt_bytes = serde_json::to_vec(&message)?.len();
        if receipt_bytes > CHAT_RECEIPT_BYTES {
            if retry_session.is_some() {
                return unknown(ChatRetryReason::Capacity);
            }
            return Err(rejected("Chat confirmation exceeds its size limit"));
        }
        // A private message reaches one bounded queue; only public fan-out
        // draws on the room-wide window. Rejecting a send must not evict an
        // accepted message's confirmation receipt.
        if recipient.is_none() && !room.reserve_chat_broadcast(now) {
            return Err(rejected("Room chat rate limit exceeded"));
        }
        if let Some((_, _, recipient_sender)) = &recipient {
            // Only acknowledged when the recipient's live connection accepted delivery.
            send(
                &room.metrics,
                recipient_sender,
                &ServerMessage::PrivateMessageReceived {
                    message: message.clone(),
                },
            )?;
        } else {
            let event = ServerMessage::ChatReceived {
                participant_id: message.participant_id.clone(),
                participant_name: message.participant_name.clone(),
                content: message.content.clone(),
                message_id: message.message_id.clone(),
                client_message_id: message.client_message_id.clone(),
                sent_at: message.sent_at.clone(),
                chat_style: message.chat_style.clone(),
                reply_to: message.reply_to.clone(),
            };
            let json = crate::OutboundJson::from(serde_json::to_string(&event)?);
            for participant in room.participants.values() {
                if participant.id != sender_id && !participant.social.ignored.contains(sender_id) {
                    let _ = try_send_essential(&room.metrics, &participant.sender, json.clone());
                }
            }
        }
        room.social.make_receipt_room(sender_session, receipt_bytes);
        room.social.receipt_bytes += receipt_bytes;
        room.social.receipts.push_back(ChatReceipt {
            sender_session,
            sequence,
            accepted_at: now,
            bytes: receipt_bytes,
            message: message.clone(),
        });
        if let Some(sequence) = sequence {
            room.participants
                .get_mut(sender_id)
                .expect("current sender checked")
                .social
                .chat_high_water = sequence;
        }
        room.social.remember(
            sender_session,
            recipient.as_ref().map(|r| r.1),
            message.clone(),
        );
        acknowledge_chat(&room.metrics, expected_sender, message)
    }

    pub async fn handle_social_request(
        &self,
        room_id: &str,
        participant_id: &str,
        expected_sender: &mpsc::Sender<crate::OutboundJson>,
        command: &ClientMessage,
    ) -> Result<()> {
        let (request_id, _) = command
            .social_request()
            .ok_or_else(|| rejected("Unknown request"))?;
        if !valid_correlation_id(request_id) {
            return Err(rejected("Invalid request ID"));
        }
        if let ClientMessage::GetRoomSnapshot { request_id } = command {
            let room_lock = self.get_room(room_id)?;
            let room = room_lock.read().await;
            room.ensure_live()?;
            let actor = Self::participant_for_sender(&room, participant_id, expected_sender)?;
            let snapshot = self.room_snapshot(&room, actor);
            drop(room);
            return send(
                &self.metrics,
                expected_sender,
                &ServerMessage::SocialResponse {
                    request_id: request_id.clone(),
                    action: "getRoomSnapshot".to_owned(),
                    data: serde_json::to_value(snapshot)?,
                },
            );
        }
        if matches!(
            command,
            ClientMessage::SetChatPreferences { .. } | ClientMessage::ChangeNickname { .. }
        ) {
            return self
                .handle_social_request_inner(
                    room_id,
                    participant_id,
                    expected_sender,
                    command,
                    None,
                )
                .await;
        }

        let participant_id = participant_id.to_owned();
        let sender = expected_sender.clone();
        let command = command.clone();
        self.run_room_control(room_id, move |manager, room_id, control| async move {
            manager
                .handle_social_request_inner(
                    &room_id,
                    &participant_id,
                    &sender,
                    &command,
                    Some(control),
                )
                .await
        })
        .await
    }

    fn room_snapshot(&self, room: &Room, actor: &Participant) -> RoomSnapshot {
        RoomSnapshot {
            participants: room
                .participants
                .values()
                .filter(|p| p.id != actor.id)
                .map(|p| ParticipantInfo {
                    id: p.id.clone(),
                    name: p.name.clone(),
                    role: p.role.name().to_owned(),
                    authenticated: p.authenticated,
                    chat_style: p.chat_style.clone(),
                    producers: p
                        .producers
                        .iter()
                        .map(|(id, (kind, source))| ProducerMetadata {
                            id: id.clone(),
                            kind: *kind,
                            source: source.clone(),
                        })
                        .collect(),
                })
                .collect(),
            messages: room
                .social
                .history
                .iter()
                .filter(|entry| visible(entry, actor))
                .map(|entry| entry.message.clone())
                .collect(),
            lobby: if actor.role >= roles::Role::Moderator {
                room.lobby
                    .values()
                    .map(|p| SnapshotLobbyEntry {
                        participant_id: p.participant_id.clone(),
                        display_name: p.name.clone(),
                        authenticated: p.authenticated,
                    })
                    .collect()
            } else {
                Vec::new()
            },
            your_role: actor.role.name(),
            chat_session_id: actor.social.chat_session_id,
            room_settings: room.settings.clone(),
            nickname: actor.name.clone(),
            allow_private_messages: actor.social.allow_private_messages,
            ignored_participant_ids: actor.social.ignored.iter().cloned().collect(),
            paused_producer_ids: room
                .participants
                .values()
                .flat_map(|p| p.producers.keys())
                .filter(|id| {
                    self.media_server
                        .transport_manager()
                        .find_producer_paused(id)
                        == Some(true)
                })
                .cloned()
                .collect(),
            text_muted: actor.punitive.text_muted,
            cam_banned: actor.punitive.cam_banned,
            can_chat: !room.settings.as_ref().is_some_and(|s| !s.allow_chat)
                && moderation::can_chat(
                    &actor.punitive,
                    actor.role,
                    room.settings.as_ref().is_some_and(|s| s.moderated),
                ),
            local_producer_ids: actor.producers.keys().cloned().collect(),
            can_broadcast: Self::participant_can_produce(
                room,
                actor,
                MediaKind::Audio,
                "microphone",
            ),
        }
    }

    async fn handle_social_request_inner(
        &self,
        room_id: &str,
        participant_id: &str,
        expected_sender: &mpsc::Sender<crate::OutboundJson>,
        command: &ClientMessage,
        control: Option<tokio::sync::OwnedMutexGuard<()>>,
    ) -> Result<()> {
        let (request_id, action) = command
            .social_request()
            .ok_or_else(|| rejected("Unknown request"))?;
        if !valid_correlation_id(request_id) {
            return Err(rejected("Invalid request ID"));
        }
        // Control admission keeps the online/offline decision stable. Reuse the
        // already-controlled role path rather than trying to acquire this gate again.
        if let ClientMessage::SetMemberRole {
            target_user_id,
            role,
            ..
        } = command
        {
            let new_role = roles::Role::from_u8(*role).ok_or_else(|| rejected("Invalid role"))?;
            let room_lock = self.get_room(room_id)?;
            let room = room_lock.read().await;
            Self::participant_for_sender(&room, participant_id, expected_sender)?;
            let online = room.participants.contains_key(target_user_id);
            drop(room);
            if online {
                self.set_participant_role_controlled(
                    room_id,
                    participant_id,
                    expected_sender,
                    target_user_id,
                    new_role,
                    control.ok_or_else(|| rejected("Room control is unavailable"))?,
                )
                .await?;
                return send(
                    &self.metrics,
                    expected_sender,
                    &ServerMessage::SocialResponse {
                        request_id: request_id.to_string(),
                        action: action.to_string(),
                        data: json!({"updated":true}),
                    },
                );
            }
        }
        let room_lock = self.get_room(room_id)?;
        let mut room = room_lock.write().await;
        room.ensure_live()?;
        let actor = Self::participant_for_sender(&room, participant_id, expected_sender)?;
        let actor_role = actor.role;
        let actor_name = actor.name.clone();
        let actor_authenticated = actor.authenticated;
        match command.social_budget() {
            SocialBudget::None => {}
            SocialBudget::ChatBroadcast => {
                if !room.reserve_chat_broadcast(std::time::Instant::now()) {
                    return Err(rejected("Room chat rate limit exceeded"));
                }
            }
            SocialBudget::AdminMutation => {
                if !room.reserve_admin_mutation(std::time::Instant::now()) {
                    return Err(rejected("Room requests are rate limited"));
                }
            }
        }
        let data = match command {
            ClientMessage::SetChatPreferences {
                allow_private_messages,
                ignored_participant_ids,
                ..
            } => {
                if ignored_participant_ids.len() > MAX_IGNORED
                    || ignored_participant_ids
                        .iter()
                        .any(|id| id.parse::<Uuid>().is_err() || id == participant_id)
                {
                    return Err(rejected(
                        "Choose at most 100 valid participant identities to ignore",
                    ));
                }
                let actor = room.participants.get_mut(participant_id).unwrap();
                actor.social.allow_private_messages = *allow_private_messages;
                actor.social.ignored = ignored_participant_ids.iter().cloned().collect();
                json!({"allowPrivateMessages":allow_private_messages,"ignoredParticipantIds":ignored_participant_ids})
            }
            ClientMessage::ChangeNickname { nickname, .. } => {
                let nickname = nickname.trim();
                if nickname.is_empty()
                    || nickname.len() > 64
                    || !crate::labels::is_plain(nickname)
                    || crate::labels::is_reserved_name(nickname)
                {
                    return Err(rejected(
                        "Nickname must be 1–64 bytes of plain text; \"You\" is taken",
                    ));
                }
                if room.name_in_use(nickname, Some(participant_id)) {
                    return Err(rejected("That name is already in use in this room"));
                }
                room.participants.get_mut(participant_id).unwrap().name = nickname.to_string();
                room.broadcast_all(&ServerMessage::NicknameChanged {
                    participant_id: participant_id.to_string(),
                    nickname: nickname.to_string(),
                });
                json!({"nickname":nickname})
            }
            ClientMessage::SetChatStyle { chat_style, .. } => {
                let chat_style = chat_style
                    .validated()
                    .ok_or_else(|| rejected("Choose a chat color from the palette"))?;
                if actor_authenticated && let Some(pool) = self.db_pool.clone() {
                    let user: Uuid = participant_id
                        .parse()
                        .map_err(|_| rejected("Invalid account"))?;
                    // The account keeps the look before anyone sees it.
                    drop(room);
                    tokio::time::timeout(
                        control::PERSISTENCE_TIMEOUT,
                        save_chat_style(&pool, user, &chat_style),
                    )
                    .await
                    .map_err(|_| rejected("Your chat look could not be saved"))?
                    .map_err(|_| rejected("Your chat look could not be saved"))?;
                    room = room_lock.write().await;
                    room.ensure_live()?;
                    Self::participant_for_sender(&room, participant_id, expected_sender)?;
                }
                room.participants
                    .get_mut(participant_id)
                    .ok_or_else(|| rejected("Participant not found"))?
                    .chat_style = chat_style.clone();
                room.broadcast_all(&ServerMessage::ChatStyleChanged {
                    participant_id: participant_id.to_string(),
                    chat_style: chat_style.clone(),
                });
                json!({"chatStyle": chat_style})
            }
            ClientMessage::ReactToMessage {
                message_id, emoji, ..
            } => {
                let reactions = react_to_message(&mut room, participant_id, message_id, emoji)?;
                json!({"messageId": message_id, "reactions": reactions})
            }
            ClientMessage::ListRoomBans { offset, .. } => {
                require_role(actor_role, roles::Role::Admin)?;
                let offset = page_offset(*offset)?;
                let mut bans: Vec<BanEntry> = if room.persisted {
                    let pool = self
                        .db_pool
                        .as_ref()
                        .ok_or_else(|| rejected("Ban service unavailable"))?;
                    drop(room);
                    let rows: Vec<BanRow> = tokio::time::timeout(
                        control::PERSISTENCE_TIMEOUT,
                        sqlx::query_as(
                            "SELECT s.id,u.display_name,s.user_id IS NOT NULL,s.reason,s.expires_at FROM room_states s LEFT JOIN users u ON u.id=s.user_id WHERE s.room_id=$1 AND s.state='banned' AND (s.expires_at IS NULL OR s.expires_at>now()) ORDER BY s.created_at DESC,s.id LIMIT 101 OFFSET $2")
                            .bind(room_id).bind(offset as i64).fetch_all(pool),
                    ).await.map_err(|_| rejected("Ban service timed out"))??;
                    room = room_lock.write().await;
                    room.ensure_live()?;
                    rows.into_iter()
                        .map(|(id, name, authenticated, reason, expiry)| BanEntry {
                            ban_id: id.to_string(),
                            display_name: name.unwrap_or_else(|| "Guest".to_string()),
                            authenticated,
                            reason,
                            expires_at: expiry.map(|e| e.to_rfc3339()),
                        })
                        .collect()
                } else {
                    room.social
                        .bans
                        .retain(|_, b| b.expiry.is_none_or(|e| e > std::time::Instant::now()));
                    let mut bans: Vec<_> =
                        room.social.bans.values().map(|b| b.entry.clone()).collect();
                    bans.sort_by(|a, b| a.ban_id.cmp(&b.ban_id));
                    bans.into_iter().skip(offset).take(PAGE_SIZE + 1).collect()
                };
                let has_more = bans.len() > PAGE_SIZE;
                bans.truncate(PAGE_SIZE);
                json!({"bans":bans,"hasMore":has_more})
            }
            ClientMessage::RemoveRoomBan { ban_id, .. } => {
                require_role(actor_role, roles::Role::Admin)?;
                let id: Uuid = ban_id.parse().map_err(|_| rejected("Invalid ban"))?;
                let (user_id, guest_ip, ids) = if room.persisted {
                    let pool = self
                        .db_pool
                        .as_ref()
                        .ok_or_else(|| rejected("Ban service unavailable"))?;
                    drop(room);
                    let actor_id = participant_id.to_string();
                    let actor_name = actor_name.clone();
                    let row: Option<(Option<Uuid>, Option<String>)> = self
                        .persist_room(room_id, &room_lock, async {
                            let mut transaction = pool.begin().await?;
                            let row: Option<(Option<Uuid>, Option<String>, Option<String>)> = sqlx::query_as("DELETE FROM room_states s WHERE s.room_id=$1 AND s.id=$2 AND s.state='banned' RETURNING s.user_id,host(s.ip_address),(SELECT u.display_name FROM users u WHERE u.id=s.user_id)")
                                .bind(room_id).bind(id).fetch_optional(&mut *transaction).await?;
                            let Some((uid, ip, name)) = row else {
                                return Ok(None);
                            };
                            let target_id = uid.map_or_else(|| id.to_string(), |uid| uid.to_string());
                            let event = ModerationEvent::new(
                                ModerationAction::Unban,
                                Actor { id: &actor_id, name: &actor_name },
                                Target {
                                    id: &target_id,
                                    name: name.as_deref().unwrap_or(if uid.is_some() { "Removed account" } else { "Guest" }),
                                    authenticated: uid.is_some(),
                                    ip: ip.as_deref().and_then(|ip| ip.parse().ok()),
                                },
                                None,
                                None,
                                None,
                            );
                            record_event(&mut transaction, room_id, &event, MAX_MODERATION_EVENTS).await?;
                            transaction.commit().await?;
                            Ok(Some((uid, ip)))
                        })
                        .await?;
                    room = room_lock.write().await;
                    room.ensure_live()?;
                    let (uid, ip) = row.ok_or_else(|| rejected("Ban not found"))?;
                    (
                        uid.map(|u| u.to_string()),
                        if uid.is_none() {
                            ip.and_then(|ip| ip.parse::<IpAddr>().ok())
                        } else {
                            None
                        },
                        Vec::new(),
                    )
                } else {
                    let ban = room
                        .social
                        .bans
                        .remove(ban_id)
                        .ok_or_else(|| rejected("Ban not found"))?;
                    let target_id = ban
                        .entry
                        .authenticated
                        .then(|| ban.participant_ids.first().cloned())
                        .flatten();
                    room.social.record_moderation_event(ModerationEvent::new(
                        ModerationAction::Unban,
                        Actor {
                            id: participant_id,
                            name: &actor_name,
                        },
                        Target {
                            id: target_id.as_deref().unwrap_or(ban_id),
                            name: &ban.entry.display_name,
                            authenticated: ban.entry.authenticated,
                            ip: ban.guest_ip,
                        },
                        None,
                        None,
                        None,
                    ));
                    (target_id, ban.guest_ip, ban.participant_ids)
                };
                if let Some(uid) = &user_id {
                    room.banned_participants.remove(uid);
                }
                if let Some(ip) = guest_ip {
                    room.banned_guest_ips.remove(&ip);
                }
                let mut affected = ids;
                room.social.bans.retain(|_, ban| {
                    let matches = user_id
                        .as_ref()
                        .is_some_and(|uid| ban.participant_ids.contains(uid))
                        || guest_ip.is_some_and(|ip| ban.guest_ip == Some(ip));
                    if matches {
                        affected.extend(ban.participant_ids.iter().cloned());
                    }
                    !matches
                });
                for id in affected {
                    room.banned_participants.remove(&id);
                    room.banned_guest_participants.remove(&id);
                }
                room.policy_revision = room.policy_revision.wrapping_add(1);
                json!({"removed":true})
            }
            ClientMessage::ListRoomMembers { offset, .. } => {
                require_role(actor_role, roles::Role::Moderator)?;
                let offset = page_offset(*offset)?;
                let mut members: Vec<MemberEntry> = if room.persisted {
                    let pool = self
                        .db_pool
                        .as_ref()
                        .ok_or_else(|| rejected("Member service unavailable"))?;
                    drop(room);
                    let rows: Vec<(Uuid, String, i16)> = tokio::time::timeout(
                        control::PERSISTENCE_TIMEOUT,
                        sqlx::query_as("WITH roster AS (SELECT owner_id AS user_id,5::smallint AS role FROM rooms WHERE id=$1 UNION ALL SELECT rr.user_id,(rr.role+1)::smallint FROM room_roles rr JOIN rooms r ON r.id=rr.room_id WHERE rr.room_id=$1 AND rr.user_id<>r.owner_id) SELECT u.id,u.display_name,roster.role FROM roster JOIN users u ON u.id=roster.user_id ORDER BY u.display_name,u.id LIMIT 101 OFFSET $2")
                            .bind(room_id).bind(offset as i64).fetch_all(pool),
                    ).await.map_err(|_| rejected("Member service timed out"))??;
                    room = room_lock.write().await;
                    room.ensure_live()?;
                    rows.into_iter()
                        .map(|(id, name, role)| {
                            let id = id.to_string();
                            MemberEntry {
                                online: room.participants.contains_key(&id),
                                user_id: id,
                                display_name: name,
                                role: roles::Role::from_u8(role as u8)
                                    .unwrap_or(roles::Role::User)
                                    .name()
                                    .to_string(),
                                authenticated: true,
                            }
                        })
                        .collect()
                } else {
                    let mut members: Vec<_> = room
                        .participants
                        .values()
                        .map(|p| MemberEntry {
                            user_id: p.id.clone(),
                            display_name: p.name.clone(),
                            role: p.role.name().to_string(),
                            online: true,
                            authenticated: p.authenticated,
                        })
                        .collect();
                    members.sort_by(|a, b| {
                        a.display_name
                            .cmp(&b.display_name)
                            .then(a.user_id.cmp(&b.user_id))
                    });
                    members
                        .into_iter()
                        .skip(offset)
                        .take(PAGE_SIZE + 1)
                        .collect()
                };
                let has_more = members.len() > PAGE_SIZE;
                members.truncate(PAGE_SIZE);
                json!({"members":members,"hasMore":has_more})
            }
            ClientMessage::SetMemberRole {
                target_user_id,
                role,
                ..
            } => {
                require_role(actor_role, roles::Role::Moderator)?;
                if !room.persisted || !actor_authenticated {
                    return Err(rejected("Offline roles require a persistent room"));
                }
                let target: Uuid = target_user_id
                    .parse()
                    .map_err(|_| rejected("Invalid member"))?;
                let setter: Uuid = participant_id.parse()?;
                let new_role =
                    roles::Role::from_u8(*role).ok_or_else(|| rejected("Invalid role"))?;
                if new_role < roles::Role::User {
                    return Err(rejected(
                        "Registered accounts require at least the user role",
                    ));
                }
                let pool = self
                    .db_pool
                    .as_ref()
                    .ok_or_else(|| rejected("Member service unavailable"))?;
                drop(room);
                // Absent targets must already belong to this room's roster; no account enumeration.
                let current: Option<i16> = tokio::time::timeout(
                    control::PERSISTENCE_TIMEOUT,
                    sqlx::query_scalar("SELECT CASE WHEN $2=owner_id THEN 5 ELSE (SELECT role+1 FROM room_roles WHERE room_id=$1 AND user_id=$2) END::smallint FROM rooms WHERE id=$1 AND ($2=owner_id OR EXISTS(SELECT 1 FROM room_roles WHERE room_id=$1 AND user_id=$2))")
                        .bind(room_id).bind(target).fetch_optional(pool),
                ).await.map_err(|_| rejected("Member service timed out"))??;
                let current = current
                    .and_then(|r| roles::Role::from_u8(r as u8))
                    .ok_or_else(|| rejected("Room member not found"))?;
                if !actor_role.can_set_role(current, new_role) {
                    return Err(rejected("Insufficient role permissions"));
                }
                self.persist_room(
                    room_id,
                    &room_lock,
                    roles::set_role(pool, room_id, &target, new_role, &setter),
                )
                .await?;
                room = room_lock.write().await;
                room.ensure_live()?;
                room.policy_revision = room.policy_revision.wrapping_add(1);
                json!({"updated":true})
            }
            ClientMessage::ReportParticipant {
                target_participant_id,
                reason,
                ..
            } => {
                let reason = reason.trim();
                if reason.is_empty() || reason.len() > 1024 || reason.chars().any(char::is_control)
                {
                    return Err(rejected("Report reason must be 1–1024 bytes"));
                }
                if target_participant_id == participant_id {
                    return Err(rejected("Choose another participant"));
                }
                let target = room
                    .participants
                    .get(target_participant_id)
                    .ok_or_else(|| rejected("Participant is no longer in this room"))?;
                let target_name = target.name.clone();
                let reporter = reporter_key(room.participants.get(participant_id).unwrap());
                if room
                    .social
                    .report_cooldown_active(&reporter, std::time::Instant::now())
                {
                    return Err(rejected("Please wait before submitting another report"));
                }
                let report_id = Uuid::new_v4();
                let report = ReportEntry {
                    report_id: report_id.to_string(),
                    reporter_id: participant_id.to_string(),
                    reporter_name: actor_name,
                    target_participant_id: target_participant_id.clone(),
                    target_name,
                    reason: reason.to_string(),
                    status: "open".to_string(),
                    created_at: chrono::Utc::now().to_rfc3339(),
                    resolved_at: None,
                    outcome: None,
                };
                if room.persisted {
                    let pool = self
                        .db_pool
                        .as_ref()
                        .ok_or_else(|| rejected("Report service unavailable"))?;
                    drop(room);
                    let inserted = self.persist_room(room_id, &room_lock, async {
                        let mut transaction = pool.begin().await?;
                        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,71339))")
                            .bind(room_id)
                            .execute(&mut *transaction)
                            .await?;
                        let count: i64 =
                            sqlx::query_scalar("SELECT COUNT(*) FROM room_reports WHERE room_id=$1")
                                .bind(room_id)
                                .fetch_one(&mut *transaction)
                                .await?;
                        if count >= MAX_REPORTS as i64 {
                            let removed=sqlx::query("DELETE FROM room_reports WHERE id IN (SELECT id FROM room_reports WHERE room_id=$1 AND status<>'open' ORDER BY created_at,id LIMIT 1)").bind(room_id).execute(&mut *transaction).await?.rows_affected();
                            if removed == 0 {
                                return Ok(false);
                            }
                        }
                        sqlx::query("INSERT INTO room_reports(id,room_id,reporter_id,reporter_name,target_participant_id,target_name,reason) VALUES($1,$2,$3,$4,$5,$6,$7)")
                            .bind(report_id).bind(room_id).bind(participant_id).bind(&report.reporter_name).bind(target_participant_id).bind(&report.target_name).bind(reason).execute(&mut *transaction).await?;
                        transaction.commit().await?;
                        Ok(true)
                    })
                    .await?;
                    if !inserted {
                        return Err(rejected(
                            "Room report capacity reached; contact a moderator",
                        ));
                    }
                    room = room_lock.write().await;
                    room.ensure_live()?;
                } else {
                    if room.social.reports.len() >= MAX_REPORTS {
                        if let Some(index) =
                            room.social.reports.iter().position(|r| r.status != "open")
                        {
                            room.social.reports.remove(index);
                        } else {
                            return Err(rejected(
                                "Room report capacity reached; contact a moderator",
                            ));
                        }
                    }
                    room.social.reports.push_back(report.clone());
                }
                room.social.note_report(reporter, std::time::Instant::now());
                // Only the submitting participant receives the report ID here.
                json!({"reportId":report.report_id,"status":"open"})
            }
            ClientMessage::ListRoomReports { offset, .. } => {
                require_role(actor_role, roles::Role::Moderator)?;
                let offset = page_offset(*offset)?;
                let mut reports: Vec<ReportEntry> = if room.persisted {
                    let pool = self
                        .db_pool
                        .as_ref()
                        .ok_or_else(|| rejected("Report service unavailable"))?;
                    drop(room);
                    let rows: Vec<ReportRow> = tokio::time::timeout(
                        control::PERSISTENCE_TIMEOUT,
                        sqlx::query_as("SELECT r.id,r.reporter_id,r.reporter_name,r.target_participant_id,r.target_name,r.reason,r.status,r.created_at,r.resolved_at,o.action,o.created_at FROM room_reports r LEFT JOIN LATERAL (SELECT e.action,e.created_at FROM moderation_events e WHERE e.report_id=r.id ORDER BY e.created_at DESC,e.id LIMIT 1) o ON true WHERE r.room_id=$1 ORDER BY r.created_at DESC,r.id LIMIT 101 OFFSET $2")
                            .bind(room_id).bind(offset as i64).fetch_all(pool),
                    ).await.map_err(|_| rejected("Report service timed out"))??;
                    room = room_lock.write().await;
                    room.ensure_live()?;
                    rows.into_iter()
                        .map(
                            |(
                                id,
                                rid,
                                rname,
                                tid,
                                tname,
                                reason,
                                status,
                                created,
                                resolved,
                                outcome_action,
                                outcome_at,
                            )| {
                                ReportEntry {
                                    report_id: id.to_string(),
                                    reporter_id: rid,
                                    reporter_name: rname,
                                    target_participant_id: tid,
                                    target_name: tname,
                                    reason,
                                    status,
                                    created_at: created.to_rfc3339(),
                                    resolved_at: resolved.map(|r| r.to_rfc3339()),
                                    outcome: outcome_action.zip(outcome_at).map(|(action, at)| {
                                        ReportOutcome {
                                            action,
                                            created_at: at.to_rfc3339(),
                                        }
                                    }),
                                }
                            },
                        )
                        .collect()
                } else {
                    room.social
                        .reports
                        .iter()
                        .rev()
                        .skip(offset)
                        .take(PAGE_SIZE + 1)
                        .map(|report| ReportEntry {
                            outcome: room.social.report_outcome(&report.report_id),
                            ..report.clone()
                        })
                        .collect()
                };
                let has_more = reports.len() > PAGE_SIZE;
                reports.truncate(PAGE_SIZE);
                json!({"reports":reports,"hasMore":has_more})
            }
            ClientMessage::ResolveRoomReport {
                report_id, status, ..
            } => {
                require_role(actor_role, roles::Role::Moderator)?;
                if !matches!(status.as_str(), "resolved" | "dismissed") {
                    return Err(rejected("Choose resolved or dismissed"));
                }
                let id: Uuid = report_id.parse().map_err(|_| rejected("Invalid report"))?;
                let action = if status == "resolved" {
                    ModerationAction::ReportResolved
                } else {
                    ModerationAction::ReportDismissed
                };
                if room.persisted {
                    let pool = self
                        .db_pool
                        .as_ref()
                        .ok_or_else(|| rejected("Report service unavailable"))?;
                    drop(room);
                    let actor_id = participant_id.to_string();
                    let actor_name = actor_name.clone();
                    let resolved = self
                        .persist_room(room_id, &room_lock, async {
                            let mut transaction = pool.begin().await?;
                            let target: Option<(String, String, bool)> = sqlx::query_as("UPDATE room_reports r SET status=$3,resolved_at=now(),resolved_by=$4 WHERE r.room_id=$1 AND r.id=$2 AND r.status='open' RETURNING r.target_participant_id,r.target_name,EXISTS(SELECT 1 FROM users u WHERE u.id::text=r.target_participant_id)")
                                .bind(room_id).bind(id).bind(status).bind(&actor_id).fetch_optional(&mut *transaction).await?;
                            let Some((target_id, target_name, target_authenticated)) = target else {
                                return Ok(false);
                            };
                            let event = ModerationEvent::new(
                                action,
                                Actor { id: &actor_id, name: &actor_name },
                                Target { id: &target_id, name: &target_name, authenticated: target_authenticated, ip: None },
                                None,
                                None,
                                Some(id),
                            );
                            record_event(&mut transaction, room_id, &event, MAX_MODERATION_EVENTS).await?;
                            transaction.commit().await?;
                            Ok(true)
                        })
                        .await?;
                    room = room_lock.write().await;
                    room.ensure_live()?;
                    if !resolved {
                        return Err(rejected("Open report not found"));
                    }
                } else {
                    let report = room
                        .social
                        .reports
                        .iter_mut()
                        .find(|r| r.report_id == *report_id && r.status == "open")
                        .ok_or_else(|| rejected("Open report not found"))?;
                    report.status = status.clone();
                    report.resolved_at = Some(chrono::Utc::now().to_rfc3339());
                    let target_id = report.target_participant_id.clone();
                    let target_name = report.target_name.clone();
                    let target_authenticated = room
                        .participants
                        .get(&target_id)
                        .is_some_and(|participant| participant.authenticated);
                    room.social.record_moderation_event(ModerationEvent::new(
                        action,
                        Actor {
                            id: participant_id,
                            name: &actor_name,
                        },
                        Target {
                            id: &target_id,
                            name: &target_name,
                            authenticated: target_authenticated,
                            ip: None,
                        },
                        None,
                        None,
                        Some(id),
                    ));
                }
                json!({"reportId":report_id,"status":status})
            }
            ClientMessage::ListModerationEvents { offset, .. } => {
                require_role(actor_role, roles::Role::Moderator)?;
                let offset = page_offset(*offset)?;
                let owner = actor_role >= roles::Role::Owner;
                let mut events: Vec<ModerationEvent> = if room.persisted {
                    let pool = self
                        .db_pool
                        .as_ref()
                        .ok_or_else(|| rejected("History service unavailable"))?;
                    drop(room);
                    let rows = tokio::time::timeout(
                        control::PERSISTENCE_TIMEOUT,
                        moderation::list_events(
                            pool,
                            room_id,
                            offset as i64,
                            (PAGE_SIZE + 1) as i64,
                        ),
                    )
                    .await
                    .map_err(|_| rejected("History service timed out"))??;
                    room = room_lock.write().await;
                    room.ensure_live()?;
                    rows
                } else {
                    room.social
                        .moderation_events
                        .iter()
                        .rev()
                        .skip(offset)
                        .take(PAGE_SIZE + 1)
                        .cloned()
                        .collect()
                };
                let has_more = events.len() > PAGE_SIZE;
                events.truncate(PAGE_SIZE);
                // Only the owner reads a target's address; moderators never do.
                let events: Vec<Value> = events.iter().map(|event| event.to_json(owner)).collect();
                json!({"events":events,"hasMore":has_more})
            }
            _ => return Err(rejected("Unknown request")),
        };
        drop(room);
        drop(control);
        send(
            &self.metrics,
            expected_sender,
            &ServerMessage::SocialResponse {
                request_id: request_id.to_string(),
                action: action.to_string(),
                data,
            },
        )
    }
}
