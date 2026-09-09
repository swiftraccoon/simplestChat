//! Room-session chat and owner/moderator community tools.
//! Private text is retained only in bounded memory, never in reports or logs.
use super::*;
use crate::signaling::protocol::{ChatEntry, ClientMessage};
use serde::Serialize;
use serde_json::{Value, json};
use uuid::Uuid;

const HISTORY_MESSAGES: usize = 300;
const HISTORY_BYTES: usize = 256 * 1024;
const MAX_IGNORED: usize = 100;
const MAX_REPORTS: usize = 500;
const MAX_RUNTIME_BANS: usize = 2000;
const PAGE_SIZE: usize = 100;

#[derive(Clone)]
pub(crate) struct ParticipantSocial {
    joined_sequence: u64,
    allow_private_messages: bool,
    ignored: HashSet<String>,
    last_report: Option<std::time::Instant>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn participant(name: &str) -> (Participant, mpsc::Receiver<Arc<String>>) {
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
            },
            receiver,
        )
    }

    fn fixture() -> (
        Room,
        Participant,
        Participant,
        Participant,
        Vec<mpsc::Receiver<Arc<String>>>,
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
            bob.sender.try_send(Arc::new("queued".into())).unwrap();
        }
        assert!(chat(&mut room, &alice, "busy", Some(&bob.id)).is_err());
        assert!(room.social.history.is_empty());
        assert!(receivers[0].try_recv().is_err());
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
            .participants
            .get_mut(&bob.id)
            .unwrap()
            .social
            .last_report = None;
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
        manager
            .handle_social_request(
                &room_id,
                &owner.id,
                &owner.sender,
                &ClientMessage::GetRoomSnapshot {
                    request_id: "snapshot".into(),
                },
            )
            .await
            .unwrap();
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
        manager.shutdown().await;
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
            joined_sequence,
            allow_private_messages: true,
            ignored: HashSet::new(),
            last_report: None,
        }
    }
}

struct HistoryEntry {
    sequence: u64,
    sender_session: Uuid,
    recipient_session: Option<Uuid>,
    message: ChatEntry,
}

