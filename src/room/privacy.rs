//! Publish current account privacy into active memberships after an owned save.
//!
//! Publication reads durable state only after acquiring each room's control
//! gate, so a delayed older HTTP completion cannot overwrite a newer policy.
#![forbid(unsafe_code)]

use super::{Room, RoomManager, TokioRwLock, control};
use crate::auth::account::{ChatPreferences, load_chat_preferences};
use crate::ids::ParticipantId;
use futures_util::{StreamExt, stream};
use sqlx::PgPool;
use std::{future::Future, sync::Arc, time::Duration};
use uuid::Uuid;

const SYNC_TIMEOUT: Duration = Duration::from_secs(15);
const SYNC_CONCURRENCY: usize = 8;

async fn refresh_live_room<F, W, C>(
    room: &Arc<TokioRwLock<Room>>,
    account: Uuid,
    current: C,
    read: F,
) -> anyhow::Result<()>
where
    F: FnOnce() -> W,
    W: Future<Output = Result<ChatPreferences, sqlx::Error>>,
    C: Fn() -> bool,
{
    let _control = tokio::time::timeout(control::ADMISSION_TIMEOUT, control::lock_room(room))
        .await
        .map_err(|_| anyhow::anyhow!("Privacy synchronization is waiting on a busy room"))?;
    let account_id = ParticipantId::from(account);
    {
        let state = room.read().await;
        if !current() {
            return Ok(());
        }
        state.ensure_live()?;
        if !state
            .participants
            .values()
            .any(|person| person.authenticated && person.id == account_id)
        {
            return Ok(());
        }
    }
    // Never accept the initiating request's snapshot here. Concurrent account
    // writes commit independently of a room; the room gate orders fresh reads.
    let preferences = tokio::time::timeout(control::PERSISTENCE_TIMEOUT, read())
        .await
        .map_err(|_| anyhow::anyhow!("Current account privacy could not be read"))??;
    let mut state = room.write().await;
    if !current() {
        return Ok(());
    }
    state.ensure_live()?;
    for person in state.participants.values_mut() {
        if person.authenticated && person.id == account_id {
            person.social.apply_account_preferences(&preferences);
        }
    }
    Ok(())
}

impl RoomManager {
    /// Caller holds the destination room's control gate, without its state lock.
    /// The room is already globally registered before this read starts, so a
    /// concurrent save either publishes after insertion or this read sees it.
    pub(super) async fn membership_chat_preferences(
        &self,
        authenticated: bool,
        participant_id: &ParticipantId,
    ) -> anyhow::Result<Option<ChatPreferences>> {
        if !authenticated {
            return Ok(None);
        }
        let account = participant_id.as_uuid();
        let pool = self
            .db_pool
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Account privacy could not be verified"))?;
        let preferences = tokio::time::timeout(
            control::PERSISTENCE_TIMEOUT,
            load_chat_preferences(pool, account),
        )
        .await
        .map_err(|_| anyhow::anyhow!("Account privacy could not be verified"))?
        .map_err(|_| anyhow::anyhow!("Account privacy could not be verified"))?;
        Ok(Some(preferences))
    }

