use super::*;
use crate::ids::fixtures::rid;
use crate::ids::{MessageId, ParticipantId};
use crate::signaling::fixtures::{account_server, bearer, server_with_unreachable_database};
use axum::response::IntoResponse;

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

/// The status a refused handler call answers with.
fn refusal<T>(result: Result<T, AuthError>) -> StatusCode {
    match result {
        Ok(_) => panic!("expected a refusal"),
        Err(error) => error.into_response().status(),
    }
}

/// An anonymous upload answers 401 without spending an upload permit, and
/// still answers 401, not 503, while the test holds all four permits:
/// authentication runs before admission. The server has no reachable
/// database, so the 401 also shows no query ran first.
#[tokio::test]
async fn uploads_authenticate_before_taking_a_slot() {
    let server = server_with_unreachable_database().await;
    let anonymous = || {
        upload(
            State(server.clone()),
            Request::post("/api/auth/attachments")
                .header("x-file-name", URL_SAFE_NO_PAD.encode("notes.txt"))
                .body(Body::from("anonymous bytes"))
                .unwrap(),
        )
    };
    assert_eq!(UPLOADS.available_permits(), 4);
    assert_eq!(refusal(anonymous().await), StatusCode::UNAUTHORIZED);
    assert_eq!(UPLOADS.available_permits(), 4);
    let held = UPLOADS.try_acquire_many(4).unwrap();
    assert_eq!(refusal(anonymous().await), StatusCode::UNAUTHORIZED);
    drop(held);
}

/// The download mirror: an anonymous download answers 401 without spending a
/// download permit, and still answers 401, not 503, while the test holds all
/// four permits. As for uploads, the 401 also shows no query ran first.
#[tokio::test]
async fn downloads_authenticate_before_taking_a_slot() {
    let server = server_with_unreachable_database().await;
    let anonymous = || {
        download(
            State(server.clone()),
            HeaderMap::new(),
            Path(Uuid::new_v4()),
        )
    };
    assert_eq!(DOWNLOADS.available_permits(), 4);
    assert_eq!(refusal(anonymous().await), StatusCode::UNAUTHORIZED);
    assert_eq!(DOWNLOADS.available_permits(), 4);
    let held = DOWNLOADS.try_acquire_many(4).unwrap();
    assert_eq!(refusal(anonymous().await), StatusCode::UNAUTHORIZED);
    drop(held);
}

/// One upload per account: a second reservation while the first slot lives
/// answers 429, another account is admitted, and dropping the slot, as a
/// cancelled upload drops its future, frees the account.
#[test]
fn one_upload_per_account_at_a_time() {
    let account = Uuid::new_v4();
    let first = reserve_account_upload(account).unwrap();
    assert_eq!(
        refusal(reserve_account_upload(account)),
        StatusCode::TOO_MANY_REQUESTS
    );
    let other = reserve_account_upload(Uuid::new_v4()).unwrap();
    drop(first);
    let again = reserve_account_upload(account).unwrap();
    drop(again);
    drop(other);
}

/// The quoted fallback keeps the ASCII letters, digits, space, `.`, `-` and
/// `_` of a stored name; `filename*` carries the exact UTF-8 name (RFC 8187).
#[test]
fn content_disposition_carries_a_utf8_filename() {
    assert_eq!(
        content_disposition("inline", "résumé 2026.png").unwrap(),
        "inline; filename=\"r_sum_ 2026.png\"; filename*=UTF-8''r%C3%A9sum%C3%A9%202026.png"
    );
}

