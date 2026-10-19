//! A dormant persisted room is admitted from its durable record before any
//! router exists for the attempt: a refused join leaves no media state behind.

use super::*;
use futures_util::FutureExt;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use std::panic::AssertUnwindSafe;
use std::str::FromStr;
use std::time::Duration;
use uuid::Uuid;

async fn join(
    manager: &RoomManager,
    room_id: &str,
    id: &str,
    authenticated: bool,
    password: Option<&str>,
) -> Result<mpsc::Sender<crate::OutboundJson>> {
    let (sender, _receiver) = mpsc::channel(16);
    manager
        .add_participant(
            room_id,
            id.to_string(),
            "Dormant fixture".to_string(),
            sender.clone(),
            authenticated,
            Arc::new(AtomicBool::new(false)),
            password,
            &format!("{id}-token"),
            Some("198.51.100.7".parse().unwrap()),
            None,
        )
        .await?;
    Ok(sender)
}

async fn has_media_state(manager: &RoomManager, room_id: &str) -> bool {
    manager
        .rooms
        .read()
        .unwrap_or_else(|error| error.into_inner())
        .contains_key(room_id)
        || manager
            .media_server
            .router_manager()
            .get_router(room_id)
            .await
            .is_ok()
}

#[tokio::test]
#[ignore = "requires migrated disposable TEST_DATABASE_URL and mediasoup worker"]
async fn a_refused_join_to_a_dormant_room_creates_no_router() {
    assert_eq!(
        std::env::var("DISPOSABLE_TEST_DATABASE").as_deref(),
        Ok("1")
    );
    let options =
        PgConnectOptions::from_str(&std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL"))
            .expect("valid test PostgreSQL URL");
    assert!(matches!(options.get_host(), "127.0.0.1" | "::1"));
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
    let manager = RoomManager::new(
        crate::room::ManagerSettings::default(),
        config,
        ServerMetrics::new(),
        Some(pool.clone()),
    )
    .await
    .unwrap();
    let owner_id = Uuid::new_v4();
    let visitor_id = Uuid::new_v4();
    let banned_id = Uuid::new_v4();
    let room_id = format!("dormant-{}", Uuid::new_v4());
    let created_before = manager.metrics.rooms_created_total();

    let outcome = AssertUnwindSafe(tokio::time::timeout(Duration::from_secs(60), async {
        for (id, name) in [
            (owner_id, "Owner"),
            (visitor_id, "Visitor"),
            (banned_id, "Banned"),
        ] {
            sqlx::query("INSERT INTO users(id,email,display_name) VALUES($1,$2,$3)")
                .bind(id)
                .bind(format!("{id}@dormant.invalid"))
                .bind(name)
                .execute(&pool)
                .await
                .unwrap();
        }
        let hash = crate::auth::password::hash_password("the-right-passphrase").unwrap();
        sqlx::query(
            "INSERT INTO rooms(id,owner_id,display_name,password_hash,guests_allowed)
             VALUES($1,$2,$1,$3,false)",
        )
        .bind(&room_id)
        .bind(owner_id)
        .bind(&hash)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO room_states(room_id,user_id,state,applied_by,expires_at)
             VALUES($1,$2,'banned',$3,NULL)",
        )
        .bind(&room_id)
        .bind(banned_id)
        .bind(owner_id)
        .execute(&pool)
        .await
        .unwrap();

        let visitor = visitor_id.to_string();
        let banned = banned_id.to_string();
        let refused = [
            (
                "guest-visitor",
                false,
                Some("the-right-passphrase"),
                "Guests are not allowed",
            ),
            (
                visitor.as_str(),
                true,
                Some("wrong"),
                "Incorrect room password",
            ),
            (
                banned.as_str(),
                true,
                Some("the-right-passphrase"),
                "banned",
            ),
        ];
        for (id, authenticated, password, expected) in refused {
            let error = join(&manager, &room_id, id, authenticated, password)
                .await
                .expect_err("the join is refused");
            assert!(error.to_string().contains(expected), "{id}: {error}");
            assert!(
                !has_media_state(&manager, &room_id).await,
                "{id}: a refused join must not create the room"
            );
        }
        let error = join(&manager, &room_id, &visitor_id.to_string(), true, None)
            .await
            .expect_err("a missing password is refused");
        assert!(error.is::<RoomPasswordRequired>(), "{error}");
        assert!(!has_media_state(&manager, &room_id).await);
        assert_eq!(manager.metrics.rooms_created_total(), created_before);

        // The right password creates the room once; the live path then gates
        // the next visitor's wrong password as before.
        let sender = join(
            &manager,
            &room_id,
            &visitor_id.to_string(),
            true,
            Some("the-right-passphrase"),
        )
        .await
        .unwrap();
        assert!(has_media_state(&manager, &room_id).await);
        assert_eq!(manager.metrics.rooms_created_total(), created_before + 1);
        let error = join(
            &manager,
            &room_id,
            &Uuid::new_v4().to_string(),
            true,
            Some("wrong"),
        )
        .await
        .expect_err("the live gate still refuses");
        assert!(
            error.to_string().contains("Incorrect room password"),
            "{error}"
        );
        manager
            .remove_participant_for_sender(&room_id, &visitor_id.to_string(), &sender)
            .await
            .unwrap();
    }))
    .catch_unwind()
    .await;

    sqlx::query("DELETE FROM rooms WHERE id = $1")
        .bind(&room_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM users WHERE id = ANY($1)")
        .bind(vec![owner_id, visitor_id, banned_id])
        .execute(&pool)
        .await
        .unwrap();
    match outcome {
        Ok(Ok(())) => {}
        Ok(Err(_)) => panic!("dormant join test timed out"),
        Err(panic) => std::panic::resume_unwind(panic),
    }
}
