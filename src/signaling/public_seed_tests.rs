//! The public lobby's seeding transaction against this server: what the
//! library sends and reads is what the server accepts and serializes, and a
//! whole seeding through the real router on a loopback port creates the
//! owner and the lobby once and afterwards changes nothing.

use super::*;
use crate::auth::types::{AuthResponse, UserInfo};
use crate::room::settings::{self, CreateRoomRequest};
use crate::room::{RoomSettings, RoomSettingsView};
use simplestchat_lobby_seed::{
    Limits, Owner, ROOM_REQUEST, Report, authentication, created_lobby, parse, seed,
};

/// The request the library sends is one this server accepts whole, and the
/// answers it reads are spelled as this server spells them: each of the six
/// settings a new lobby must state is found under the name it is sent by.
#[test]
fn the_seeding_library_speaks_this_servers_contract() {
    let request: CreateRoomRequest = serde_json::from_str(ROOM_REQUEST).unwrap();
    let room = settings::validate_create_request(&request).unwrap();
    assert_eq!(room, rid("lobby"));
    let token = "t".repeat(simplestchat_lobby_seed::TOKEN_CHARACTERS);
    let email = "owner@chat.example.test";
    let response = AuthResponse {
        token: token.clone().into(),
        expires_at: chrono::Utc::now(),
        user: UserInfo {
            id: Uuid::new_v4().to_string(),
            email: email.to_owned(),
            display_name: "Test room owner".to_owned(),
        },
    };
    let answer = parse(&serde_json::to_vec(&response).unwrap()).unwrap();
    assert_eq!(authentication(Some(&answer), email), Ok(token.as_str()));
    let other = authentication(Some(&answer), "owner@other.example.test");
    assert_eq!(other, Err("owner_identity_mismatch"));
    let created = RoomSettings::defaults(&room);
    let view = |settings: &RoomSettings| {
        let sent = serde_json::to_vec(&RoomSettingsView::from(settings)).unwrap();
        created_lobby(Some(&parse(&sent).unwrap()))
    };
    assert_eq!(view(&created), Ok(()));
    type Change = fn(&mut RoomSettings);
    let changes: [Change; 6] = [
        |settings| settings.secret = true,
        |settings| settings.password_protected = true,
        |settings| settings.require_registration = true,
        |settings| settings.lobby_enabled = true,
        |settings| settings.guests_allowed = false,
        |settings| settings.guests_can_broadcast = false,
    ];
    for change in changes {
        let mut changed = created.clone();
        change(&mut changed);
        assert_eq!(view(&changed), Err("unexpected_new_room_settings"));
    }
    let elsewhere = RoomSettings::defaults(&rid("another"));
    assert_eq!(view(&elsewhere), Err("room_ownership_mismatch"));
}

/// One seeding through the real router creates the owner's account and the
/// lobby with the requested settings and ends its session; a second one
/// finds both, creates nothing and leaves the owner's later changes alone.
#[tokio::test]
#[ignore = "requires DISPOSABLE_TEST_DATABASE=1 and TEST_DATABASE_URL"]
async fn the_lobby_is_seeded_once_through_the_real_router_and_then_preserved() {
    let nonce = Uuid::new_v4().simple().to_string();
    let origin = format!("https://seed-{nonce}.example.test");
    let config = crate::config::Config::from_source(&[("ALLOWED_ORIGINS", origin.as_str())]);
    let mut fixture = Fixture::configured(0, &config.unwrap()).await;
    fixture.server.registration_enabled = true;
    let lobbies = "SELECT count(*) FROM rooms WHERE id = 'lobby'";
    let existing: i64 = sqlx::query_scalar(lobbies)
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
    assert_eq!(existing, 0, "the disposable database already holds a lobby");
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let address = listener.local_addr().unwrap();
    let router = fixture.server.clone().router();
    let serving = tokio::spawn(async move {
        let service = router.into_make_service_with_connect_info::<SocketAddr>();
        axum::serve(listener, service).await.unwrap();
    });
    let owner = Owner {
        email: format!("owner@seed-{nonce}.example.test"),
        password: format!("Seeding-fixture-phrase-{nonce}"),
    };
    let run = || {
        let (origin, owner) = (origin.clone(), owner.clone());
        let limits = Limits::within(Duration::from_secs(30));
        tokio::task::spawn_blocking(move || seed(address, &origin, &owner, &limits))
    };

    let first = run().await.unwrap();
    let created = Report {
        owner_created: true,
        room_created: true,
        refresh_session_revoked: Some(true),
        ..Report::default()
    };
    assert_eq!(first, created);
    let registered = "SELECT id FROM users WHERE email = $1";
    let user: Uuid = sqlx::query_scalar(registered)
        .bind(&owner.email)
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
    fixture.users.push(user);
    let (lobby, password) = settings::load_room(&fixture.pool, &rid("lobby"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!((lobby.owner_id.as_uuid(), password), (user, None));
    let stated = (
        lobby.display_name.as_str(),
        lobby.max_participants,
        lobby.max_broadcasters,
        lobby.topic.as_deref(),
    );
    let topic = "Public test room. Test data may be reset; do not share sensitive information.";
    assert_eq!(
        stated,
        ("SimplestChat Test Lobby", Some(30), Some(6), Some(topic))
    );
    let open = !lobby.secret
        && !lobby.password_protected
        && !lobby.require_registration
        && !lobby.lobby_enabled
        && !lobby.moderated
        && lobby.guests_allowed
        && lobby.guests_can_broadcast;
    assert!(open);
    let sessions = "SELECT count(*) FROM sessions WHERE user_id = $1";
    let held: i64 = sqlx::query_scalar(sessions)
        .bind(user)
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
    assert_eq!(held, 0);

    // The owner renames and closes the room; the next seeding keeps that.
    sqlx::query(
        "UPDATE rooms SET display_name = 'Renamed by its owner', secret = true WHERE id = 'lobby'",
    )
    .execute(&fixture.pool)
    .await
    .unwrap();
    let second = run().await.unwrap();
    let preserved = Report {
        existing_settings_preserved: true,
        refresh_session_revoked: Some(true),
        ..Report::default()
    };
    assert_eq!(second, preserved);
    let (lobby, _) = settings::load_room(&fixture.pool, &rid("lobby"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (lobby.display_name.as_str(), lobby.secret),
        ("Renamed by its owner", true)
    );
    let counted = "SELECT count(*) FROM users WHERE email = $1";
    let owners: i64 = sqlx::query_scalar(counted)
        .bind(&owner.email)
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
    let held: i64 = sqlx::query_scalar(sessions)
        .bind(user)
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
    assert_eq!((owners, held), (1, 0));

    // Another account's password for the same address is a conflict that
    // changes nothing and holds no session.
    let stranger = Owner {
        password: format!("Another-fixture-phrase-{nonce}"),
        ..owner.clone()
    };
    let limits = Limits::within(Duration::from_secs(30));
    let conflict = tokio::task::spawn_blocking({
        let origin = origin.clone();
        move || seed(address, &origin, &stranger, &limits)
    });
    let refused = Report {
        failure: Some("owner_credentials_conflict"),
        ..Report::default()
    };
    assert_eq!(conflict.await.unwrap(), refused);
    serving.abort();
    fixture.finish().await;
}
