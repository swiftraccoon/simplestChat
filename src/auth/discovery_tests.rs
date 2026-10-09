use super::*;

#[test]
fn contact_preferences_match_private_message_optout_and_ignore() {
    let peer = Uuid::new_v4();
    assert!(allows_contact(&serde_json::json!({}), peer));
    assert!(!allows_contact(
        &serde_json::json!({"allowPrivateMessages":false}),
        peer
    ));
    assert!(!allows_contact(
        &serde_json::json!({"ignored":[{"id":peer,"name":"Someone"}]}),
        peer
    ));
    assert!(allows_contact(
        &serde_json::json!({"ignored":[{"id":Uuid::new_v4()}]}),
        peer
    ));
    let own = Uuid::new_v4();
    assert_eq!(ordered_pair(own, peer), ordered_pair(peer, own));
}

async fn pool() -> PgPool {
    let url = std::env::var("TEST_DATABASE_URL").expect("migrated disposable TEST_DATABASE_URL");
    sqlx::postgres::PgPoolOptions::new()
        .max_connections(4)
        .connect(&url)
        .await
        .unwrap()
}

async fn person(pool: &PgPool, name: &str) -> Claims {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO users (id,email,display_name) VALUES ($1,$2,$3)")
        .bind(id)
        .bind(format!("discovery-{id}@example.test"))
        .bind(name)
        .execute(pool)
        .await
        .unwrap();
    let sid = crate::auth::session::create_session(
        pool,
        &id,
        &crate::auth::session::generate_refresh_token().unwrap(),
    )
    .await
    .unwrap();
    let token = crate::auth::jwt::create_session_token(
        &id.to_string(),
        name,
        "discovery-test-secret-at-least-32-bytes",
        0,
        sid,
    )
    .unwrap();
    crate::auth::jwt::validate_token(&token, "discovery-test-secret-at-least-32-bytes").unwrap()
}

async fn cleanup(pool: &PgPool, ids: &[Uuid]) {
    sqlx::query("DELETE FROM rooms WHERE owner_id=ANY($1)")
        .bind(ids)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM users WHERE id=ANY($1)")
        .bind(ids)
        .execute(pool)
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "requires a migrated disposable TEST_DATABASE_URL"]
async fn database_discovery_contacts_require_acceptance_and_current_session() {
    let pool = pool().await;
    let alice = person(&pool, "Alice").await;
    let bob = person(&pool, "Bob").await;
    let aid = account_id(&alice).unwrap();
    let bid = account_id(&bob).unwrap();
    request_contact(&pool, &alice, bid).await.unwrap();
    let mut connection = pool.acquire().await.unwrap();
    assert!(!accepted_contact(&mut connection, aid, bid).await.unwrap());
    assert!(change_contact(&pool, &alice, bid, true).await.is_err());
    change_contact(&pool, &bob, aid, true).await.unwrap();
    assert!(accepted_contact(&mut connection, aid, bid).await.unwrap());
    assert!(accepted_contact(&mut connection, bid, aid).await.unwrap());
    // A second device shares the accepted relationship, not the session id.
    let second = crate::auth::session::create_session(
        &pool,
        &aid,
        &crate::auth::session::generate_refresh_token().unwrap(),
    )
    .await
    .unwrap();
    let mut other_device = alice.clone();
    other_device.sid = second;
    request_contact(&pool, &other_device, bid).await.unwrap();
    sqlx::query("DELETE FROM sessions WHERE id=$1")
        .bind(alice.sid)
        .execute(&pool)
        .await
        .unwrap();
    assert!(matches!(
        change_contact(&pool, &alice, bid, false).await,
        Err(AuthError::InvalidToken)
    ));
    change_contact(&pool, &other_device, bid, false)
        .await
        .unwrap();
    assert!(!accepted_contact(&mut connection, aid, bid).await.unwrap());
    // Re-requesting immediately after removal must not recreate harassment.
    request_contact(&pool, &other_device, bid).await.unwrap();
    assert!(change_contact(&pool, &bob, aid, true).await.is_err());
    drop(connection);
    cleanup(&pool, &[aid, bid]).await;
}