    /// Called by the owned preference-save task after COMMIT. Unreadable or
    /// timed-out rooms retain their existing policy and make the save response
    /// explicitly report incomplete synchronization rather than claim success.
    pub(crate) async fn refresh_account_preferences(&self, account: Uuid) -> anyhow::Result<()> {
        let pool: PgPool = self
            .db_pool
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Account privacy requires a database"))?
            .clone();
        let account_id = ParticipantId::from(account);
        let rooms: Vec<_> = self
            .rooms
            .read()
            .iter()
            .filter(|(_, room)| {
                room.try_read().map_or(true, |state| {
                    state
                        .participants
                        .values()
                        .any(|person| person.authenticated && person.id == account_id)
                })
            })
            .map(|(id, room)| (id.clone(), room.clone()))
            .collect();
        let synchronize = async {
            let mut work = stream::iter(rooms)
                .map(|(id, room)| {
                    let pool = pool.clone();
                    async move {
                        refresh_live_room(
                            &room,
                            account,
                            || {
                                self.rooms
                                    .read()
                                    .get(&id)
                                    .is_some_and(|live| Arc::ptr_eq(live, &room))
                            },
                            || load_chat_preferences(&pool, account),
                        )
                        .await
                    }
                })
                .buffer_unordered(SYNC_CONCURRENCY);
            let mut failed = 0;
            while let Some(result) = work.next().await {
                if result.is_err() {
                    failed += 1;
                }
            }
            anyhow::ensure!(
                failed == 0,
                "Account privacy did not synchronize to {failed} rooms"
            );
            Ok(())
        };
        let result = tokio::time::timeout(SYNC_TIMEOUT, synchronize)
            .await
            .map_err(|_| anyhow::anyhow!("Account privacy synchronization deadline exceeded"))?;
        if result.is_err() {
            tracing::warn!("Saved account privacy could not be synchronized to every active room");
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::fixtures::{pid, rid};
    use crate::{
        room::{Participant, moderation, roles, social},
        signaling::protocol::ChatStyle,
    };
    use std::sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    };
    use tokio::sync::{mpsc, oneshot};

    fn fixture(account: Uuid) -> Arc<TokioRwLock<Room>> {
        let mut room = Room::new(rid("privacy-test"), None, false, None);
        let (sender, _receiver) = mpsc::channel(1);
        room.participants.insert(
            ParticipantId::from(account),
            Participant {
                id: ParticipantId::from(account),
                name: "Privacy policy fixture".into(),
                social: social::ParticipantSocial::new(0),
                sender,
                media_session_id: Uuid::new_v4(),
                producers: Default::default(),
                role: roles::Role::User,
                punitive: moderation::PunitiveState::default(),
                authenticated: true,
                ip: None,
                chat_style: ChatStyle::default(),
            },
        );
        Arc::new(TokioRwLock::new(room))
    }

    #[tokio::test]
    async fn queued_publication_reads_the_latest_policy_after_room_control() {
        let account = Uuid::new_v4();
        let room = fixture(account);
        let held = control::lock_room(&room).await;
        let reads = Arc::new(AtomicUsize::new(0));
        let current_policy = Arc::new(Mutex::new(ChatPreferences::default()));
        let queued_room = room.clone();
        let queued_reads = reads.clone();
        let queued_policy = current_policy.clone();
        let publication = tokio::spawn(async move {
            refresh_live_room(
                &queued_room,
                account,
                || true,
                || async {
                    queued_reads.fetch_add(1, Ordering::SeqCst);
                    Ok(queued_policy.lock().unwrap().clone())
                },
            )
            .await
        });
        tokio::task::yield_now().await;
        assert_eq!(reads.load(Ordering::SeqCst), 0);
        current_policy.lock().unwrap().allow_private_messages = false;
        drop(held);
        tokio::time::timeout(Duration::from_secs(2), publication)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let state = room.read().await;
        assert!(
            !state.participants[&ParticipantId::from(account)]
                .social
                .accepts_inbox_from(&pid("peer"))
        );
        assert_eq!(reads.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn concurrent_publications_cannot_apply_an_old_snapshot_after_the_new_one() {
        let account = Uuid::new_v4();
        let room = fixture(account);
        let reads = Arc::new(AtomicUsize::new(0));
        let (entered, entered_rx) = oneshot::channel();
        let (release, release_rx) = oneshot::channel();
        let old_room = room.clone();
        let first = tokio::spawn(async move {
            refresh_live_room(
                &old_room,
                account,
                || true,
                || async {
                    entered.send(()).unwrap();
                    release_rx.await.unwrap();
                    Ok(ChatPreferences::default())
                },
            )
            .await
        });
        entered_rx.await.unwrap();
        let new_room = room.clone();
        let new_reads = reads.clone();
        let second = tokio::spawn(async move {
            refresh_live_room(
                &new_room,
                account,
                || true,
                || async {
                    new_reads.fetch_add(1, Ordering::SeqCst);
                    Ok(ChatPreferences {
                        allow_private_messages: false,
                        ..Default::default()
                    })
                },
            )
            .await
        });
        tokio::task::yield_now().await;
        assert_eq!(
            reads.load(Ordering::SeqCst),
            0,
            "the newer read waits for the old publication"
        );
        release.send(()).unwrap();
        first.await.unwrap().unwrap();
        second.await.unwrap().unwrap();
        let state = room.read().await;
        assert!(
            !state.participants[&ParticipantId::from(account)]
                .social
                .accepts_inbox_from(&pid("peer"))
        );
        assert_eq!(reads.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_retired_room_is_not_loaded_and_read_failure_preserves_existing_ignores() {
        let account = Uuid::new_v4();
        let peer = Uuid::new_v4();
        let room = fixture(account);
        let saved = ChatPreferences {
            ignored: vec![crate::auth::account::IgnoredPerson {
                id: peer.to_string(),
                name: "Peer".into(),
            }],
            ..Default::default()
        };
        room.write()
            .await
            .participants
            .get_mut(&ParticipantId::from(account))
            .unwrap()
            .social
            .apply_account_preferences(&saved);
        let reads = AtomicUsize::new(0);
        refresh_live_room(
            &room,
            account,
            || false,
            || async {
                reads.fetch_add(1, Ordering::SeqCst);
                Ok(ChatPreferences::default())
            },
        )
        .await
        .unwrap();
        assert_eq!(
            reads.load(Ordering::SeqCst),
            0,
            "a replaced runtime cannot read or publish policy"
        );
        assert!(
            refresh_live_room(
                &room,
                account,
                || true,
                || async { Err(sqlx::Error::PoolTimedOut) }
            )
            .await
            .is_err()
        );
        let state = room.read().await;
        let actor = &state.participants[&ParticipantId::from(account)];
        assert!(!actor.social.accepts_inbox_from(&ParticipantId::from(peer)));
        assert!(actor.social.accepts_inbox_from(&pid("unrelated-peer")));
    }

    async fn database_fixture() -> (RoomManager, PgPool, Uuid, Uuid) {
        use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
        use std::str::FromStr;
        assert_eq!(
            std::env::var("DISPOSABLE_TEST_DATABASE").as_deref(),
            Ok("1")
        );
        let options = PgConnectOptions::from_str(
            &std::env::var("TEST_DATABASE_URL").expect("migrated disposable TEST_DATABASE_URL"),
        )
        .unwrap();
        assert!(matches!(options.get_host(), "127.0.0.1" | "::1"));
        assert!(options.get_socket().is_none());
        assert!(
            options
                .get_database()
                .is_some_and(|name| name.ends_with("_test"))
        );
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .acquire_timeout(Duration::from_secs(3))
            .connect_with(options)
            .await
            .unwrap();
        let mut accounts = Vec::new();
        for _ in 0..2 {
            let account: Uuid = sqlx::query_scalar("INSERT INTO users(email,display_name) VALUES($1,'Privacy membership fixture') RETURNING id")
                .bind(format!("{}@privacy-membership.invalid", Uuid::new_v4())).fetch_one(&pool).await.unwrap();
            accounts.push(account);
        }
        let mut config = crate::media::MediaConfig::default();
        config.worker_config.num_workers = 1;
        config.webrtc_server_port_base = 0;
        let mut manager = RoomManager::new(
            crate::room::ManagerSettings::default(),
            config,
            crate::metrics::ServerMetrics::new(),
            Some(pool.clone()),
        )
        .await
        .unwrap();
        manager.allow_ad_hoc_rooms = true;
        (manager, pool, accounts[0], accounts[1])
    }

    async fn join(
        manager: &RoomManager,
        room: &str,
        participant: Uuid,
        authenticated: bool,
    ) -> (
        mpsc::Sender<crate::OutboundJson>,
        mpsc::Receiver<crate::OutboundJson>,
        super::super::JoinResult,
    ) {
        let (sender, receiver) = mpsc::channel(32);
        let result = manager
            .add_participant(
                &rid(room),
                ParticipantId::from(participant),
                "Maya".into(),
                sender.clone(),
                authenticated,
                Arc::new(std::sync::atomic::AtomicBool::new(false)),
                None,
                &Uuid::new_v4().to_string(),
                None,
                None,
            )
            .await
            .unwrap();
        (sender, receiver, result)
    }

    async fn finish_database_fixture(manager: RoomManager, pool: PgPool, accounts: [Uuid; 2]) {
        manager.shutdown().await.unwrap();
        manager.media_server().shutdown().await.unwrap();
        sqlx::query("DELETE FROM users WHERE id=ANY($1)")
            .bind(accounts.to_vec())
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;
    }

    #[tokio::test]
    #[ignore = "requires migrated disposable TEST_DATABASE_URL and mediasoup worker"]
    async fn database_join_enforces_saved_privacy_before_any_client_preference_command() {
        let (manager, pool, account, peer) = database_fixture().await;
        sqlx::query("UPDATE users SET preferences=$2 WHERE id=$1").bind(account)
            .bind(serde_json::json!({"allowPrivateMessages": false, "ignored": [{"id": peer, "name": "Peer"}]}))
            .execute(&pool).await.unwrap();
        let room_id = rid(&format!("privacy-join-{}", Uuid::new_v4()));
        let guest = Uuid::new_v4();
        let (guest_sender, _guest_receiver, _) =
            join(&manager, room_id.as_str(), guest, false).await;
        let (_account_sender, mut account_receiver, joined) =
            join(&manager, room_id.as_str(), account, true).await;
        assert!(
            matches!(joined, super::super::JoinResult::Joined { ref name, .. } if name == "Maya (2)")
        );
        assert!(
            manager
                .send_social_chat(
                    &room_id,
                    &ParticipantId::from(guest),
                    &guest_sender,
                    "Blocked guest private message".into(),
                    Some(Uuid::new_v4().to_string()),
                    Some(&ParticipantId::from(account))
                )
                .await
                .is_err()
        );
        let (peer_sender, _peer_receiver, _) = join(&manager, room_id.as_str(), peer, true).await;
        while account_receiver.try_recv().is_ok() {}
        manager
            .send_social_chat(
                &room_id,
                &ParticipantId::from(peer),
                &peer_sender,
                "Ignored public message".into(),
                Some(Uuid::new_v4().to_string()),
                None,
            )
            .await
            .unwrap();
        while let Ok(message) = account_receiver.try_recv() {
            let message: serde_json::Value = serde_json::from_str(&message).unwrap();
            assert_ne!(
                message["type"], "chatReceived",
                "saved ignores apply before the account's first client command"
            );
        }
        finish_database_fixture(manager, pool, [account, peer]).await;
    }

    #[tokio::test]
    #[ignore = "requires migrated disposable TEST_DATABASE_URL and mediasoup worker"]
    async fn database_pending_join_and_lobby_admission_load_policy_after_a_concurrent_save() {
        let (manager, pool, account, peer) = database_fixture().await;
        let room_id = rid(&format!("privacy-race-{}", Uuid::new_v4()));
        let guest = Uuid::new_v4();
        let (guest_sender, _guest_receiver, _) =
            join(&manager, room_id.as_str(), guest, false).await;
        let runtime = manager.get_room(&room_id).unwrap();
        let held = control::lock_room(&runtime).await;
        let joining_manager = manager.clone();
        let joining_room = room_id.clone();
        let joining = tokio::spawn(async move {
            join(&joining_manager, joining_room.as_str(), account, true).await
        });
        tokio::task::yield_now().await;
        assert!(!joining.is_finished());
        sqlx::query("UPDATE users SET preferences=$2 WHERE id=$1")
            .bind(account)
            .bind(serde_json::json!({"allowPrivateMessages": false}))
            .execute(&pool)
            .await
            .unwrap();
        // The account is not installed yet, so this publication may skip this
        // room. Its join must independently read the newly committed policy.
        manager.refresh_account_preferences(account).await.unwrap();
        drop(held);
        let (_account_sender, _account_receiver, _) = joining.await.unwrap();
        assert!(
            manager
                .send_social_chat(
                    &room_id,
                    &ParticipantId::from(guest),
                    &guest_sender,
                    "Blocked during join race".into(),
                    Some(Uuid::new_v4().to_string()),
                    Some(&ParticipantId::from(account))
                )
                .await
                .is_err()
        );

        let lobby_room_id = rid(&format!("privacy-lobby-{}", Uuid::new_v4()));
        let moderator = Uuid::new_v4();
        let (moderator_sender, _moderator_receiver, _) =
            join(&manager, lobby_room_id.as_str(), moderator, false).await;
        let mut settings = crate::room::RoomSettings::defaults(&lobby_room_id);
        settings.lobby_enabled = true;
        manager
            .get_room(&lobby_room_id)
            .unwrap()
            .write()
            .await
            .settings = Some(settings);
        let (_peer_sender, _peer_receiver, waiting) =
            join(&manager, lobby_room_id.as_str(), peer, true).await;
        assert!(matches!(waiting, super::super::JoinResult::Lobbied));
        sqlx::query("UPDATE users SET preferences=$2 WHERE id=$1")
            .bind(peer)
            .bind(serde_json::json!({"allowPrivateMessages": false}))
            .execute(&pool)
            .await
            .unwrap();
        manager
            .admit_from_lobby(
                &lobby_room_id,
                &ParticipantId::from(moderator),
                &moderator_sender,
                &ParticipantId::from(peer),
            )
            .await
            .unwrap();
        assert!(
            manager
                .send_social_chat(
                    &lobby_room_id,
                    &ParticipantId::from(moderator),
                    &moderator_sender,
                    "Blocked after lobby admission".into(),
                    Some(Uuid::new_v4().to_string()),
                    Some(&ParticipantId::from(peer))
                )
                .await
                .is_err()
        );
        finish_database_fixture(manager, pool, [account, peer]).await;
    }
}
