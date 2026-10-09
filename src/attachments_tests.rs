use super::*;

#[test]
fn attachments_accept_bounded_unique_ids_and_sanitize_names() {
    let id = Uuid::new_v4();
    assert!(valid_ids(&[id]));
    assert!(!valid_ids(&[id, id]));
    assert!(!valid_ids(
        &(0..5).map(|_| Uuid::new_v4()).collect::<Vec<_>>()
    ));
    assert_eq!(
        filename(&URL_SAFE_NO_PAD.encode("folder/report.pdf")).unwrap(),
        "report.pdf"
    );
    assert_eq!(
        filename(&URL_SAFE_NO_PAD.encode("photo 2026.png")).unwrap(),
        "photo 2026.png"
    );
    assert!(filename(&URL_SAFE_NO_PAD.encode("n".repeat(256))).is_err());
    assert_eq!(
        content_type(b"ordinary document"),
        "application/octet-stream"
    );
    let png=base64::engine::general_purpose::STANDARD.decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aCGUAAAAASUVORK5CYII=").unwrap();
    assert_eq!(content_type(&png), "image/png");
}

struct Database {
    pool: PgPool,
    users: Vec<Uuid>,
    claims: Vec<Claims>,
    rooms: Vec<String>,
}
impl Database {
    async fn new() -> Self {
        use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
        use std::str::FromStr;
        let url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL");
        let options = PgConnectOptions::from_str(&url).unwrap();
        assert!(
            matches!(options.get_host(), "127.0.0.1" | "::1" | "localhost")
                && options
                    .get_database()
                    .is_some_and(|name| name.ends_with("_test"))
        );
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .connect_with(options)
            .await
            .unwrap();
        let mut db = Self {
            pool,
            users: Vec::new(),
            claims: Vec::new(),
            rooms: Vec::new(),
        };
        for _ in 0..2 {
            let user: Uuid = sqlx::query_scalar(
                "INSERT INTO users(email,display_name) VALUES($1,'Attachment tester') RETURNING id",
            )
            .bind(format!("{}@attachments.invalid", Uuid::new_v4()))
            .fetch_one(&db.pool)
            .await
            .unwrap();
            let sid = crate::auth::session::create_session(
                &db.pool,
                &user,
                &crate::auth::session::generate_refresh_token().unwrap(),
            )
            .await
            .unwrap();
            db.users.push(user);
            db.claims.push(Claims {
                sub: user.to_string(),
                name: "Attachment tester".into(),
                iss: "simplestChat".into(),
                aud: "simplestChat".into(),
                exp: (Utc::now().timestamp() + 3600) as usize,
                auth_version: 0,
                sid,
            });
        }
        db
    }
    async fn room(&mut self, days: i32) -> String {
        let id = format!("attachment-{}", Uuid::new_v4());
        sqlx::query("INSERT INTO rooms(id,owner_id,display_name,history_retention_days) VALUES($1,$2,'Attachment test room',$3)")
            .bind(&id).bind(self.users[0]).bind(days).execute(&self.pool).await.unwrap();
        self.rooms.push(id.clone());
        id
    }
    async fn upload(&self, owner: usize, data: Vec<u8>) -> ChatAttachment {
        store_upload(&self.pool, &self.claims[owner], "notes.txt".into(), data)
            .await
            .unwrap()
    }
    async fn close(self) {
        sqlx::query("DELETE FROM rooms WHERE id=ANY($1)")
            .bind(self.rooms)
            .execute(&self.pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM users WHERE id=ANY($1)")
            .bind(self.users)
            .execute(&self.pool)
            .await
            .unwrap();
        self.pool.close().await;
    }
    fn message(&self) -> crate::signaling::protocol::ChatEntry {
        crate::signaling::protocol::ChatEntry {
            message_id: Uuid::new_v4().to_string(),
            client_message_id: Uuid::new_v4().to_string(),
            participant_id: self.users[0].to_string(),
            participant_name: "Attachment tester".into(),
            recipient_id: None,
            recipient_name: None,
            content: String::new(),
            attachments: Vec::new(),
            sent_at: Utc::now().to_rfc3339(),
            removed_at: None,
            revision: 0,
            edited_at: None,
            chat_style: Default::default(),
            reply_to: None,
            reactions: Vec::new(),
        }
    }
}

#[tokio::test]
#[ignore = "requires a migrated disposable TEST_DATABASE_URL"]
async fn database_attachments_claim_atomically_require_owner_and_preserve_retry_identity() {
    let mut db = Database::new().await;
    let room = db.room(7).await;
    let file = db.upload(0, b"retained attachment".to_vec()).await;
    let mut tx = db.pool.begin().await.unwrap();
    assert!(
        pending_metadata(&mut tx, db.users[1], &[file.id])
            .await
            .is_err()
    );
    tx.rollback().await.unwrap();
    let message = db.message();
    let session = Uuid::new_v4();
    let saved = crate::room::history::persist_message(
        &db.pool,
        Some(&room),
        session,
        true,
        7,
        &message,
        &[file.id],
    )
    .await
    .unwrap();
    assert_eq!(saved.attachments, vec![file.clone()]);
    let mut retry = message.clone();
    retry.message_id = Uuid::new_v4().to_string();
    let retried = crate::room::history::persist_message(
        &db.pool,
        Some(&room),
        session,
        true,
        7,
        &retry,
        &[file.id],
    )
    .await
    .unwrap();
    assert_eq!(saved.message_id, retried.message_id);
    assert!(
        crate::room::history::persist_message(&db.pool, Some(&room), session, true, 7, &retry, &[])
            .await
            .is_err()
    );
    retry.client_message_id = Uuid::new_v4().to_string();
    assert!(
        crate::room::history::persist_message(
            &db.pool,
            Some(&room),
            session,
            true,
            7,
            &retry,
            &[file.id]
        )
        .await
        .is_err()
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM chat_messages WHERE room_id=$1")
        .bind(&room)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(
        count, 1,
        "a failed attachment claim cannot insert an empty message"
    );
    crate::room::history::set_retention(&db.pool, &room, 1)
        .await
        .unwrap();
    let matched:bool=sqlx::query_scalar("SELECT a.expires_at=m.expires_at FROM attachments a JOIN chat_messages m ON m.id=a.message_id WHERE a.id=$1")
        .bind(file.id).fetch_one(&db.pool).await.unwrap();
    assert!(matched);
    let mut tx = db.pool.begin().await.unwrap();
    crate::room::history::remove_public(
        &mut tx,
        &room,
        &saved.message_id,
        &Utc::now().to_rfc3339(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert!(location(&db.pool, file.id).await.is_err());
    let remaining: i64 = sqlx::query_scalar("SELECT count(*) FROM attachments WHERE id=$1")
        .bind(file.id)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(remaining, 0);
    let second = db.upload(0, b"cascade attachment".to_vec()).await;
    crate::room::history::persist_message(
        &db.pool,
        Some(&room),
        session,
        true,
        1,
        &db.message(),
        &[second.id],
    )
    .await
    .unwrap();
    crate::room::history::set_retention(&db.pool, &room, 0)
        .await
        .unwrap();
    assert!(location(&db.pool, second.id).await.is_err());
    let pending = db.upload(0, b"pending attachment".to_vec()).await;
    sqlx::query("UPDATE attachments SET expires_at=now()-interval '1 second' WHERE id=$1")
        .bind(pending.id)
        .execute(&db.pool)
        .await
        .unwrap();
    let mut tx = db.pool.begin().await.unwrap();
    assert!(
        pending_metadata(&mut tx, db.users[0], &[pending.id])
            .await
            .is_err()
    );
    tx.rollback().await.unwrap();
    cleanup(&db.pool).await.unwrap();
    assert!(location(&db.pool, pending.id).await.is_err());
    db.close().await;
}

#[tokio::test]
#[ignore = "requires a migrated disposable TEST_DATABASE_URL"]
async fn database_attachments_enforce_logical_byte_quota_and_current_session() {
    let db = Database::new().await;
    for _ in 0..12 {
        db.upload(0, vec![1; MAX_FILE_BYTES]).await;
    }
    assert!(
        store_upload(
            &db.pool,
            &db.claims[0],
            "too much.bin".into(),
            vec![1; MAX_FILE_BYTES]
        )
        .await
        .is_err()
    );
    db.upload(1, b"independent account".to_vec()).await;
    sqlx::query("DELETE FROM sessions WHERE id=$1")
        .bind(db.claims[1].sid)
        .execute(&db.pool)
        .await
        .unwrap();
    assert!(
        store_upload(&db.pool, &db.claims[1], "revoked.txt".into(), vec![1])
            .await
            .is_err()
    );
    assert!(validate_account(&db.pool, &db.claims[1]).await.is_err());
    db.close().await;
}

#[tokio::test]
#[ignore = "requires a migrated disposable TEST_DATABASE_URL and mediasoup worker"]
async fn database_attachments_live_grants_recheck_membership_removal_and_private_visibility() {
    use crate::{
        media::config::MediaConfig, metrics::ServerMetrics, room::RoomManager,
        signaling::protocol::ClientMessage,
    };
    let mut db = Database::new().await;
    let room = db.room(0).await;
    let metrics = ServerMetrics::new();
    let mut config = MediaConfig::default();
    config.worker_config.num_workers = 1;
    config.webrtc_server_port_base = 0;
    let manager = Arc::new(
        RoomManager::new(config, metrics.clone(), Some(db.pool.clone()))
            .await
            .unwrap(),
    );
    let owner = db.users[0].to_string();
    let guest = Uuid::new_v4().to_string();
    let stranger = Uuid::new_v4().to_string();
    let mut senders = Vec::new();
    let mut receivers = Vec::new();
    for (id, authenticated) in [(&owner, true), (&guest, false), (&stranger, false)] {
        let (sender, receiver) = mpsc::channel(64);
        manager
            .add_participant(
                &room,
                id.clone(),
                "Reader".into(),
                sender.clone(),
                authenticated,
                Arc::new(std::sync::atomic::AtomicBool::new(false)),
                None,
                "attachment-test",
                None,
                None,
            )
            .await
            .unwrap();
        senders.push(sender);
        receivers.push(receiver);
    }
    let server =
        SignalingServer::new(manager.clone(), None, metrics, Some(db.pool.clone())).unwrap();
    let file = db.upload(0, b"live bytes".to_vec()).await;
    manager
        .handle_chat_command(
            &room,
            &owner,
            &senders[0],
            &ClientMessage::ChatMessage {
                content: String::new(),
                attachment_ids: vec![file.id],
                client_message_id: Some("live-file".into()),
                sequence: Some(1),
                reply_to: None,
            },
        )
        .await
        .unwrap();
    let location = location(&db.pool, file.id).await.unwrap();
    assert!(location.message_id.is_none());
    assert!(location.ephemeral_message_id.is_some());
    let access = grant_access(
        &manager,
        &db.pool,
        &room,
        &guest,
        &senders[1],
        None,
        file.id,
    )
    .await
    .unwrap();
    let mut headers = HeaderMap::new();
    headers.insert(
        "authorization",
        HeaderValue::from_str(&format!("Attachment {}", access.token)).unwrap(),
    );
    let (response, body) = download(State(server.clone()), headers.clone(), Path(file.id))
        .await
        .unwrap();
    assert_eq!(response["content-type"], "application/octet-stream");
    assert_eq!(
        to_bytes(body, MAX_FILE_BYTES).await.unwrap().as_ref(),
        b"live bytes"
    );
    let other = db.upload(0, b"unpublished".to_vec()).await;
    assert!(
        grant_access(
            &manager,
            &db.pool,
            &room,
            &guest,
            &senders[1],
            None,
            other.id
        )
        .await
        .is_err()
    );
    assert!(
        download(State(server.clone()), headers.clone(), Path(other.id))
            .await
            .is_err()
    );
    manager
        .remove_participant_for_sender(&room, &guest, &senders[1])
        .await
        .unwrap();
    let (replacement, replacement_receiver) = mpsc::channel(64);
    receivers.push(replacement_receiver);
    manager
        .add_participant(
            &room,
            guest.clone(),
            "Reader".into(),
            replacement.clone(),
            false,
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
            None,
            "attachment-rejoin",
            None,
            None,
        )
        .await
        .unwrap();
    assert!(
        download(State(server.clone()), headers.clone(), Path(file.id))
            .await
            .is_err()
    );
    assert!(
        grant_access(
            &manager,
            &db.pool,
            &room,
            &guest,
            &replacement,
            None,
            file.id
        )
        .await
        .is_err(),
        "retention-off rejoin cannot read prejoin files"
    );
    let owner_access = grant_access(
        &manager,
        &db.pool,
        &room,
        &owner,
        &senders[0],
        Some(&db.claims[0]),
        file.id,
    )
    .await
    .unwrap();
    let mut owner_headers = HeaderMap::new();
    owner_headers.insert(
        "authorization",
        HeaderValue::from_str(&format!("Attachment {}", owner_access.token)).unwrap(),
    );
    manager
        .handle_social_request(
            &room,
            &owner,
            &senders[0],
            &ClientMessage::RemoveChatMessage {
                request_id: "remove-file".into(),
                message_id: location.ephemeral_message_id.unwrap().to_string(),
            },
        )
        .await
        .unwrap();
    assert!(
        download(State(server.clone()), owner_headers, Path(file.id))
            .await
            .is_err()
    );
    let private = db.upload(0, b"private bytes".to_vec()).await;
    manager
        .handle_chat_command(
            &room,
            &owner,
            &senders[0],
            &ClientMessage::PrivateMessage {
                target_participant_id: guest.clone(),
                content: String::new(),
                attachment_ids: vec![private.id],
                client_message_id: "private-file".into(),
                sequence: Some(2),
                reply_to: None,
            },
        )
        .await
        .unwrap();
    assert!(
        grant_access(
            &manager,
            &db.pool,
            &room,
            &guest,
            &replacement,
            None,
            private.id
        )
        .await
        .is_ok()
    );
    assert!(
        grant_access(
            &manager,
            &db.pool,
            &room,
            &stranger,
            &senders[2],
            None,
            private.id
        )
        .await
        .is_err()
    );
    let own_access = grant_access(
        &manager,
        &db.pool,
        &room,
        &owner,
        &senders[0],
        Some(&db.claims[0]),
        private.id,
    )
    .await
    .unwrap();
    sqlx::query("DELETE FROM sessions WHERE id=$1")
        .bind(db.claims[0].sid)
        .execute(&db.pool)
        .await
        .unwrap();
    let mut headers = HeaderMap::new();
    headers.insert(
        "authorization",
        HeaderValue::from_str(&format!("Attachment {}", own_access.token)).unwrap(),
    );
    assert!(
        download(State(server), headers, Path(private.id))
            .await
            .is_err()
    );
    manager.shutdown().await.unwrap();
    drop(receivers);
    db.close().await;
}
