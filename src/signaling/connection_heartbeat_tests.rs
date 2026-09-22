//! Real loopback WebSockets exercise the production connection handler with a
//! short private timing policy; no browser or public service is used. The one
//! ignored authentication fixture requires the existing disposable test database.

use super::*;
use axum::{Router, extract::ws::WebSocketUpgrade, routing::get};
use tokio::sync::{Semaphore, broadcast};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, tungstenite::Message as PeerMessage};

const TEST_IDLE: Duration = Duration::from_millis(500);
type Peer = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

pub(super) struct Fixture {
    peer: Peer,
    manager: Arc<RoomManager>,
    metrics: ServerMetrics,
    grace: GracePeriodMap,
    completed: mpsc::Receiver<()>,
    did_complete: bool,
    server: OwnedTask,
    _revocations: broadcast::Sender<(String, i64)>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.manager.drain_signal().begin_draining();
        self.grace.close();
    }
}

impl Fixture {
    pub(super) async fn new() -> Self {
        Self::with_claims(None).await
    }

    async fn with_claims(claims: Option<Claims>) -> Self {
        Self::with_authentication(claims, None, None).await
    }

    async fn with_authentication(
        claims: Option<Claims>,
        pool: Option<sqlx::PgPool>,
        renewal_authenticator: Option<RenewalAuthenticator>,
    ) -> Self {
        let metrics = ServerMetrics::new();
        let manager = Arc::new(RoomManager::new_for_connection_tests(metrics.clone()).await);
        let grace = GracePeriodMap::new();
        let (completed_tx, completed) = mpsc::channel(1);
        let (revocations, _) = broadcast::channel(4);
        let permits = Arc::new(Semaphore::new(1));
        let router = Router::new().route(
            "/ws",
            get({
                let manager = manager.clone();
                let metrics = metrics.clone();
                let grace = grace.clone();
                let revocations = revocations.clone();
                move |upgrade: WebSocketUpgrade| {
                    let claims = claims.clone();
                    let pool = pool.clone();
                    let renewal_authenticator = renewal_authenticator.clone();
                    let manager = manager.clone();
                    let metrics = metrics.clone();
                    let grace = grace.clone();
                    let completed_tx = completed_tx.clone();
                    let revocations = revocations.subscribe();
                    let permits = permits.clone();
                    async move {
                        upgrade.on_upgrade(move |socket| async move {
                            handle_connection_with_timing(
                                socket,
                                manager,
                                None,
                                grace,
                                metrics,
                                permits.acquire_owned().await.unwrap(),
                                claims,
                                None,
                                pool,
                                revocations,
                                renewal_authenticator,
                                ConnectionTiming {
                                    idle_timeout: TEST_IDLE,
                                    heartbeat_interval: Duration::from_millis(75),
                                    membership_timeout: Duration::from_millis(25),
                                },
                            )
                            .await;
                            let _ = completed_tx.send(()).await;
                        })
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let server = OwnedTask(tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        }));
        let (peer, _) = tokio_tungstenite::connect_async(format!("ws://{address}/ws"))
            .await
            .unwrap();
        Self {
            peer,
            manager,
            metrics,
            grace,
            completed,
            did_complete: false,
            server,
            _revocations: revocations,
        }
    }

    pub(super) async fn join(&mut self) -> anyhow::Result<serde_json::Value> {
        self.peer
            .send(PeerMessage::Text(
                serde_json::json!({
                    "type": "joinRoom", "roomId": "heartbeat-test", "participantName": "Quiet"
                })
                .to_string()
                .into(),
            ))
            .await?;
        tokio::time::timeout(Duration::from_secs(3), async {
            while let Some(message) = self.peer.next().await {
                if let PeerMessage::Text(text) = message? {
                    let value: serde_json::Value = serde_json::from_str(&text)?;
                    if value["type"] == "roomJoined" || value["type"] == "lobbyWaiting" {
                        return Ok(value);
                    }
                    anyhow::ensure!(value["type"] != "error", "join rejected");
                }
            }
            anyhow::bail!("connection closed before join")
        })
        .await?
    }

    pub(super) async fn request(
        &mut self,
        message: serde_json::Value,
        response_type: &str,
    ) -> anyhow::Result<serde_json::Value> {
        self.peer
            .send(PeerMessage::Text(message.to_string().into()))
            .await?;
        tokio::time::timeout(Duration::from_secs(3), async {
            while let Some(message) = self.peer.next().await {
                if let PeerMessage::Text(text) = message? {
                    let value: serde_json::Value = serde_json::from_str(&text)?;
                    if value["type"] == response_type {
                        return Ok(value);
                    }
                    anyhow::ensure!(
                        value["type"] != "error"
                            && value["type"] != "socialError"
                            && value["type"] != "authenticationRenewalFailed",
                        "fixture request rejected",
                    );
                }
            }
            anyhow::bail!("connection closed before fixture response")
        })
        .await?
    }

    pub(super) async fn next_message(&mut self) -> anyhow::Result<serde_json::Value> {
        tokio::time::timeout(Duration::from_secs(3), async {
            while let Some(message) = self.peer.next().await {
                if let PeerMessage::Text(text) = message? {
                    return Ok(serde_json::from_str(&text)?);
                }
            }
            anyhow::bail!("connection closed before fixture event")
        })
        .await?
    }

    /// Polling tungstenite drives its automatic protocol Pong, as browsers do.
    /// No application request is sent during this observation window.
    async fn observe_alive(&mut self, duration: Duration) -> anyhow::Result<usize> {
        let deadline = tokio::time::Instant::now() + duration;
        let mut pings = 0;
        loop {
            tokio::select! {
                biased;
                _ = tokio::time::sleep_until(deadline) => return Ok(pings),
                message = self.peer.next() => match message {
                    Some(Ok(PeerMessage::Ping(_))) => pings += 1,
                    Some(Ok(PeerMessage::Close(_))) | None => anyhow::bail!("quiet connection closed"),
                    Some(Err(error)) => return Err(error.into()),
                    Some(Ok(_)) => {},
                },
            }
        }
    }

    async fn wait_for_completion(&mut self) -> bool {
        if !self.did_complete {
            self.did_complete = matches!(
                tokio::time::timeout(TEST_IDLE * 3, self.completed.recv()).await,
                Ok(Some(()))
            );
        }
        self.did_complete
    }

    pub(super) async fn finish(mut self) {
        self.manager.drain_signal().begin_draining();
        self.grace.close();
        assert!(
            self.wait_for_completion().await,
            "connection task must settle"
        );
        self.manager.shutdown().await.unwrap();
        self.manager.media_server().shutdown().await.unwrap();
        self.server.abort();
        let _ = (&mut self.server.0).await;
        assert!(
            self.metrics
                .render_prometheus(0, 0, 0)
                .contains("simplestchat_connections_active 0\n")
        );
    }
}

#[tokio::test]
async fn quiet_joined_socket_survives_multiple_idle_windows() {
    let mut fixture = Fixture::new().await;
    fixture.join().await.unwrap();
    let observed = fixture.observe_alive(TEST_IDLE * 3).await;
    fixture.finish().await;
    assert!(
        observed.is_ok(),
        "a responsive joined socket must remain live: {observed:?}"
    );
    assert!(
        observed.unwrap() >= 3,
        "server must send protocol heartbeats"
    );
}

#[tokio::test]
async fn quiet_lobby_socket_survives_without_gaining_room_admission() {
    let mut fixture = Fixture::new().await;
    let (owner_sender, _owner_receiver) = mpsc::channel(16);
    fixture
        .manager
        .add_participant(
            "heartbeat-test",
            Uuid::new_v4().to_string(),
            "Owner".into(),
            owner_sender,
            false,
            Arc::new(AtomicBool::new(false)),
            None,
            "test-owner-token",
            None,
        )
        .await
        .unwrap();
    let room = fixture.manager.room_for_connection_tests("heartbeat-test");
    room.write().await.settings = Some(settings::RoomSettings {
        id: "heartbeat-test".into(),
        owner_id: Uuid::nil(),
        display_name: "Heartbeat fixture".into(),
        password_protected: false,
        require_registration: false,
        max_participants: None,
        max_broadcasters: None,
        allow_screen_sharing: true,
        allow_chat: true,
        allow_video: true,
        moderated: false,
        invite_only: false,
        secret: false,
        lobby_enabled: true,
        push_to_talk: false,
        guests_allowed: true,
        guests_can_broadcast: true,
        topic: None,
    });
    let joined = fixture.join().await.unwrap();
    let observed = fixture.observe_alive(TEST_IDLE * 3).await;
    let room_state = room.read().await;
    let membership = (room_state.participants.len(), room_state.lobby.len());
    drop(room_state);
    fixture.finish().await;
    assert_eq!(joined["type"], "lobbyWaiting");
    assert!(
        observed.is_ok(),
        "waiting lobby members remain live: {observed:?}"
    );
    assert!(observed.unwrap() >= 3);
    assert_eq!(membership, (1, 1), "heartbeat cannot admit a lobby member");
}

#[test]
fn production_liveness_policy_remains_bounded_and_fixed() {
    let timing = ConnectionTiming::default();
    assert_eq!(timing.idle_timeout, Duration::from_secs(300));
    assert_eq!(timing.heartbeat_interval, Duration::from_secs(30));
    assert!(timing.membership_timeout < timing.heartbeat_interval);
}

#[tokio::test]
async fn unjoined_socket_receives_no_heartbeat_and_expires() {
    let mut fixture = Fixture::new().await;
    let first = tokio::time::timeout(TEST_IDLE * 3, fixture.peer.next()).await;
    fixture.finish().await;
    assert!(matches!(first, Ok(None | Some(Err(_)))));
}

#[tokio::test]
async fn outbound_heartbeats_do_not_keep_a_nonresponding_peer_alive() {
    let mut fixture = Fixture::new().await;
    fixture.join().await.unwrap();
    // Deliberately do not poll the peer: tungstenite cannot send an automatic
    // Pong until it reads Ping. Server writes alone must not renew its lease.
    let completed = fixture.wait_for_completion().await;
    let counters = fixture.metrics.render_prometheus(0, 0, 0);
    fixture.finish().await;
    assert!(completed, "unanswered heartbeats must not renew idle time");
    assert!(counters.contains("simplestchat_messages_received_total 1\n"));
}

#[tokio::test]
async fn leaving_room_stops_heartbeat_renewal() {
    let mut fixture = Fixture::new().await;
    fixture.join().await.unwrap();
    fixture
        .peer
        .send(PeerMessage::Text(r#"{"type":"leaveRoom"}"#.into()))
        .await
        .unwrap();
    let observed = fixture.observe_alive(TEST_IDLE * 3).await;
    let remaining = fixture.manager.total_participant_count().await;
    fixture.finish().await;
    assert!(
        observed.is_err(),
        "left sockets retain the ordinary idle cutoff"
    );
    assert_eq!(remaining, 0);
}

#[tokio::test]
async fn authoritative_removal_stops_heartbeat_despite_stale_local_room() {
    let mut fixture = Fixture::new().await;
    let joined = fixture.join().await.unwrap();
    fixture
        .manager
        .remove_participant("heartbeat-test", joined["participantId"].as_str().unwrap())
        .await
        .unwrap();
    let observed = fixture.observe_alive(TEST_IDLE * 3).await;
    fixture.finish().await;
    assert!(
        observed.is_err(),
        "removed membership cannot grant heartbeats"
    );
}

#[tokio::test]
async fn sender_rebind_stops_heartbeat_for_the_superseded_socket() {
    let mut fixture = Fixture::new().await;
    let joined = fixture.join().await.unwrap();
    let id = joined["participantId"].as_str().unwrap();
    let room = fixture.manager.room_for_connection_tests("heartbeat-test");
    let old_sender = room.read().await.participants[id].sender.clone();
    let (new_sender, _new_receiver) = mpsc::channel(16);
    assert!(
        fixture
            .manager
            .rebind_participant_sender("heartbeat-test", id, None, &old_sender, new_sender.clone())
            .await
            .unwrap()
    );
    let observed = fixture.observe_alive(TEST_IDLE * 3).await;
    let still_bound = fixture
        .manager
        .is_bound_participant("heartbeat-test", id, &new_sender)
        .await;
    fixture.finish().await;
    assert!(observed.is_err(), "a stale sender cannot grant heartbeats");
    assert!(
        still_bound,
        "old socket cleanup must preserve the rebound sender"
    );
}

#[tokio::test]
async fn membership_lock_contention_does_not_extend_the_idle_deadline() {
    let mut fixture = Fixture::new().await;
    fixture.join().await.unwrap();
    let room = fixture.manager.room_for_connection_tests("heartbeat-test");
    let held = room.write().await;
    let observed = fixture.observe_alive(TEST_IDLE * 3).await;
    drop(held);
    fixture.finish().await;
    assert!(
        observed.is_err(),
        "membership lookup must have a short deadline"
    );
}

#[tokio::test]
async fn drain_finishes_while_room_membership_is_locked() {
    let mut fixture = Fixture::new().await;
    fixture.join().await.unwrap();
    let room = fixture.manager.room_for_connection_tests("heartbeat-test");
    let held = room.write().await;
    fixture.manager.drain_signal().begin_draining();
    let completed = fixture.wait_for_completion().await;
    drop(held);
    fixture.finish().await;
    assert!(completed, "drain must not wait for the membership lock");
}

#[tokio::test]
async fn heartbeat_control_writes_do_not_change_application_write_counters() {
    let metrics = ServerMetrics::new();
    let mut successful = futures_util::sink::drain();
    write_message(
        &mut successful,
        Message::Ping(bytes::Bytes::new()),
        &metrics,
        None,
        tokio::time::Instant::now() + Duration::from_secs(1),
        OperationKind::SocketWrite,
    )
    .await
    .unwrap();
    let mut failed = Box::pin(futures_util::sink::unfold((), |(), _: Message| async {
        Err::<(), ()>(())
    }));
    assert!(
        write_message(
            &mut failed,
            Message::Ping(bytes::Bytes::new()),
            &metrics,
            None,
            tokio::time::Instant::now() + Duration::from_secs(1),
            OperationKind::SocketWrite
        )
        .await
        .is_err()
    );
    let mut stalled = Box::pin(futures_util::sink::unfold((), |(), _: Message| {
        std::future::pending::<Result<(), ()>>()
    }));
    assert!(
        write_message(
            &mut stalled,
            Message::Ping(bytes::Bytes::new()),
            &metrics,
            None,
            tokio::time::Instant::now(),
            OperationKind::SocketWrite
        )
        .await
        .is_err()
    );
    let counters = metrics.render_prometheus(0, 0, 0);
    assert!(counters.contains("simplestchat_messages_sent_total 0\n"));
    assert!(counters.contains("simplestchat_message_send_failed_total 0\n"));
}

#[tokio::test]
async fn expired_or_unverifiable_account_cannot_receive_heartbeats() {
    for exp in [0, usize::MAX] {
        let mut fixture = Fixture::with_claims(Some(Claims {
            sub: Uuid::new_v4().to_string(),
            name: "Account".into(),
            iss: "test".into(),
            aud: "test".into(),
            exp,
            auth_version: 0,
        }))
        .await;
        let first = tokio::time::timeout(TEST_IDLE, fixture.peer.next()).await;
        fixture.finish().await;
        assert!(
            matches!(first, Ok(None | Some(Err(_)))),
            "credential checks precede heartbeat work"
        );
    }
}

#[tokio::test]
async fn renewal_transport_loss_and_drain_do_not_invalidate_credentials() {
    let fixture = Fixture::new().await;
    let secret = "interrupted-renewal-fixture-secret-at-least-32-bytes";
    let token =
        crate::auth::jwt::create_token(&Uuid::new_v4().to_string(), "Renewal fixture", secret)
            .unwrap();
    let claims = crate::auth::jwt::validate_token(&token, secret).unwrap();
    let token = serde_json::from_value(serde_json::json!(token)).unwrap();
    let authenticator = RenewalAuthenticator::new(secret.into(), Arc::new(Semaphore::new(1)));
    let (_notices, mut revocations) = broadcast::channel(4);
    let (closed_sender, closed_receiver) = mpsc::channel(1);
    drop(closed_receiver);
    let drain = fixture.manager.drain_signal();
    let lost_transport = renew_authentication(
        &authenticator,
        &claims,
        &token,
        None,
        &mut revocations,
        &drain,
        &closed_sender,
    )
    .await;
    let (live_sender, _live_receiver) = mpsc::channel(1);
    drain.begin_draining();
    let draining = renew_authentication(
        &authenticator,
        &claims,
        &token,
        None,
        &mut revocations,
        &drain,
        &live_sender,
    )
    .await;
    fixture.finish().await;
    assert!(matches!(lost_transport, RenewalOutcome::Interrupted));
    assert!(matches!(draining, RenewalOutcome::Interrupted));
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL pointing to a migrated disposable PostgreSQL database"]
async fn authenticated_renewal_keeps_socket_and_membership_across_original_expiry() {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect(&std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL"))
        .await
        .unwrap();
    let email = format!("renewal-{}@example.test", Uuid::new_v4());
    let account_id: Uuid = sqlx::query_scalar(
        "INSERT INTO users (email, display_name, password_hash) VALUES ($1, 'Renewal fixture', 'unused-fixture-hash') RETURNING id",
    ).bind(email).fetch_one(&pool).await.unwrap();
    let secret = "same-socket-renewal-fixture-secret-at-least-32-bytes";
    let mut original = crate::auth::jwt::validate_token(
        &crate::auth::jwt::create_token(&account_id.to_string(), "Renewal fixture", secret)
            .unwrap(),
        secret,
    )
    .unwrap();
    // Only this private handler fixture uses a short original credential. The
    // renewal passes the production JWT verifier and database account check.
    original.exp = unix_seconds() as usize + 6;
    let original_expiry = original.exp as u64;
    let mut fixture = Fixture::with_authentication(
        Some(original),
        Some(pool.clone()),
        Some(RenewalAuthenticator::new(
            secret.into(),
            Arc::new(Semaphore::new(1)),
        )),
    )
    .await;
    let observed =
        async {
            let joined = fixture.join().await?;
            anyhow::ensure!(joined["type"] == "roomJoined", "fixture was not admitted");
            let participant_id = account_id.to_string();
            let room = fixture.manager.room_for_connection_tests("heartbeat-test");
            let before = room.read().await.participants[&participant_id].clone();
            let token = crate::auth::jwt::create_token(&participant_id, "Renewal fixture", secret)
                .map_err(|_| anyhow::anyhow!("fixture token could not be issued"))?;
            let renewed = fixture.request(serde_json::json!({
            "type": "renewAuthentication", "requestId": "renewal-fixture-1", "token": token,
        }), "authenticationRenewed").await?;
            anyhow::ensure!(
                renewed["requestId"] == "renewal-fixture-1",
                "correlation changed"
            );
            anyhow::ensure!(
                renewed["expiresAt"]
                    .as_u64()
                    .is_some_and(|exp| exp > original_expiry),
                "expiry did not advance"
            );
            fixture
                .observe_alive(Duration::from_secs(
                    original_expiry.saturating_sub(unix_seconds()) + 1,
                ))
                .await?;
            let after = room.read().await.participants[&participant_id].clone();
            anyhow::ensure!(
                before.media_session_id == after.media_session_id,
                "membership was replaced"
            );
            anyhow::ensure!(
                before.sender.same_channel(&after.sender),
                "socket ownership changed"
            );
            anyhow::ensure!(
                before.role == after.role && after.authenticated,
                "membership identity changed"
            );
            anyhow::ensure!(
                fixture.grace.inner.read().unwrap().is_empty(),
                "renewal entered reconnect grace"
            );
            let snapshot = fixture
                .request(
                    serde_json::json!({
                        "type": "getRoomSnapshot", "requestId": "after-original-expiry",
                    }),
                    "socialResponse",
                )
                .await?;
            anyhow::ensure!(
                snapshot["requestId"] == "after-original-expiry",
                "post-expiry signaling failed"
            );
            anyhow::ensure!(
                snapshot["action"] == "getRoomSnapshot",
                "snapshot action changed"
            );
            anyhow::ensure!(
                fixture
                    .metrics
                    .render_prometheus(0, 0, 0)
                    .contains("simplestchat_connections_active 1\n"),
                "connection was replaced"
            );
            Ok::<(), anyhow::Error>(())
        }
        .await;
    fixture.finish().await;
    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(account_id)
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
    assert!(
        observed.is_ok(),
        "same-socket renewal must preserve live membership: {observed:?}"
    );
}