/// APNG detection reads chunk types: the bytes `acTL` inside a `tEXt` chunk's
/// data leave a still PNG an image, an `acTL` chunk makes it an animation, an
/// `acTL` header whose body runs past the end still counts (failing closed),
/// and any other chunk running past the end stops the walk without a panic.
#[test]
fn apng_is_detected_by_chunk_type_only() {
    // A chunk is its length, type, data and a CRC the walk never checks.
    let chunk = |kind: &[u8; 4], data: &[u8]| {
        let mut bytes = u32::try_from(data.len()).unwrap().to_be_bytes().to_vec();
        bytes.extend_from_slice(kind);
        bytes.extend_from_slice(data);
        bytes.extend_from_slice(&[0; 4]);
        bytes
    };
    let png = |middle: Vec<u8>| {
        [
            b"\x89PNG\r\n\x1a\n".to_vec(),
            chunk(b"IHDR", &[0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0, 0, 0]),
            middle,
            chunk(b"IDAT", b"pixels"),
            chunk(b"IEND", b""),
        ]
        .concat()
    };
    let still = png(chunk(b"tEXt", b"Comment\0acTL"));
    let animated = png(chunk(b"acTL", &[0, 0, 0, 1, 0, 0, 0, 0]));
    assert!(!png_is_animated(&still));
    assert!(png_is_animated(&animated));
    assert_eq!(content_type(&still), "image/png");
    assert_eq!(content_type(&animated), "application/octet-stream");
    let mut hostile = u32::MAX.to_be_bytes().to_vec();
    hostile.extend_from_slice(b"tEXtacTL");
    assert!(!png_is_animated(&png(hostile)));
    // A declared `acTL` counts even when its body runs past the end: the walk
    // fails closed, and the file is served as a download.
    let mut declared = u32::MAX.to_be_bytes().to_vec();
    declared.extend_from_slice(b"acTL");
    assert!(png_is_animated(&png(declared)));
}

struct Database {
    pool: PgPool,
    users: Vec<Uuid>,
    principals: Vec<Principal>,
    /// A bearer header per user, for a token issued once through the session row.
    bearers: Vec<axum::http::HeaderMap>,
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
            principals: Vec::new(),
            bearers: Vec::new(),
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
            let principal = crate::auth::access_tokens::fixtures::principal(&db.pool, sid).await;
            db.principals.push(principal);
            db.bearers.push(bearer(&db.pool, sid).await);
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
        store_upload(
            &self.pool,
            &self.principals[owner],
            "notes.txt".into(),
            data,
        )
        .await
        .unwrap()
    }
    /// An upload request from user `owner` carrying `body`, with a bearer token
    /// that `account_server` accepts for that user's session.
    fn upload_request(&self, owner: usize, body: Body) -> Request {
        let mut request = Request::post("/api/auth/attachments")
            .header("x-file-name", URL_SAFE_NO_PAD.encode("notes.txt"))
            .body(body)
            .unwrap();
        request.headers_mut().extend(self.bearers[owner].clone());
        request
    }
    async fn owner_of(&self, id: Uuid) -> Uuid {
        sqlx::query_scalar("SELECT owner_id FROM attachments WHERE id=$1")
            .bind(id)
            .fetch_one(&self.pool)
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
            message_id: MessageId::random(),
            client_message_id: Uuid::new_v4().to_string(),
            participant_id: ParticipantId::from(self.users[0]),
            participant_name: "Attachment tester".into(),
            recipient_id: None,
            recipient_name: None,
            content: String::new(),
            attachments: Vec::new(),
            sent_at: Utc::now(),
            removed_at: None,
            revision: 0,
            edited_at: None,
            chat_style: Default::default(),
            reply_to: None,
            account_conversation: None,
            reactions: Vec::new(),
        }
    }
}