#[tokio::test]
#[ignore = "requires a migrated disposable TEST_DATABASE_URL"]
async fn database_discovery_contact_requests_respect_optout_ignore_and_expiry() {
    let pool = pool().await;
    let alice = person(&pool, "Alice").await;
    let bob = person(&pool, "Bob").await;
    let aid = account_id(&alice).unwrap();
    let bid = account_id(&bob).unwrap();
    for preferences in [
        serde_json::json!({"allowPrivateMessages":false}),
        serde_json::json!({"ignored":[{"id":aid}]}),
    ] {
        sqlx::query("UPDATE users SET preferences=$2 WHERE id=$1")
            .bind(bid)
            .bind(preferences)
            .execute(&pool)
            .await
            .unwrap();
        request_contact(&pool, &alice, bid).await.unwrap();
        assert!(change_contact(&pool, &bob, aid, true).await.is_err());
    }
    request_contact(&pool, &alice, Uuid::new_v4())
        .await
        .unwrap();
    sqlx::query("UPDATE users SET preferences='{}'::jsonb WHERE id=$1")
        .bind(bid)
        .execute(&pool)
        .await
        .unwrap();
    request_contact(&pool, &alice, bid).await.unwrap();
    sqlx::query("UPDATE users SET preferences=$2 WHERE id=$1")
        .bind(aid)
        .bind(serde_json::json!({"ignored":[{"id":bid}]}))
        .execute(&pool)
        .await
        .unwrap();
    assert!(change_contact(&pool, &bob, aid, true).await.is_err());
    sqlx::query("UPDATE users SET preferences='{}'::jsonb WHERE id=$1")
        .bind(aid)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE contacts SET requested_at=now()-interval '15 days' WHERE requester_id=$1")
        .bind(aid)
        .execute(&pool)
        .await
        .unwrap();
    assert!(change_contact(&pool, &bob, aid, true).await.is_err());
    cleanup(&pool, &[aid, bid]).await;
}

#[tokio::test]
#[ignore = "requires a migrated disposable TEST_DATABASE_URL"]
async fn database_discovery_room_shortcuts_preserve_favorites_and_bound_recents() {
    let pool = pool().await;
    let alice = person(&pool, "Alice").await;
    let bob = person(&pool, "Bob").await;
    let aid = account_id(&alice).unwrap();
    let bid = account_id(&bob).unwrap();
    let private = format!("discovery-{}-private", Uuid::new_v4());
    sqlx::query(
        "INSERT INTO rooms (id,owner_id,display_name,secret) VALUES ($1,$2,'Private',true)",
    )
    .bind(&private)
    .bind(bid)
    .execute(&pool)
    .await
    .unwrap();
    assert!(save_favorite(&pool, &alice, &private, true).await.is_err());
    sqlx::query("INSERT INTO room_roles (room_id,user_id,role) VALUES ($1,$2,1)")
        .bind(&private)
        .bind(aid)
        .execute(&pool)
        .await
        .unwrap();
    save_favorite(&pool, &alice, &private, true).await.unwrap();
    assert_eq!(visible_saved_entries(&pool, aid).await.unwrap().len(), 1);
    sqlx::query("DELETE FROM room_roles WHERE room_id=$1 AND user_id=$2")
        .bind(&private)
        .bind(aid)
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        visible_saved_entries(&pool, aid).await.unwrap().is_empty(),
        "A saved unlisted room loses its labels when membership is revoked"
    );
    assert!(
        visible_saved_entries(&pool, bid).await.unwrap().is_empty(),
        "Room ownership cannot read someone else's shortcuts"
    );
    let mut last = String::new();
    for index in 0..55 {
        last = format!("discovery-{aid}-{index}");
        sqlx::query("INSERT INTO rooms (id,owner_id,display_name) VALUES ($1,$2,'Public')")
            .bind(&last)
            .bind(aid)
            .execute(&pool)
            .await
            .unwrap();
        record_room_visit(&pool, aid, &last).await.unwrap();
    }
    let counts: (i64,i64) = sqlx::query_as("SELECT count(*) FILTER (WHERE favorite),count(*) FILTER (WHERE NOT favorite) FROM saved_rooms WHERE user_id=$1")
        .bind(aid).fetch_one(&pool).await.unwrap();
    assert_eq!(counts, (1, MAX_RECENT));
    save_favorite(&pool, &alice, &last, true).await.unwrap();
    save_favorite(&pool, &alice, &last, false).await.unwrap();
    record_room_visit(&pool, aid, "nonexistent-ad-hoc-room")
        .await
        .unwrap();
    save_favorite(&pool, &alice, &private, false).await.unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM saved_rooms WHERE user_id=$1")
        .bind(aid)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, MAX_RECENT);
    let own: i64 = sqlx::query_scalar("SELECT count(*) FROM saved_rooms WHERE user_id=$1")
        .bind(bid)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(own, 0);
    cleanup(&pool, &[aid, bid]).await;
}

