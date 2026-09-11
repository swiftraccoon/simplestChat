use super::*;
use crate::signaling::protocol::ClientMessage;
use futures_util::FutureExt;
use serde_json::{Value, json};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use std::panic::AssertUnwindSafe;
use std::str::FromStr;
use std::time::Duration;
use uuid::Uuid;

async fn apply_wire_patch(manager: &RoomManager, room_id: &str, owner: &Participant, patch: Value) {
    let ClientMessage::UpdateRoomSettings {
        moderated,
        lobby_enabled,
        guests_allowed,
        guests_can_broadcast,
        max_broadcasters,
        max_participants,
        allow_screen_sharing,
        allow_chat,
        allow_video,
        require_registration,
        invite_only,
        push_to_talk,
        secret,
        password,
    } = serde_json::from_value(patch).unwrap()
    else {
        panic!("expected a room settings patch");
    };
    manager
        .update_room_settings(
            room_id,
            &owner.id,
            &owner.sender,
            moderated,
            lobby_enabled,
            guests_allowed,
            guests_can_broadcast,
            max_broadcasters,
            max_participants,
            allow_screen_sharing,
            allow_chat,
            allow_video,
            require_registration,
            invite_only,
            push_to_talk,
            secret,
            password,
        )
        .await
        .unwrap();
}

async fn assert_saved_settings(
    manager: &RoomManager,
    pool: &sqlx::PgPool,
    room_id: &str,
    participants: Option<i32>,
    broadcasters: Option<i32>,
    protected: bool,
) -> Option<String> {
    let (persisted, hash) = settings::load_room(pool, room_id).await.unwrap().unwrap();
    assert_eq!(persisted.max_participants, participants);
    assert_eq!(persisted.max_broadcasters, broadcasters);
    assert_eq!(persisted.password_protected, protected);
    assert_eq!(hash.is_some(), protected);
    let room_lock = manager.get_room(room_id).unwrap();
    let room = room_lock.read().await;
    assert_eq!(room.settings.as_ref(), Some(&persisted));
    assert_eq!(room.password_hash, hash);
    hash
}

#[tokio::test]
#[ignore = "requires migrated disposable TEST_DATABASE_URL and mediasoup worker"]
async fn wire_settings_set_omit_and_clear_are_persisted() {
    assert_eq!(
        std::env::var("DISPOSABLE_TEST_DATABASE").as_deref(),
        Ok("1")
    );
    let options =
        PgConnectOptions::from_str(&std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL"))
            .expect("valid test PostgreSQL URL");
    assert!(matches!(options.get_host(), "127.0.0.1" | "::1"));
    assert!(
        options.get_socket().is_none(),
        "use a loopback TCP database"
    );
    assert!(
        options
            .get_database()
            .is_some_and(|name| name.ends_with("_test"))
    );
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .acquire_timeout(Duration::from_secs(5))
        .connect_with(options)
        .await
        .unwrap();
    let mut config = MediaConfig::default();
    config.worker_config.num_workers = 1;
    config.webrtc_server_port_base = 0; // Only this test's ephemeral UDP socket.
    let manager = RoomManager::new(config, ServerMetrics::new(), Some(pool.clone()))
        .await
        .unwrap();
    let owner_id = Uuid::new_v4();
    let room_id = format!("settings-{}", Uuid::new_v4());
    let (sender, _receiver) = mpsc::channel(16);
    let owner = Participant {
        id: owner_id.to_string(),
        social: social::ParticipantSocial::new(0),
        name: "Settings test owner".into(),
        sender,
        media_session_id: Uuid::new_v4(),
        producers: HashMap::new(),
        role: roles::Role::Owner,
        punitive: moderation::PunitiveState::default(),
        authenticated: true,
        ip: None,
    };

    // Assertions and timeouts cannot skip cleanup of this test's own rows.
    let outcome = AssertUnwindSafe(tokio::time::timeout(Duration::from_secs(30), async {
        sqlx::query("INSERT INTO users(id,email,display_name) VALUES($1,$2,$3)")
            .bind(owner_id)
            .bind(format!("{owner_id}@settings.invalid"))
            .bind(&owner.name)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO rooms(id,owner_id,display_name) VALUES($1,$2,$1)")
            .bind(&room_id)
            .bind(owner_id)
            .execute(&pool)
            .await
            .unwrap();
        let (initial, hash) = settings::load_room(&pool, &room_id).await.unwrap().unwrap();
        let mut room = Room::new(
            room_id.clone(),
            "unused-router".into(),
            Some(initial),
            true,
            hash,
        );
        room.participants.insert(owner.id.clone(), owner.clone());
        manager
            .rooms
            .write()
            .unwrap()
            .insert(room_id.clone(), Arc::new(TokioRwLock::new(room)));

        apply_wire_patch(
            &manager,
            &room_id,
            &owner,
            json!({
                "type": "updateRoomSettings", "password": "room-passphrase",
                "maxParticipants": 12, "maxBroadcasters": 4,
            }),
        )
        .await;
        let original_hash =
            assert_saved_settings(&manager, &pool, &room_id, Some(12), Some(4), true).await;
        assert_ne!(original_hash.as_deref(), Some("room-passphrase"));

        apply_wire_patch(
            &manager,
            &room_id,
            &owner,
            json!({
                "type": "updateRoomSettings", "allowChat": false,
            }),
        )
        .await;
        assert_eq!(
            assert_saved_settings(&manager, &pool, &room_id, Some(12), Some(4), true).await,
            original_hash
        );

        apply_wire_patch(
            &manager,
            &room_id,
            &owner,
            json!({
                "type": "updateRoomSettings", "maxBroadcasters": null,
            }),
        )
        .await;
        assert_eq!(
            assert_saved_settings(&manager, &pool, &room_id, Some(12), None, true).await,
            original_hash
        );

        apply_wire_patch(
            &manager,
            &room_id,
            &owner,
            json!({
                "type": "updateRoomSettings", "password": null, "maxParticipants": null,
            }),
        )
        .await;
        assert_saved_settings(&manager, &pool, &room_id, None, None, false).await;
        let (reloaded, _) = settings::load_room(&pool, &room_id).await.unwrap().unwrap();
        assert!(
            !reloaded.allow_chat,
            "clearing nullable fields must preserve unrelated settings"
        );
    }))
    .catch_unwind()
    .await;

    manager.rooms.write().unwrap().remove(&room_id);
    let cleanup = tokio::time::timeout(Duration::from_secs(10), async {
        sqlx::query("DELETE FROM rooms WHERE id=$1 AND owner_id=$2")
            .bind(&room_id)
            .bind(owner_id)
            .execute(&pool)
            .await?;
        sqlx::query("DELETE FROM users WHERE id=$1")
            .bind(owner_id)
            .execute(&pool)
            .await?;
        Ok::<(), sqlx::Error>(())
    })
    .await;
    let shutdown =
        tokio::time::timeout(Duration::from_secs(10), manager.media_server.shutdown()).await;
    let close = tokio::time::timeout(Duration::from_secs(5), pool.close()).await;
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
    outcome.unwrap().expect("settings mutation test timed out");
    cleanup
        .expect("test row cleanup timed out")
        .expect("test row cleanup failed");
    shutdown
        .expect("test worker shutdown timed out")
        .expect("test worker shutdown failed");
    close.expect("test pool close timed out");
}