#[tokio::test]
#[ignore = "requires a migrated disposable TEST_DATABASE_URL"]
async fn database_attachments_claim_atomically_require_owner_and_preserve_retry_identity() {
    let mut db = Database::new().await;
    let room = rid(&db.room(7).await);
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
        Some(room.as_str()),
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
    retry.message_id = MessageId::random();
    let retried = crate::room::history::persist_message(
        &db.pool,
        Some(room.as_str()),
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
        crate::room::history::persist_message(
            &db.pool,
            Some(room.as_str()),
            session,
            true,
            7,
            &retry,
            &[]
        )
        .await
        .is_err()
    );
    retry.client_message_id = Uuid::new_v4().to_string();
    assert!(
        crate::room::history::persist_message(
            &db.pool,
            Some(room.as_str()),
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
    crate::room::history::set_retention(&db.pool, room.as_str(), 1)
        .await
        .unwrap();
    let matched:bool=sqlx::query_scalar("SELECT a.expires_at=m.expires_at FROM attachments a JOIN chat_messages m ON m.id=a.message_id WHERE a.id=$1")
        .bind(file.id).fetch_one(&db.pool).await.unwrap();
    assert!(matched);
    let mut tx = db.pool.begin().await.unwrap();
    crate::room::history::remove_public(&mut tx, room.as_str(), &saved.message_id, Utc::now())
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
        Some(room.as_str()),
        session,
        true,
        1,
        &db.message(),
        &[second.id],
    )
    .await
    .unwrap();
    crate::room::history::set_retention(&db.pool, room.as_str(), 0)
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
            &db.principals[0],
            "too much.bin".into(),
            vec![1; MAX_FILE_BYTES]
        )
        .await
        .is_err()
    );
    db.upload(1, b"independent account".to_vec()).await;
    sqlx::query("DELETE FROM sessions WHERE id=$1")
        .bind(db.principals[1].sid)
        .execute(&db.pool)
        .await
        .unwrap();
    assert!(
        store_upload(&db.pool, &db.principals[1], "revoked.txt".into(), vec![1])
            .await
            .is_err()
    );
    assert!(validate_account(&db.pool, &db.principals[1]).await.is_err());
    db.close().await;
}

/// The upload handler consults the account's slot before the upload permit:
/// with user 0's slot held and all four permits taken, user 0's upload answers
/// 429, not 503; with the slot free and the permits still taken, 503; with
/// the permits free, user 1's upload is stored while user 0's slot is held.
/// This fails if `upload` stops calling `reserve_account_upload` or calls it
/// after taking the permit.
#[tokio::test]
#[ignore = "requires a migrated disposable TEST_DATABASE_URL"]
async fn upload_consults_the_account_slot_before_the_permit() {
    let db = Database::new().await;
    let server = account_server(db.pool.clone()).await;
    let request = |owner: usize| db.upload_request(owner, Body::from("slot test bytes"));
    let slot = reserve_account_upload(db.users[0]).unwrap();
    let permits = UPLOADS.try_acquire_many(4).unwrap();
    assert_eq!(
        refusal(upload(State(server.clone()), request(0)).await),
        StatusCode::TOO_MANY_REQUESTS
    );
    drop(slot);
    assert_eq!(
        refusal(upload(State(server.clone()), request(0)).await),
        StatusCode::SERVICE_UNAVAILABLE
    );
    drop(permits);
    let slot = reserve_account_upload(db.users[0]).unwrap();
    let (_, Json(stored)) = upload(State(server), request(1)).await.unwrap();
    drop(slot);
    assert_eq!(db.owner_of(stored.id).await, db.users[1]);
    db.close().await;
}

