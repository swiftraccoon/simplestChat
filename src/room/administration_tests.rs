//! Runs only against the explicitly disposable, migrated hosted test database.
use super::*;
use crate::ids::fixtures::rid;
use crate::{media::MediaConfig, metrics::ServerMetrics};
use futures_util::FutureExt;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use std::{collections::HashMap, panic::AssertUnwindSafe, str::FromStr, time::Duration};
use tokio::sync::mpsc;

fn claims(user: Uuid, session: Uuid) -> Claims {
    Claims {
        sub: user.to_string(),
        name: "Administration fixture".into(),
        iss: "simplestchat".into(),
        aud: "simplestchat".into(),
        exp: (Utc::now().timestamp() + 3600) as usize,
        auth_version: 0,
        sid: session,
    }
}

async fn await_owned_lock_wait(pool: &sqlx::PgPool, blocker: i32) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let waiting: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT FROM pg_stat_activity WHERE $1=ANY(pg_blocking_pids(pid)))",
            )
            .bind(blocker)
            .fetch_one(pool)
            .await
            .unwrap();
            if waiting {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("request must wait on this fixture's locked row");
}

#[tokio::test]
#[ignore = "requires migrated disposable TEST_DATABASE_URL and mediasoup worker"]
async fn account_administration_controls_dormant_and_live_rooms_without_membership() {
    assert_eq!(
        std::env::var("DISPOSABLE_TEST_DATABASE").as_deref(),
        Ok("1")
    );
    let options =
        PgConnectOptions::from_str(&std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL"))
            .unwrap();
    assert!(matches!(options.get_host(), "127.0.0.1" | "::1"));
    assert!(options.get_socket().is_none());
    assert!(
        options
            .get_database()
            .is_some_and(|name| name.ends_with("_test"))
    );
    let options = options.options([("statement_timeout", "5s"), ("lock_timeout", "5s")]);
    let pool = PgPoolOptions::new()
        .max_connections(6)
        .acquire_timeout(Duration::from_secs(5))
        .connect_with(options)
        .await
        .unwrap();
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
    let users = [
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
    ];
    let owner = users[0];
    let moderator = users[1];
    let peer = users[2];
    let outsider = users[3];
    let sessions = [
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
    ];
    let room_id = rid(&format!("http-admin-{}", Uuid::new_v4()));
    let other_room = rid(&format!("http-admin-other-{}", Uuid::new_v4()));
    let proof = async {
        for (index, user) in users.iter().enumerate() {
            sqlx::query("INSERT INTO users(id,email,display_name) VALUES($1,$2,$3)")
                .bind(user)
                .bind(format!("{user}@admin.invalid"))
                .bind(format!("Administrator {index}"))
                .execute(&pool)
                .await
                .unwrap();
            sqlx::query("INSERT INTO sessions(id,user_id,refresh_token_hash,refresh_token_family_hash,expires_at) VALUES($1,$2,$3,$3,now()+interval '1 day')")
                .bind(sessions[index]).bind(user).bind(crate::auth::session::hash_token(&Uuid::new_v4().to_string())).execute(&pool).await.unwrap();
        }
        for id in [&room_id, &other_room] {
            sqlx::query("INSERT INTO rooms(id,owner_id,display_name) VALUES($1,$2,$1)")
                .bind(id)
                .bind(owner)
                .execute(&pool)
                .await
                .unwrap();
        }
        roles::set_role(&pool, &room_id, &moderator, roles::Role::Moderator, &owner)
            .await
            .unwrap();
        roles::set_role(&pool, &room_id, &peer, roles::Role::Member, &owner)
            .await
            .unwrap();
        roles::set_role(&pool, &other_room, &outsider, roles::Role::Member, &owner)
            .await
            .unwrap();
        assert!(manager.rooms.read().get(&room_id).is_none());
        let patch: SettingsPatch =
            serde_json::from_value(json!({"allowChat":false,"maxParticipants":12})).unwrap();
        Context::admit(manager.clone(), room_id.clone(), claims(owner, sessions[0]))
            .await
            .unwrap()
            .execute(Command::Settings(patch))
            .await
            .unwrap();
        assert!(
            manager.rooms.read().get(&room_id).is_none(),
            "HTTP administration must not create a runtime room"
        );
        let (saved, hash) = settings::load_room(&pool, &room_id).await.unwrap().unwrap();
        assert!(!saved.allow_chat);
        assert_eq!(saved.max_participants, Some(12));
        let denied = Context::admit(
            manager.clone(),
            room_id.clone(),
            claims(moderator, sessions[1]),
        )
        .await
        .unwrap()
        .execute(Command::Settings(SettingsPatch::default()))
        .await;
        assert!(denied.is_err(), "Moderator cannot change Admin settings");
        let missing = Context::admit(manager.clone(), room_id.clone(), claims(owner, sessions[0]))
            .await
            .unwrap()
            .execute(Command::Role(outsider, roles::Role::Admin))
            .await;
        assert!(
            matches!(missing, Err(RoomError::Refused(_))),
            "another room's roster is never an account lookup"
        );
        assert!(
            !manager.deleting_rooms.read().contains_key(&room_id),
            "a known missing member must not quarantine the saved room"
        );

        let (sender, mut receiver) = mpsc::channel(64);
        let person = super::super::Participant {
            id: ParticipantId::from(peer),
            name: "Live member".into(),
            sender,
            media_session_id: Uuid::new_v4(),
            producers: HashMap::new(),
            role: roles::Role::Member,
            punitive: moderation::PunitiveState::default(),
            authenticated: true,
            ip: None,
            chat_style: Default::default(),
            social: super::super::social::ParticipantSocial::new(0),
        };
        let mut room = Room::new(room_id.clone(), Some(saved), true, hash);
        room.participants.insert(person.id, person);
        let room = Arc::new(RwLock::new(room));
        manager.rooms.write().insert(room_id.clone(), room.clone());
        Context::admit(manager.clone(), room_id.clone(), claims(owner, sessions[0]))
            .await
            .unwrap()
            .execute(Command::Settings(
                serde_json::from_value(json!({"allowChat":true,"guestsAllowed":false})).unwrap(),
            ))
            .await
            .unwrap();
        let notification: Value = serde_json::from_str(&receiver.recv().await.unwrap()).unwrap();
        assert_eq!(notification["type"], "roomSettingsChanged");
        assert_eq!(notification["settings"]["guestsAllowed"], false);
        assert_eq!(room.read().await.participants.len(), 1);
        assert!(
            !room
                .read()
                .await
                .participants
                .contains_key(&ParticipantId::from(owner))
        );
        Context::admit(manager.clone(), room_id.clone(), claims(owner, sessions[0]))
            .await
            .unwrap()
            .execute(Command::Role(peer, roles::Role::Admin))
            .await
            .unwrap();
        assert_eq!(
            room.read().await.participants[&ParticipantId::from(peer)].role,
            roles::Role::Admin
        );
        let mut listing =
            Context::admit(manager.clone(), room_id.clone(), claims(owner, sessions[0]))
                .await
                .unwrap();
        let members = listing.list("members", 0).await.unwrap();
        assert_eq!(members["members"].as_array().unwrap().len(), 3);
        listing.transaction.take().unwrap().commit().await.unwrap();
        drop(listing);

        let report = Uuid::new_v4();
        sqlx::query("INSERT INTO room_reports(id,room_id,reporter_id,reporter_name,target_participant_id,target_name,reason) VALUES($1,$2,$3,'Reporter',$4,'Target','Reported abuse')")
            .bind(report).bind(&room_id).bind(owner.to_string()).bind(peer.to_string()).execute(&pool).await.unwrap();
        Context::admit(
            manager.clone(),
            room_id.clone(),
            claims(moderator, sessions[1]),
        )
        .await
        .unwrap()
        .execute(Command::Report(
            report,
            moderation::ReportResolution::Dismissed,
        ))
        .await
        .unwrap();
        let mut history = Context::admit(
            manager.clone(),
            room_id.clone(),
            claims(moderator, sessions[1]),
        )
        .await
        .unwrap();
        let events = history.list("history", 0).await.unwrap();
        assert_eq!(events["events"][0]["reportId"], report.to_string());
        assert!(events["events"][0].get("targetIp").is_none());
        history.transaction.take().unwrap().commit().await.unwrap();
        drop(history);
        let stale_report = Context::admit(
            manager.clone(),
            room_id.clone(),
            claims(moderator, sessions[1]),
        )
        .await
        .unwrap()
        .execute(Command::Report(
            report,
            moderation::ReportResolution::Dismissed,
        ))
        .await;
        assert!(matches!(stale_report, Err(RoomError::Refused(_))));
        assert!(
            !room.read().await.deleting,
            "a resolved report is a definite rejection, not an uncertain write"
        );

        for kind in ["camera", "chat"] {
            for enabled in [true, false] {
                Context::admit(manager.clone(), room_id.clone(), claims(owner, sessions[0]))
                    .await
                    .unwrap()
                    .execute(Command::Sanction(SanctionRequest {
                        target_participant_id: ParticipantId::from(peer),
                        kind: kind.into(),
                        enabled,
                        reason: None,
                    }))
                    .await
                    .unwrap();
                let state = room.read().await.participants[&ParticipantId::from(peer)]
                    .punitive
                    .clone();
                assert_eq!(
                    if kind == "camera" {
                        state.cam_banned
                    } else {
                        state.text_muted
                    },
                    enabled
                );
            }
        }
        let mut returning = room.read().await.participants[&ParticipantId::from(peer)].clone();
        Context::admit(manager.clone(), room_id.clone(), claims(owner, sessions[0]))
            .await
            .unwrap()
            .execute(Command::Moderate(ModerationRequest {
                action: "ban".into(),
                target_participant_id: ParticipantId::from(peer),
                reason: Some("Test administration".into()),
                duration_seconds: Some(3600),
                report_id: None,
            }))
            .await
            .unwrap();
        assert!(room.read().await.participants.is_empty());
        assert!(
            room.read()
                .await
                .banned_participants
                .contains_key(&ParticipantId::from(peer))
        );
        let ban: Uuid = sqlx::query_scalar(
            "SELECT id FROM room_states WHERE room_id=$1 AND user_id=$2 AND state='banned'",
        )
        .bind(&room_id)
        .bind(peer)
        .fetch_one(&pool)
        .await
        .unwrap();
        let denied = Context::admit(
            manager.clone(),
            room_id.clone(),
            claims(moderator, sessions[1]),
        )
        .await
        .unwrap()
        .execute(Command::Unban(ban))
        .await;
        assert!(denied.is_err(), "Moderator cannot unban");
        Context::admit(manager.clone(), room_id.clone(), claims(owner, sessions[0]))
            .await
            .unwrap()
            .execute(Command::Unban(ban))
            .await
            .unwrap();
        assert!(
            !room
                .read()
                .await
                .banned_participants
                .contains_key(&ParticipantId::from(peer))
        );
        assert!(
            !moderation::is_banned(&pool, &room_id, Some(peer), None, true)
                .await
                .unwrap()
        );
        let stale_ban =
            Context::admit(manager.clone(), room_id.clone(), claims(owner, sessions[0]))
                .await
                .unwrap()
                .execute(Command::Unban(ban))
                .await;
        assert!(matches!(stale_ban, Err(RoomError::Refused(_))));
        assert!(
            !room.read().await.deleting,
            "an absent ban must not close a healthy room"
        );
        returning.media_session_id = Uuid::new_v4();
        room.write()
            .await
            .participants
            .insert(ParticipantId::from(peer), returning);

        let (waiting_sender, mut waiting_receiver) = mpsc::channel(32);
        let waiting = Arc::new(std::sync::atomic::AtomicBool::new(true));
        room.write().await.lobby.insert(
            ParticipantId::from(outsider),
            super::super::LobbyEntry {
                participant_id: ParticipantId::from(outsider),
                name: "Waiting account".into(),
                sender: waiting_sender,
                chat_style: Default::default(),
                media_session_id: Uuid::new_v4(),
                authenticated: true,
                reconnect_token: Uuid::new_v4().to_string(),
                in_lobby_flag: waiting.clone(),
                ip: None,
                role: roles::Role::User,
                punitive: moderation::PunitiveState::default(),
            },
        );
        Context::admit(manager.clone(), room_id.clone(), claims(owner, sessions[0]))
            .await
            .unwrap()
            .execute(Command::Lobby(
                ParticipantId::from(outsider),
                LobbyRequest {
                    action: "admit".into(),
                    reason: None,
                },
            ))
            .await
            .unwrap();
        assert!(!waiting.load(std::sync::atomic::Ordering::Acquire));
        assert!(
            room.read()
                .await
                .participants
                .contains_key(&ParticipantId::from(outsider))
        );
        let admitted: Value =
            serde_json::from_str(&waiting_receiver.recv().await.unwrap()).unwrap();
        assert_eq!(admitted["type"], "lobbyAdmitted");
        Context::admit(manager.clone(), room_id.clone(), claims(owner, sessions[0]))
            .await
            .unwrap()
            .execute(Command::Moderate(ModerationRequest {
                action: "kick".into(),
                target_participant_id: ParticipantId::from(outsider),
                reason: None,
                duration_seconds: None,
                report_id: None,
            }))
            .await
            .unwrap();
        let denied_id = ParticipantId::random();
        let (denied_sender, mut denied_receiver) = mpsc::channel(16);
        room.write().await.lobby.insert(
            denied_id,
            super::super::LobbyEntry {
                participant_id: denied_id,
                name: "Waiting guest".into(),
                sender: denied_sender,
                chat_style: Default::default(),
                media_session_id: Uuid::new_v4(),
                authenticated: false,
                reconnect_token: Uuid::new_v4().to_string(),
                in_lobby_flag: Arc::new(std::sync::atomic::AtomicBool::new(true)),
                ip: None,
                role: roles::Role::Guest,
                punitive: moderation::PunitiveState::default(),
            },
        );
        Context::admit(manager.clone(), room_id.clone(), claims(owner, sessions[0]))
            .await
            .unwrap()
            .execute(Command::Lobby(
                denied_id,
                LobbyRequest {
                    action: "deny".into(),
                    reason: Some("Closed meeting".into()),
                },
            ))
            .await
            .unwrap();
        assert!(!room.read().await.lobby.contains_key(&denied_id));
        let denied: Value = serde_json::from_str(&denied_receiver.recv().await.unwrap()).unwrap();
        assert_eq!(denied["type"], "lobbyDenied");

        // JWT expiry and refresh-session expiry are rechecked AFTER the room
        // row lock wait, even though both were valid when auth locks completed.
        for expire_session in [false, true] {
            let expires = Utc::now() + chrono::Duration::seconds(2);
            if expire_session {
                sqlx::query("UPDATE sessions SET expires_at=$3 WHERE id=$1 AND user_id=$2")
                    .bind(sessions[0])
                    .bind(owner)
                    .bind(expires)
                    .execute(&pool)
                    .await
                    .unwrap();
            }
            let mut blocker = pool.begin().await.unwrap();
            sqlx::query("SET LOCAL statement_timeout='3s'")
                .execute(&mut *blocker)
                .await
                .unwrap();
            let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
                .fetch_one(&mut *blocker)
                .await
                .unwrap();
            sqlx::query("SELECT id FROM rooms WHERE id=$1 FOR UPDATE")
                .bind(&room_id)
                .fetch_one(&mut *blocker)
                .await
                .unwrap();
            let mut expiring = claims(owner, sessions[0]);
            if !expire_session {
                expiring.exp = expires.timestamp() as usize;
            }
            let request = tokio::spawn(Context::admit(manager.clone(), room_id.clone(), expiring));
            await_owned_lock_wait(&pool, pid).await;
            let remaining = (expires - Utc::now()).num_milliseconds().max(0) as u64;
            tokio::time::sleep(Duration::from_millis(remaining + 20)).await;
            blocker.commit().await.unwrap();
            let error = match request.await.unwrap() {
                Ok(_) => panic!("expired sign-in admitted"),
                Err(error) => error,
            };
            assert_eq!(error.into_response().status(), StatusCode::UNAUTHORIZED);
            if expire_session {
                sqlx::query("UPDATE sessions SET expires_at=now()+interval '1 day' WHERE id=$1 AND user_id=$2").bind(sessions[0]).bind(owner).execute(&pool).await.unwrap();
            }
        }

        // Queue behind the real room control gate, revoke the issuing session,
        // then release admission. Cached claims must not authorize that write.
        let control = control::lock_room(&room).await;
        let request = tokio::spawn(Context::admit(
            manager.clone(),
            room_id.clone(),
            claims(owner, sessions[0]),
        ));
        tokio::task::yield_now().await;
        sqlx::query("DELETE FROM sessions WHERE id=$1 AND user_id=$2")
            .bind(sessions[0])
            .bind(owner)
            .execute(&pool)
            .await
            .unwrap();
        drop(control);
        let error = match request.await.unwrap() {
            Ok(_) => panic!("revoked session admitted"),
            Err(error) => error,
        };
        assert_eq!(error.into_response().status(), StatusCode::UNAUTHORIZED);
        assert_eq!(room.read().await.participants.len(), 1);
    };
    let outcome = AssertUnwindSafe(tokio::time::timeout(Duration::from_secs(50), proof))
        .catch_unwind()
        .await;
    manager.rooms.write().remove(&room_id);
    let cleanup = async {
        sqlx::query("DELETE FROM rooms WHERE id=ANY($1) AND owner_id=$2")
            .bind(vec![room_id, other_room])
            .bind(owner)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM users WHERE id=ANY($1)")
            .bind(users.to_vec())
            .execute(&pool)
            .await
            .unwrap();
    };
    tokio::time::timeout(Duration::from_secs(10), cleanup)
        .await
        .expect("owned rows clean up");
    tokio::time::timeout(Duration::from_secs(10), manager.media_server.shutdown())
        .await
        .expect("owned worker shuts down")
        .unwrap();
    pool.close().await;
    match outcome {
        Ok(result) => result.expect("administration proof timed out"),
        Err(error) => std::panic::resume_unwind(error),
    };
}