#[derive(Default)]
pub(crate) struct RoomSocial {
    pub(crate) next_sequence: u64,
    history: VecDeque<HistoryEntry>,
    history_bytes: usize,
    bans: HashMap<String, RuntimeBan>,
    reports: VecDeque<ReportEntry>,
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
fn rejected(message: &str) -> anyhow::Error {
    SocialFailure(message.to_string()).into()
}

pub(crate) fn valid_correlation_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

impl ClientMessage {
    pub(crate) fn social_request(&self) -> Option<(&str, &'static str)> {
        match self {
            Self::SetChatPreferences { request_id, .. } => Some((request_id, "setChatPreferences")),
            Self::ChangeNickname { request_id, .. } => Some((request_id, "changeNickname")),
            Self::GetRoomSnapshot { request_id } => Some((request_id, "getRoomSnapshot")),
            Self::ListRoomBans { request_id, .. } => Some((request_id, "listRoomBans")),
            Self::RemoveRoomBan { request_id, .. } => Some((request_id, "removeRoomBan")),
            Self::ListRoomMembers { request_id, .. } => Some((request_id, "listRoomMembers")),
            Self::SetMemberRole { request_id, .. } => Some((request_id, "setMemberRole")),
            Self::ReportParticipant { request_id, .. } => Some((request_id, "reportParticipant")),
            Self::ListRoomReports { request_id, .. } => Some((request_id, "listRoomReports")),
            Self::ResolveRoomReport { request_id, .. } => Some((request_id, "resolveRoomReport")),
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

fn send(sender: &mpsc::Sender<Arc<String>>, message: &ServerMessage) -> Result<()> {
    sender
        .try_send(Arc::new(serde_json::to_string(message)?))
        .map_err(|_| rejected("Connection is busy; please retry"))
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
fn can_private_message(sender: &Participant, recipient: &Participant) -> bool {
    sender.id != recipient.id
        && recipient.social.allow_private_messages
        && !recipient.social.ignored.contains(&sender.id)
        && !sender.social.ignored.contains(&recipient.id)
}
impl RoomSocial {
    pub(crate) fn forget_user_ban(&mut self, participant_id: &str) {
        self.bans.retain(|_, ban| {
            !ban.entry.authenticated || !ban.participant_ids.iter().any(|id| id == participant_id)
        });
    }
    fn remember(
        &mut self,
        sender_session: Uuid,
        recipient_session: Option<Uuid>,
        message: ChatEntry,
    ) {
        // Names and identifiers also count, keeping retention bounded even with short text.
        let size = serde_json::to_vec(&message).map_or(HISTORY_BYTES, |v| v.len());
        self.history_bytes += size;
        self.history.push_back(HistoryEntry {
            sequence: self.next_sequence,
            sender_session,
            recipient_session,
            message,
        });
        self.next_sequence = self.next_sequence.saturating_add(1);
        while self.history.len() > HISTORY_MESSAGES || self.history_bytes > HISTORY_BYTES {
            if let Some(old) = self.history.pop_front() {
                self.history_bytes = self.history_bytes.saturating_sub(
                    serde_json::to_vec(&old.message).map_or(HISTORY_BYTES, |v| v.len()),
                );
            } else {
                break;
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

impl RoomManager {
    pub async fn send_social_chat(
        &self,
        room_id: &str,
        sender_id: &str,
        expected_sender: &mpsc::Sender<Arc<String>>,
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
        expected_sender: &mpsc::Sender<Arc<String>>,
        content: String,
        client_message_id: Option<String>,
        recipient_id: Option<&str>,
    ) -> Result<()> {
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
        room.ensure_live()?;
        let sender = Self::participant_for_sender(room, sender_id, expected_sender)?;
        let moderated = room.settings.as_ref().is_some_and(|s| s.moderated);
        if room.settings.as_ref().is_some_and(|s| !s.allow_chat)
            || !moderation::can_chat(&sender.punitive, sender.role, moderated)
        {
            return Err(rejected("You are not allowed to chat"));
        }
        let sender_session = sender.media_session_id;
        let sender_name = sender.name.clone();
        if let Some(existing) = room.social.history.iter().find(|entry| {
            entry.sender_session == sender_session
                && entry.message.client_message_id == client_message_id
        }) {
            if existing.message.content != content
                || existing.message.recipient_id.as_deref() != recipient_id
            {
                return Err(rejected("Message ID was already used"));
            }
            return send(
                expected_sender,
                &ServerMessage::MessageAck {
                    client_message_id,
                    message: existing.message.clone(),
                },
            );
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
        if !room.reserve_chat_broadcast(std::time::Instant::now()) {
            return Err(rejected("Room chat rate limit exceeded"));
        }
        let message = ChatEntry {
            message_id: Uuid::new_v4().to_string(),
            client_message_id: client_message_id.clone(),
            participant_id: sender_id.to_string(),
            participant_name: sender_name,
            recipient_id: recipient_id.map(String::from),
            recipient_name: recipient.as_ref().map(|p| p.0.clone()),
            content,
            sent_at: chrono::Utc::now().to_rfc3339(),
        };
        if let Some((_, _, recipient_sender)) = &recipient {
            // Only acknowledged when the recipient's live connection accepted delivery.
            send(
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
            };
            let json = Arc::new(serde_json::to_string(&event)?);
            for participant in room.participants.values() {
                if participant.id != sender_id && !participant.social.ignored.contains(sender_id) {
                    let _ = participant.sender.try_send(json.clone());
                }
            }
        }
        room.social.remember(
            sender_session,
            recipient.as_ref().map(|r| r.1),
            message.clone(),
        );
        send(
            expected_sender,
            &ServerMessage::MessageAck {
                client_message_id,
                message,
            },
        )
    }

    pub async fn handle_social_request(
        &self,
        room_id: &str,
        participant_id: &str,
        expected_sender: &mpsc::Sender<Arc<String>>,
        command: &ClientMessage,
    ) -> Result<()> {
        let (request_id, action) = command
            .social_request()
            .ok_or_else(|| rejected("Unknown request"))?;
        if !valid_correlation_id(request_id) {
            return Err(rejected("Invalid request ID"));
        }
        // Existing live role mutation handles producer revocation and all existing policies.
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
                self.set_participant_role(
                    room_id,
                    participant_id,
                    expected_sender,
                    target_user_id,
                    new_role,
                )
                .await?;
                return send(
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
        if !room.reserve_admin_mutation(std::time::Instant::now()) {
            return Err(rejected("Room requests are rate limited"));
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
                    || nickname.chars().any(char::is_control)
                {
                    return Err(rejected(
                        "Nickname must be 1–64 bytes without control characters",
                    ));
                }
                room.participants.get_mut(participant_id).unwrap().name = nickname.to_string();
                room.broadcast_all(&ServerMessage::NicknameChanged {
                    participant_id: participant_id.to_string(),
                    nickname: nickname.to_string(),
                });
                json!({"nickname":nickname})
            }
            ClientMessage::GetRoomSnapshot { .. } => {
                let actor = room.participants.get(participant_id).unwrap();
                let participants: Vec<ParticipantInfo> = room
                    .participants
                    .values()
                    .filter(|p| p.id != participant_id)
                    .map(|p| ParticipantInfo {
                        id: p.id.clone(),
                        name: p.name.clone(),
                        role: p.role.name().to_string(),
                        authenticated: p.authenticated,
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
                    .collect();
                let messages: Vec<&ChatEntry> = room
                    .social
                    .history
                    .iter()
                    .filter(|entry| visible(entry, actor))
                    .map(|entry| &entry.message)
                    .collect();
                let lobby: Vec<Value> = if actor_role >= roles::Role::Moderator {
                    room.lobby.values().map(|p| json!({"participantId":p.participant_id,"displayName":p.name,"authenticated":p.authenticated})).collect()
                } else {
                    Vec::new()
                };
                let paused_producer_ids: Vec<String> = room
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
                    .collect();
                let can_chat = !room.settings.as_ref().is_some_and(|s| !s.allow_chat)
                    && moderation::can_chat(
                        &actor.punitive,
                        actor.role,
                        room.settings.as_ref().is_some_and(|s| s.moderated),
                    );
                json!({"participants":participants,"messages":messages,"lobby":lobby,"yourRole":actor_role.name(),
                    "roomSettings":room.settings,"nickname":actor.name,"allowPrivateMessages":actor.social.allow_private_messages,"ignoredParticipantIds":actor.social.ignored,
                    "pausedProducerIds":paused_producer_ids,"textMuted":actor.punitive.text_muted,"camBanned":actor.punitive.cam_banned,"canChat":can_chat,
                    "localProducerIds":actor.producers.keys().collect::<Vec<_>>(),
                    "canBroadcast":Self::participant_can_produce(&room,actor,MediaKind::Audio,"microphone")})
            }
            ClientMessage::ListRoomBans { offset, .. } => {
                require_role(actor_role, roles::Role::Admin)?;
                let offset = page_offset(*offset)?;
                let mut bans: Vec<BanEntry> = if room.persisted {
                    let pool = self
                        .db_pool
                        .as_ref()
                        .ok_or_else(|| rejected("Ban service unavailable"))?;
                    let rows:Vec<(Uuid,Option<String>,bool,Option<String>,Option<chrono::DateTime<chrono::Utc>>)>=sqlx::query_as(
                        "SELECT s.id,u.display_name,s.user_id IS NOT NULL,s.reason,s.expires_at FROM room_states s LEFT JOIN users u ON u.id=s.user_id WHERE s.room_id=$1 AND s.state='banned' AND (s.expires_at IS NULL OR s.expires_at>now()) ORDER BY s.created_at DESC,s.id LIMIT 101 OFFSET $2")
                        .bind(room_id).bind(offset as i64).fetch_all(pool).await?;
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
                    let row:Option<(Option<Uuid>,Option<String>)>=sqlx::query_as("DELETE FROM room_states WHERE room_id=$1 AND id=$2 AND state='banned' RETURNING user_id,host(ip_address)")
                        .bind(room_id).bind(id).fetch_optional(pool).await?;
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
                    (
                        ban.entry
                            .authenticated
                            .then(|| ban.participant_ids.first().cloned())
                            .flatten(),
                        ban.guest_ip,
                        ban.participant_ids,
                    )
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
                    let rows:Vec<(Uuid,String,i16)>=sqlx::query_as("WITH roster AS (SELECT owner_id AS user_id,5::smallint AS role FROM rooms WHERE id=$1 UNION ALL SELECT rr.user_id,(rr.role+1)::smallint FROM room_roles rr JOIN rooms r ON r.id=rr.room_id WHERE rr.room_id=$1 AND rr.user_id<>r.owner_id) SELECT u.id,u.display_name,roster.role FROM roster JOIN users u ON u.id=roster.user_id ORDER BY u.display_name,u.id LIMIT 101 OFFSET $2")
                        .bind(room_id).bind(offset as i64).fetch_all(pool).await?;
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
                // Absent targets must already belong to this room's roster; no account enumeration.
                let current:Option<i16>=sqlx::query_scalar("SELECT CASE WHEN $2=owner_id THEN 5 ELSE (SELECT role+1 FROM room_roles WHERE room_id=$1 AND user_id=$2) END::smallint FROM rooms WHERE id=$1 AND ($2=owner_id OR EXISTS(SELECT 1 FROM room_roles WHERE room_id=$1 AND user_id=$2))")
                    .bind(room_id).bind(target).fetch_optional(pool).await?;
                let current = current
                    .and_then(|r| roles::Role::from_u8(r as u8))
                    .ok_or_else(|| rejected("Room member not found"))?;
                if !actor_role.can_set_role(current, new_role) {
                    return Err(rejected("Insufficient role permissions"));
                }
                roles::set_role(pool, room_id, &target, new_role, &setter).await?;
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
                let actor = room.participants.get(participant_id).unwrap();
                if actor
                    .social
                    .last_report
                    .is_some_and(|at| at.elapsed() < std::time::Duration::from_secs(30))
                {
                    return Err(rejected("Please wait before submitting another report"));
                }
                let report = ReportEntry {
                    report_id: Uuid::new_v4().to_string(),
                    reporter_id: participant_id.to_string(),
                    reporter_name: actor_name,
                    target_participant_id: target_participant_id.clone(),
                    target_name,
                    reason: reason.to_string(),
                    status: "open".to_string(),
                    created_at: chrono::Utc::now().to_rfc3339(),
                    resolved_at: None,
                };
                if room.persisted {
                    let pool = self
                        .db_pool
                        .as_ref()
                        .ok_or_else(|| rejected("Report service unavailable"))?;
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
                            return Err(rejected(
                                "Room report capacity reached; contact a moderator",
                            ));
                        }
                    }
                    sqlx::query("INSERT INTO room_reports(id,room_id,reporter_id,reporter_name,target_participant_id,target_name,reason) VALUES($1,$2,$3,$4,$5,$6,$7)")
                        .bind(report.report_id.parse::<Uuid>()?).bind(room_id).bind(participant_id).bind(&report.reporter_name).bind(target_participant_id).bind(&report.target_name).bind(reason).execute(&mut *transaction).await?;
                    transaction.commit().await?;
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
                room.participants
                    .get_mut(participant_id)
                    .unwrap()
                    .social
                    .last_report = Some(std::time::Instant::now());
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
                    let rows:Vec<(Uuid,String,String,String,String,String,String,chrono::DateTime<chrono::Utc>,Option<chrono::DateTime<chrono::Utc>>)>=sqlx::query_as("SELECT id,reporter_id,reporter_name,target_participant_id,target_name,reason,status,created_at,resolved_at FROM room_reports WHERE room_id=$1 ORDER BY created_at DESC,id LIMIT 101 OFFSET $2")
                        .bind(room_id).bind(offset as i64).fetch_all(pool).await?;
                    rows.into_iter()
                        .map(
                            |(id, rid, rname, tid, tname, reason, status, created, resolved)| {
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
                        .cloned()
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
                if room.persisted {
                    let pool = self
                        .db_pool
                        .as_ref()
                        .ok_or_else(|| rejected("Report service unavailable"))?;
                    let affected=sqlx::query("UPDATE room_reports SET status=$3,resolved_at=now(),resolved_by=$4 WHERE room_id=$1 AND id=$2 AND status='open'")
                        .bind(room_id).bind(id).bind(status).bind(participant_id).execute(pool).await?.rows_affected();
                    if affected == 0 {
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
                }
                json!({"reportId":report_id,"status":status})
            }
            _ => return Err(rejected("Unknown request")),
        };
        send(
            expected_sender,
            &ServerMessage::SocialResponse {
                request_id: request_id.to_string(),
                action: action.to_string(),
                data,
            },
        )
    }
}