/// The upload handler holds the account's slot for the whole upload: while
/// user 0's body is still arriving, another upload from user 0 answers 429 and
/// user 1's upload is stored; dropping the pending request, as when the client
/// goes away mid-body, frees user 0's slot, and so does a completed upload.
/// This fails if `upload` lets the slot go early, for example by binding it to
/// `_`.
#[tokio::test]
#[ignore = "requires a migrated disposable TEST_DATABASE_URL"]
async fn upload_holds_the_account_slot_until_the_body_is_read_or_the_request_is_dropped() {
    let db = Database::new().await;
    let server = account_server(db.pool.clone()).await;
    // A body that stays pending until the test sends into it or drops the sender.
    let pending_body = || {
        let (sender, receiver) = mpsc::channel::<Bytes>(1);
        let chunks = futures_util::stream::unfold(receiver, |mut receiver| async move {
            let chunk = receiver.recv().await?;
            Some((Ok::<_, std::convert::Infallible>(chunk), receiver))
        });
        (sender, Body::from_stream(chunks))
    };
    // A spawned upload takes the account slot, then an upload permit, and holds
    // both while it waits for its body; wait at most ten seconds for the permit.
    let reading_body = || async {
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while UPLOADS.available_permits() == 4 {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .is_ok()
    };
    let holds_slot = || ACCOUNT_UPLOADS.lock().contains(&db.users[0]);

    let (sender, body) = pending_body();
    let dropped = tokio::spawn(upload(State(server.clone()), db.upload_request(0, body)));
    assert!(reading_body().await, "the upload never reached its body");
    let second = db.upload_request(0, Body::from("second upload"));
    assert_eq!(
        refusal(upload(State(server.clone()), second).await),
        StatusCode::TOO_MANY_REQUESTS
    );
    let other = db.upload_request(1, Body::from("another account"));
    let (_, Json(other)) = upload(State(server.clone()), other).await.unwrap();
    assert_eq!(db.owner_of(other.id).await, db.users[1]);
    assert!(holds_slot());
    dropped.abort();
    assert!(dropped.await.unwrap_err().is_cancelled());
    drop(sender);
    assert!(!holds_slot(), "dropping the request frees the slot");
    assert_eq!(UPLOADS.available_permits(), 4);

    let (sender, body) = pending_body();
    let completed = tokio::spawn(upload(State(server.clone()), db.upload_request(0, body)));
    assert!(reading_body().await, "the upload never reached its body");
    assert!(holds_slot());
    sender
        .send(Bytes::from_static(b"completed upload"))
        .await
        .unwrap();
    drop(sender);
    let (_, Json(stored)) = completed.await.unwrap().unwrap();
    assert_eq!(db.owner_of(stored.id).await, db.users[0]);
    assert!(!holds_slot(), "a completed upload frees the slot");
    assert_eq!(UPLOADS.available_permits(), 4);
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
    let room = rid(&db.room(0).await);
    let metrics = ServerMetrics::new();
    let mut config = MediaConfig::default();
    config.worker_config.num_workers = 1;
    config.webrtc_server_port_base = 0;
    let manager = Arc::new(
        RoomManager::new(
            crate::room::ManagerSettings::default(),
            config,
            metrics.clone(),
            Some(db.pool.clone()),
        )
        .await
        .unwrap(),
    );
    let owner = ParticipantId::from(db.users[0]);
    let guest = ParticipantId::random();
    let stranger = ParticipantId::random();
    let mut senders = Vec::new();
    let mut receivers = Vec::new();
    for (id, authenticated) in [(&owner, true), (&guest, false), (&stranger, false)] {
        let (sender, receiver) = mpsc::channel(64);
        manager
            .add_participant(
                &room,
                *id,
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
    let server = SignalingServer::new(
        manager.clone(),
        metrics,
        Some(db.pool.clone()),
        &crate::config::Config::for_tests(),
    )
    .unwrap();
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
        server.attachment_grant_key(),
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
        HeaderValue::from_str(&format!("Attachment {}", access.token.expose_secret())).unwrap(),
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
            server.attachment_grant_key(),
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
            guest,
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
            server.attachment_grant_key(),
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
        server.attachment_grant_key(),
        &manager,
        &db.pool,
        &room,
        &owner,
        &senders[0],
        Some(&db.principals[0]),
        file.id,
    )
    .await
    .unwrap();
    let mut owner_headers = HeaderMap::new();
    owner_headers.insert(
        "authorization",
        HeaderValue::from_str(&format!(
            "Attachment {}",
            owner_access.token.expose_secret()
        ))
        .unwrap(),
    );
    manager
        .handle_social_request(
            &room,
            &owner,
            &senders[0],
            &ClientMessage::RemoveChatMessage {
                request_id: "remove-file".into(),
                message_id: MessageId::from(location.ephemeral_message_id.unwrap()),
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
                target_participant_id: guest,
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
            server.attachment_grant_key(),
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
            server.attachment_grant_key(),
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
        server.attachment_grant_key(),
        &manager,
        &db.pool,
        &room,
        &owner,
        &senders[0],
        Some(&db.principals[0]),
        private.id,
    )
    .await
    .unwrap();
    sqlx::query("DELETE FROM sessions WHERE id=$1")
        .bind(db.principals[0].sid)
        .execute(&db.pool)
        .await
        .unwrap();
    let mut headers = HeaderMap::new();
    headers.insert(
        "authorization",
        HeaderValue::from_str(&format!("Attachment {}", own_access.token.expose_secret())).unwrap(),
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