#[tokio::test]
#[ignore = "requires a migrated disposable TEST_DATABASE_URL"]
async fn database_discovery_contact_and_favorite_caps_serialize_concurrent_writes() {
    let pool = pool().await;
    let alice = person(&pool, "Alice").await;
    let bob = person(&pool, "Bob").await;
    let aid = account_id(&alice).unwrap();
    let bid = account_id(&bob).unwrap();
    let mut ids = vec![aid, bid];
    for _ in 0..MAX_REQUESTS_PER_DAY {
        let target = Uuid::new_v4();
        sqlx::query("INSERT INTO users (id,email,display_name) VALUES ($1,$2,'Contact cap')")
            .bind(target)
            .bind(format!("discovery-{target}@example.test"))
            .execute(&pool)
            .await
            .unwrap();
        ids.push(target);
        request_contact(&pool, &alice, target).await.unwrap();
    }
    assert!(matches!(
        request_contact(&pool, &alice, bid).await,
        Err(AuthError::RateLimited)
    ));
    // Fill the account's accepted relationships directly to isolate the active
    // cap from the separate daily request limiter.
    sqlx::query("UPDATE contacts SET status='accepted',requested_at=now()-interval '2 days' WHERE requester_id=$1")
        .bind(aid).execute(&pool).await.unwrap();
    for _ in MAX_REQUESTS_PER_DAY..MAX_CONTACTS {
        let target = Uuid::new_v4();
        sqlx::query("INSERT INTO users (id,email,display_name) VALUES ($1,$2,'Contact cap')")
            .bind(target)
            .bind(format!("discovery-{target}@example.test"))
            .execute(&pool)
            .await
            .unwrap();
        ids.push(target);
        let (low, high) = ordered_pair(aid, target);
        sqlx::query("INSERT INTO contacts (low_id,high_id,requester_id,status,requested_at) VALUES ($1,$2,$3,'accepted',now()-interval '2 days')")
            .bind(low).bind(high).bind(aid).execute(&pool).await.unwrap();
    }
    assert!(matches!(
        request_contact(&pool, &alice, bid).await,
        Err(AuthError::InvalidInput(_))
    ));
    request_contact(&pool, &bob, aid).await.unwrap();
    assert!(
        !accepted_contact(&mut pool.acquire().await.unwrap(), aid, bid)
            .await
            .unwrap()
    );
    for index in 0..101 {
        let room = format!("discovery-cap-{aid}-{index}");
        sqlx::query("INSERT INTO rooms (id,owner_id,display_name) VALUES ($1,$2,'Favorite cap')")
            .bind(&room)
            .bind(aid)
            .execute(&pool)
            .await
            .unwrap();
        if index < 99 {
            save_favorite(&pool, &alice, &room, true).await.unwrap();
        }
    }
    let first_room = format!("discovery-cap-{aid}-99");
    let second_room = format!("discovery-cap-{aid}-100");
    let (first, second) = tokio::join!(
        save_favorite(&pool, &alice, &first_room, true),
        save_favorite(&pool, &alice, &second_room, true)
    );
    assert_ne!(
        first.is_ok(),
        second.is_ok(),
        "Only the hundredth favorite may commit"
    );
    assert_eq!(
        visible_saved_entries(&pool, aid).await.unwrap().len(),
        MAX_FAVORITES as usize
    );
    cleanup(&pool, &ids).await;
}
