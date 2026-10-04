//! Persisted appearance remains independent of chat and honors account/room ownership.

use super::*;

async fn body(response: Response) -> Value {
    serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), 32 * 1024)
            .await
            .unwrap(),
    )
    .unwrap()
}

#[tokio::test]
#[ignore = "requires DISPOSABLE_TEST_DATABASE=1 and TEST_DATABASE_URL"]
async fn authorization_database_appearance_persists_independently_and_only_owner_updates_room() {
    let fixture = Fixture::new(2).await;
    let automatic = json!({"color":null,"style":"accent"});
    let profile_style = json!({"color":"teal","style":"text"});
    sqlx::query("UPDATE users SET chat_color='pink', chat_style='bubble' WHERE id=$1")
        .bind(fixture.users[0])
        .execute(&fixture.pool)
        .await
        .unwrap();
    let profile = operation(
        "PATCH",
        "/api/auth/profile".into(),
        Some(
            json!({"display_name":"Account name", "avatar_url":null,"bio":"Public bio",
            "profile_style":profile_style}),
        ),
    );
    let response = fixture.request(&profile, Some(&fixture.tokens[0]), 1).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body(response).await["profile_style"], profile_style);
    let saved: (Value, String, String) =
        sqlx::query_as("SELECT profile_style, chat_color, chat_style FROM users WHERE id=$1")
            .bind(fixture.users[0])
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    assert_eq!(
        saved,
        (profile_style.clone(), "pink".into(), "bubble".into())
    );
    let other_style: Value = sqlx::query_scalar("SELECT profile_style FROM users WHERE id=$1")
        .bind(fixture.users[1])
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
    assert_eq!(other_style, automatic);
    let public = operation(
        "GET",
        format!("/api/auth/profiles/{}", fixture.users[0]),
        None,
    );
    let response = fixture.request(&public, None, 2).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body(response).await["profile_style"], profile_style);

    let room = format!("appearance-{}", Uuid::new_v4());
    sqlx::query("INSERT INTO rooms(id,owner_id,display_name) VALUES($1,$2,'Our room')")
        .bind(&room)
        .bind(fixture.users[0])
        .execute(&fixture.pool)
        .await
        .unwrap();
    roles::set_role(
        &fixture.pool,
        &room,
        &fixture.users[1],
        Role::Admin,
        &fixture.users[0],
    )
    .await
    .unwrap();
    let (sender, mut receiver) = tokio::sync::mpsc::channel(64);
    fixture
        .server
        .room_manager()
        .add_participant(
            &room,
            fixture.users[0].to_string(),
            "Room nickname".into(),
            sender,
            true,
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
            None,
            "appearance-fixture",
            None,
            None,
        )
        .await
        .unwrap();
    while receiver.try_recv().is_ok() {}
    let name_style = json!({"color":"orange","style":"bubble"});
    let topic_style = json!({"color":"violet","style":"text"});
    let identity = operation(
        "PATCH",
        format!("/api/rooms/{room}/identity"),
        Some(
            json!({"display_name":"Our styled room", "description":"", "image_url":null,
            "topic":"Room description", "name_style":name_style,"topic_style":topic_style}),
        ),
    );
    let denied = fixture
        .request(&identity, Some(&fixture.tokens[1]), 3)
        .await;
    assert_eq!(
        denied.status(),
        StatusCode::NOT_FOUND,
        "admins are not owners"
    );
    let unchanged: (Value, Value) =
        sqlx::query_as("SELECT name_style, topic_style FROM rooms WHERE id=$1")
            .bind(&room)
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    assert_eq!(unchanged, (automatic.clone(), automatic));
    assert!(receiver.try_recv().is_err(), "denied edit cannot broadcast");
    let response = fixture
        .request(&identity, Some(&fixture.tokens[0]), 4)
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let response = body(response).await;
    assert_eq!(response["name_style"], name_style);
    assert_eq!(response["topic_style"], topic_style);
    let broadcast: Value = serde_json::from_str(&receiver.try_recv().unwrap()).unwrap();
    assert_eq!(broadcast["type"], "roomSettingsChanged");
    assert_eq!(broadcast["settings"]["nameStyle"], name_style);
    assert_eq!(broadcast["settings"]["topicStyle"], topic_style);
    let (saved, _) = crate::room::settings::load_room(&fixture.pool, &room)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(serde_json::to_value(saved.name_style).unwrap(), name_style);
    assert_eq!(
        serde_json::to_value(saved.topic_style).unwrap(),
        topic_style
    );
    let memberships = crate::room::invites::memberships(&fixture.pool, fixture.users[1], None)
        .await
        .unwrap();
    let member_room =
        crate::room::api::room_list_item(&fixture.server, memberships.rows[0].0.clone());
    assert_eq!(
        serde_json::to_value(member_room.name_style).unwrap(),
        name_style
    );
    assert_eq!(
        serde_json::to_value(member_room.topic_style).unwrap(),
        topic_style
    );
    fixture.finish().await;
}
