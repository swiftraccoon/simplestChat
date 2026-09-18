//! Room-control ordering and persistence isolation regressions.
//!
//! Future gates establish exact phases without scheduling sleeps. The ignored
//! integration fixture locks only its generated row in an explicitly disposable
//! PostgreSQL database; no service failure or public workload is induced.

use super::*;
use futures_util::FutureExt;
use serde_json::Value;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use std::future::{Future, ready};
use std::panic::AssertUnwindSafe;
use std::str::FromStr;
use std::time::Duration;
use tokio::sync::oneshot;
use uuid::Uuid;

const CHECK_DEADLINE: Duration = Duration::from_secs(2);

async fn bounded<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout(CHECK_DEADLINE, future)
        .await
        .expect("owned room-control check must make progress")
}

fn runtime_room(id: &str) -> Arc<TokioRwLock<Room>> {
    Arc::new(TokioRwLock::new(Room::new(
        id.to_string(),
        "unused-control-router".to_string(),
        Some(RoomManager::default_room_settings(id)),
        false,
        None,
    )))
}

fn participant(role: roles::Role) -> (Participant, mpsc::Receiver<Arc<String>>) {
    let (sender, receiver) = mpsc::channel(32);
    (
        Participant {
            id: Uuid::new_v4().to_string(),
            name: "Room control fixture".to_string(),
            social: social::ParticipantSocial::new(0),
            sender,
            media_session_id: Uuid::new_v4(),
            producers: HashMap::new(),
            role,
            punitive: moderation::PunitiveState::default(),
            authenticated: true,
            ip: None,
        },
        receiver,
    )
}

struct Fixture {
    manager: RoomManager,
    room_id: String,
    room: Arc<TokioRwLock<Room>>,
    owner: Participant,
    owner_messages: mpsc::Receiver<Arc<String>>,
}

/// Native construction is necessary only for tests of RoomManager's actual
/// coordinator. Cleanup runs even when assertions or the outer deadline fail.
async fn with_fixture<F, Fut>(test: F)
where
    F: FnOnce(Fixture) -> Fut,
    Fut: Future<Output = ()>,
{
    let manager = RoomManager::new_for_connection_tests(ServerMetrics::new()).await;
    let room_id = format!("control-{}", Uuid::new_v4());
    let room = runtime_room(&room_id);
    let (owner, owner_messages) = participant(roles::Role::Owner);
    room.write()
        .await
        .participants
        .insert(owner.id.clone(), owner.clone());
    manager
        .rooms
        .write()
        .unwrap()
        .insert(room_id.clone(), room.clone());
    let fixture = Fixture {
        manager: manager.clone(),
        room_id,
        room,
        owner,
        owner_messages,
    };
    let outcome = AssertUnwindSafe(tokio::time::timeout(Duration::from_secs(20), test(fixture)))
        .catch_unwind()
        .await;
    let room_cleanup = tokio::time::timeout(Duration::from_secs(10), manager.shutdown()).await;
    let media_cleanup =
        tokio::time::timeout(Duration::from_secs(10), manager.media_server.shutdown()).await;
    match outcome {
        Err(panic) => std::panic::resume_unwind(panic),
        Ok(result) => result.expect("room-control fixture timed out"),
    }
    room_cleanup
        .expect("room fixture cleanup timed out")
        .expect("room fixture cleanup failed");
    media_cleanup
        .expect("native fixture cleanup timed out")
        .expect("native fixture cleanup failed");
}

#[tokio::test]
async fn waiting_for_control_never_retains_the_main_room_lock() {
    let room = runtime_room("control-lock");
    let first = control::lock_room(&room).await;
    let mut waiting = std::pin::pin!(control::lock_room(&room));
    assert!(futures_util::poll!(&mut waiting).is_pending());
    assert!(!bounded(room.read()).await.deleting);
    bounded(room.write()).await.social = social::RoomSocial::default();
    drop(first);
    drop(bounded(waiting).await);
}

#[tokio::test]
async fn control_gates_are_per_room_not_process_wide() {
    let first = runtime_room("first-control-room");
    let second = runtime_room("second-control-room");
    let held = control::lock_room(&first).await;
    drop(bounded(control::lock_room(&second)).await);
    drop(held);
}

#[test]
fn cloning_shared_limiters_cannot_replenish_admission_capacity() {
    let limiter = SharedRateLimiter::new(1);
    let cloned = limiter.clone();
    let now = std::time::Instant::now();
    assert!(limiter.allow("same identity", now));
    assert!(!cloned.allow("same identity", now));
    assert!(cloned.allow("different identity", now));
    assert!(!limiter.allow("different identity", now));
}

#[tokio::test]
async fn persistence_timeout_drops_pending_sql_and_quarantines_before_control_release() {
    struct PendingWrite(Arc<AtomicBool>);
    impl Drop for PendingWrite {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Relaxed);
        }
    }
    with_fixture(|mut fixture| async move {
        let control = control::lock_room(&fixture.room).await;
        let cancelled = Arc::new(AtomicBool::new(false));
        let pending = PendingWrite(cancelled.clone());
        let persistence = async move {
            let _pending = pending;
            std::future::pending::<Result<(), sqlx::Error>>().await
        };
        let error = bounded(fixture.manager.persist_room_until(
            &fixture.room_id,
            &fixture.room,
            tokio::time::Instant::now(),
            persistence,
        ))
        .await
        .unwrap_err();
        assert!(matches!(error, sqlx::Error::Protocol(_)));
        assert!(cancelled.load(Ordering::Relaxed));
        assert!(fixture.room.read().await.deleting);
        assert!(fixture.room.read().await.participants.is_empty());
        assert!(
            fixture
                .manager
                .deleting_rooms
                .read()
                .unwrap()
                .contains_key(&fixture.room_id)
        );
        let notice: Value =
            serde_json::from_str(&bounded(fixture.owner_messages.recv()).await.unwrap()).unwrap();
        assert_eq!(notice["type"], "roomClosed");
        drop(control);
    })
    .await;
}

#[test]
fn postgres_rejection_requires_a_confirmed_rollback_class_or_code() {
    for code in [
        "22012", "22P02", "23502", "23505", "42601", "42P01", "40000", "40001", "40002", "40P01",
        "55P03", "57014",
    ] {
        assert!(
            control::postgres_rejection_is_definite(Some(code)),
            "PostgreSQL rejection {code} confirms this statement did not commit"
        );
    }
    for code in [
        None,
        Some("40003"),
        Some("08007"),
        Some("08006"),
        Some("57P01"),
        Some("XX000"),
    ] {
        assert!(
            !control::postgres_rejection_is_definite(code),
            "PostgreSQL outcome {code:?} cannot establish rollback"
        );
    }
}

#[tokio::test]
async fn admitted_control_survives_caller_cancellation_and_publishes_before_its_successor() {
    with_fixture(|fixture| async move {
        let (entered, admission) = oneshot::channel();
        let (release, persistence) = oneshot::channel();
        let manager = fixture.manager.clone();
        let room_id = fixture.room_id.clone();
        let caller = tokio::spawn(async move {
            manager
                .run_room_control(&room_id, move |manager, id, control| async move {
                    let _control = control;
                    let _ = entered.send(());
                    persistence.await?;
                    let room = manager.get_room(&id)?;
                    manager
                        .persist_room(&id, &room, ready(Ok::<(), sqlx::Error>(())))
                        .await?;
                    room.write().await.settings.as_mut().unwrap().topic = Some("first".into());
                    Ok(())
                })
                .await
        });
        bounded(admission).await.unwrap();
        let mut successor = std::pin::pin!(fixture.manager.run_room_control(
            &fixture.room_id,
            |manager, id, control| async move {
                let _control = control;
                let room = manager.get_room(&id)?;
                let mut room = room.write().await;
                assert_eq!(
                    room.settings.as_ref().unwrap().topic.as_deref(),
                    Some("first")
                );
                room.settings.as_mut().unwrap().topic = Some("second".into());
                Ok(())
            },
        ));
        assert!(futures_util::poll!(&mut successor).is_pending());
        assert!(
            bounded(fixture.room.read())
                .await
                .settings
                .as_ref()
                .unwrap()
                .topic
                .is_none()
        );
        drop(bounded(fixture.room.write()).await);
        caller.abort();
        assert!(bounded(caller).await.unwrap_err().is_cancelled());
        release.send(()).unwrap();
        bounded(successor).await.unwrap();
        assert_eq!(
            fixture
                .room
                .read()
                .await
                .settings
                .as_ref()
                .unwrap()
                .topic
                .as_deref(),
            Some("second")
        );
        drop(bounded(control::lock_room(&fixture.room)).await);
    })
    .await;
}

#[tokio::test]
async fn queued_control_revalidates_sender_permissions_before_applying_a_topic() {
    with_fixture(|mut fixture| async move {
        for retire_sender in [false, true] {
            let held = control::lock_room(&fixture.room).await;
            {
                let mut room = fixture.room.write().await;
                let owner = room.participants.get_mut(&fixture.owner.id).unwrap();
                owner.role = roles::Role::Owner;
                owner.sender = fixture.owner.sender.clone();
            }
            let mut pending = std::pin::pin!(fixture.manager.set_topic(
                &fixture.room_id,
                &fixture.owner.id,
                &fixture.owner.sender,
                "must-not-publish".into(),
            ));
            assert!(futures_util::poll!(&mut pending).is_pending());
            let mut room = bounded(fixture.room.write()).await;
            let owner = room.participants.get_mut(&fixture.owner.id).unwrap();
            if retire_sender {
                let (replacement, _receiver) = mpsc::channel(4);
                owner.sender = replacement;
                owner.role = roles::Role::Owner;
            } else {
                owner.role = roles::Role::User;
            }
            drop(room);
            drop(held);
            assert!(bounded(pending).await.is_err());
            assert!(
                fixture
                    .room
                    .read()
                    .await
                    .settings
                    .as_ref()
                    .unwrap()
                    .topic
                    .is_none()
            );
            assert!(fixture.owner_messages.try_recv().is_err());
        }
    })
    .await;
}

#[tokio::test]
async fn cancellation_while_queued_does_no_work_and_releases_its_admission_slot() {
    with_fixture(|fixture| async move {
        let held = control::lock_room(&fixture.room).await;
        let ran = Arc::new(AtomicBool::new(false));
        let action_ran = ran.clone();
        {
            let mut queued = std::pin::pin!(fixture.manager.run_room_control(
                &fixture.room_id,
                move |_manager, _id, control| async move {
                    let _control = control;
                    action_ran.store(true, Ordering::Relaxed);
                    Ok(())
                },
            ));
            assert!(futures_util::poll!(&mut queued).is_pending());
        }
        drop(held);
        bounded(fixture.manager.run_room_control(
            &fixture.room_id,
            |_manager, _id, control| async move {
                let _control = control;
                Ok(())
            },
        ))
        .await
        .unwrap();
        assert!(!ran.load(Ordering::Relaxed));
    })
    .await;
}

#[tokio::test]
async fn queued_control_never_runs_against_a_same_id_replacement() {
    with_fixture(|fixture| async move {
        let held = control::lock_room(&fixture.room).await;
        let ran = Arc::new(AtomicBool::new(false));
        let action_ran = ran.clone();
        let mut pending = std::pin::pin!(fixture.manager.run_room_control(
            &fixture.room_id,
            move |_manager, _id, control| async move {
                let _control = control;
                action_ran.store(true, Ordering::Relaxed);
                Ok(())
            },
        ));
        assert!(futures_util::poll!(&mut pending).is_pending());
        let replacement = runtime_room(&fixture.room_id);
        fixture
            .manager
            .rooms
            .write()
            .unwrap()
            .insert(fixture.room_id.clone(), replacement.clone());
        drop(held);
        assert!(bounded(pending).await.is_err());
        assert!(!ran.load(Ordering::Relaxed));
        assert!(!replacement.read().await.deleting);
        assert!(
            replacement
                .read()
                .await
                .settings
                .as_ref()
                .unwrap()
                .topic
                .is_none()
        );
    })
    .await;
}

#[tokio::test]
async fn queued_control_rejects_tombstone_and_drain_before_starting_work() {
    with_fixture(|fixture| async move {
        for draining in [false, true] {
            let held = control::lock_room(&fixture.room).await;
            let ran = Arc::new(AtomicBool::new(false));
            let action_ran = ran.clone();
            let mut pending = std::pin::pin!(fixture.manager.run_room_control(
                &fixture.room_id,
                move |_manager, _id, control| async move {
                    let _control = control;
                    action_ran.store(true, Ordering::Relaxed);
                    Ok(())
                },
            ));
            assert!(futures_util::poll!(&mut pending).is_pending());
            if draining {
                fixture.manager.drain_signal().begin_draining();
            } else {
                fixture.room.write().await.deleting = true;
            }
            drop(held);
            assert!(bounded(pending).await.is_err());
            assert!(!ran.load(Ordering::Relaxed));
            fixture.room.write().await.deleting = false;
        }
    })
    .await;
}

#[tokio::test]
async fn definite_persistence_failure_preserves_room_policy_and_control_capacity() {
    with_fixture(|fixture| async move {
        for error in [
            sqlx::Error::PoolClosed,
            sqlx::Error::PoolTimedOut,
            sqlx::Error::InvalidArgument("controlled fixture rejection".into()),
        ] {
            assert!(!control::persistence_is_indeterminate(&error));
            assert!(
                fixture
                    .manager
                    .persist_room(
                        &fixture.room_id,
                        &fixture.room,
                        ready(Err::<(), sqlx::Error>(error)),
                    )
                    .await
                    .is_err()
            );
            assert!(!fixture.room.read().await.deleting);
            assert!(
                fixture
                    .room
                    .read()
                    .await
                    .settings
                    .as_ref()
                    .unwrap()
                    .topic
                    .is_none()
            );
            assert!(
                !fixture
                    .manager
                    .deleting_rooms
                    .read()
                    .unwrap()
                    .contains_key(&fixture.room_id)
            );
            drop(bounded(control::lock_room(&fixture.room)).await);
        }
    })
    .await;
}

#[tokio::test]
async fn indeterminate_persistence_quarantines_only_its_exact_room_incarnation() {
    with_fixture(|fixture| async move {
        for error in [
            sqlx::Error::Io(std::io::Error::new(
                std::io::ErrorKind::ConnectionReset,
                "fixture",
            )),
            sqlx::Error::Protocol("fixture".into()),
            sqlx::Error::RowNotFound,
        ] {
            assert!(control::persistence_is_indeterminate(&error));
        }
        let replacement = runtime_room(&fixture.room_id);
        fixture
            .manager
            .rooms
            .write()
            .unwrap()
            .insert(fixture.room_id.clone(), replacement.clone());
        assert!(
            fixture
                .manager
                .persist_room(
                    &fixture.room_id,
                    &fixture.room,
                    ready(Err::<(), sqlx::Error>(sqlx::Error::Protocol(
                        "fixture".into()
                    ))),
                )
                .await
                .is_err()
        );
        assert!(!replacement.read().await.deleting);
        assert!(
            !fixture
                .manager
                .deleting_rooms
                .read()
                .unwrap()
                .contains_key(&fixture.room_id)
        );

        assert!(
            fixture
                .manager
                .persist_room(
                    &fixture.room_id,
                    &replacement,
                    ready(Err::<(), sqlx::Error>(sqlx::Error::RowNotFound)),
                )
                .await
                .is_err()
        );
        assert!(replacement.read().await.deleting);
        assert!(
            fixture
                .manager
                .deleting_rooms
                .read()
                .unwrap()
                .contains_key(&fixture.room_id)
        );
        assert!(Arc::ptr_eq(
            &fixture.manager.get_room(&fixture.room_id).unwrap(),
            &replacement
        ));
    })
    .await;
}

/// Observe the mutation's actual PostgreSQL lock wait, rather than guessing
/// whether a spawned task reached SQL after a scheduler yield or fixed sleep.
async fn wait_for_owned_row_wait(
    blocker: &mut sqlx::Transaction<'static, sqlx::Postgres>,
    application_name: &str,
) {
    bounded(async {
        loop {
            // The lock-owning transaction must refresh its activity snapshot
            // when the writer acquires a new pool connection after this wait starts.
            sqlx::query("SELECT pg_stat_clear_snapshot()")
                .execute(&mut **blocker)
                .await
                .unwrap();
            let blocked: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM pg_stat_activity
                 WHERE application_name = $1 AND pid <> pg_backend_pid()
                   AND pg_backend_pid() = ANY(pg_blocking_pids(pid)))",
            )
            .bind(application_name)
            .fetch_one(&mut **blocker)
            .await
            .unwrap();
            if blocked {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
}

#[tokio::test]
#[ignore = "requires migrated disposable TEST_DATABASE_URL and mediasoup worker"]
async fn blocked_room_sql_keeps_chat_available_and_committed_control_outlives_its_caller() {
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
            .is_some_and(|database| database.ends_with("_test"))
    );
    let room_id = format!("control-sql-{}", Uuid::new_v4());
    let application_name = format!("control-fixture-{}", Uuid::new_v4());
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .acquire_timeout(Duration::from_secs(3))
        .connect_with(options.application_name(&application_name).options([
            ("statement_timeout", "10s"),
            ("lock_timeout", "5s"),
            ("idle_in_transaction_session_timeout", "15s"),
        ]))
        .await
        .unwrap();
    let mut config = MediaConfig::default();
    config.worker_config.num_workers = 1;
    config.webrtc_server_port_base = 0;
    let manager = RoomManager::new(config, ServerMetrics::new(), Some(pool.clone()))
        .await
        .unwrap();
    let (owner, mut owner_messages) = participant(roles::Role::Owner);
    let (member, mut member_messages) = participant(roles::Role::User);
    let owner_id = Uuid::parse_str(&owner.id).unwrap();
    let mut blocker = None;
    let mut caller = None;
    let mut installed_room = None;

    let outcome = AssertUnwindSafe(tokio::time::timeout(Duration::from_secs(30), async {
        sqlx::query("INSERT INTO users(id,email,display_name) VALUES($1,$2,$3)")
            .bind(owner_id)
            .bind(format!("{owner_id}@control.invalid"))
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
        let (settings, hash) = settings::load_room(&pool, &room_id).await.unwrap().unwrap();
        let room = Arc::new(TokioRwLock::new(Room::new(
            room_id.clone(),
            "unused-control-router".into(),
            Some(settings),
            true,
            hash,
        )));
        {
            let mut room = room.write().await;
            room.participants.insert(owner.id.clone(), owner.clone());
            room.participants.insert(member.id.clone(), member.clone());
        }
        manager
            .rooms
            .write()
            .unwrap()
            .insert(room_id.clone(), room.clone());
        installed_room = Some(room.clone());
        blocker = Some(pool.begin().await.unwrap());
        sqlx::query("SELECT id FROM rooms WHERE id=$1 FOR UPDATE")
            .bind(&room_id)
            .fetch_one(&mut **blocker.as_mut().unwrap())
            .await
            .unwrap();
        let controlled_manager = manager.clone();
        let controlled_id = room_id.clone();
        let controlled_owner = owner.clone();
        caller = Some(tokio::spawn(async move {
            controlled_manager
                .set_topic(
                    &controlled_id,
                    &controlled_owner.id,
                    &controlled_owner.sender,
                    "first committed topic".into(),
                )
                .await
        }));
        wait_for_owned_row_wait(blocker.as_mut().unwrap(), &application_name).await;

        assert!(
            bounded(room.read())
                .await
                .settings
                .as_ref()
                .unwrap()
                .topic
                .is_none()
        );
        drop(bounded(room.write()).await);
        let mut second_topic = std::pin::pin!(manager.set_topic(
            &room_id,
            &owner.id,
            &owner.sender,
            "second committed topic".into(),
        ));
        assert!(futures_util::poll!(&mut second_topic).is_pending());
        let mut departure = std::pin::pin!(manager.remove_participant_for_sender(
            &room_id,
            &member.id,
            &member.sender,
        ));
        assert!(
            futures_util::poll!(&mut departure).is_pending(),
            "membership must wait for the policy commit, not bypass it"
        );
        bounded(manager.broadcast_chat(
            &room_id,
            &owner.id,
            &owner.sender,
            "chat remains available during owned SQL wait".into(),
        ))
        .await
        .unwrap();
        let chat: Value =
            serde_json::from_str(&bounded(member_messages.recv()).await.unwrap()).unwrap();
        assert_eq!(chat["type"], "chatReceived");
        assert_eq!(
            chat["content"],
            "chat remains available during owned SQL wait"
        );
        let ack: Value =
            serde_json::from_str(&bounded(owner_messages.recv()).await.unwrap()).unwrap();
        assert_eq!(ack["type"], "messageAck");
        assert!(
            owner_messages.try_recv().is_err(),
            "no topic is published before SQL commits"
        );

        // Keep only runtime publication pending, after allowing SQL to commit.
        // This distinguishes caller cancellation from cancellation before SQL.
        let held_room = bounded(room.write()).await;
        blocker.take().unwrap().rollback().await.unwrap();
        bounded(async {
            loop {
                let topic: Option<String> =
                    sqlx::query_scalar("SELECT topic FROM rooms WHERE id=$1")
                        .bind(&room_id)
                        .fetch_one(&pool)
                        .await
                        .unwrap();
                if topic.as_deref() == Some("first committed topic") {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await;
        assert!(held_room.settings.as_ref().unwrap().topic.is_none());
        let cancelled = caller.take().unwrap();
        cancelled.abort();
        assert!(bounded(cancelled).await.unwrap_err().is_cancelled());
        drop(held_room);
        bounded(second_topic).await.unwrap();
        assert!(bounded(departure).await.unwrap());
        let saved: Option<String> = sqlx::query_scalar("SELECT topic FROM rooms WHERE id=$1")
            .bind(&room_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(saved.as_deref(), Some("second committed topic"));
        let room = room.read().await;
        assert_eq!(room.settings.as_ref().unwrap().topic, saved);
        assert!(!room.participants.contains_key(&member.id));
        drop(room);
        let mut topics = Vec::new();
        while let Ok(message) = owner_messages.try_recv() {
            let message: Value = serde_json::from_str(&message).unwrap();
            if message["type"] == "topicChanged" {
                topics.push(message["topic"].as_str().unwrap().to_string());
            }
        }
        assert_eq!(topics, ["first committed topic", "second committed topic"]);

        // A PostgreSQL-rejected statement has a definite unchanged outcome.
        // The fixture targets its own row with a NOT NULL violation; no runtime
        // policy or persisted topic should be quarantined or changed by it.
        let controlled = control::lock_room(installed_room.as_ref().unwrap()).await;
        let error = manager
            .persist_room(
                &room_id,
                installed_room.as_ref().unwrap(),
                sqlx::query("UPDATE rooms SET display_name=NULL WHERE id=$1")
                    .bind(&room_id)
                    .execute(&pool),
            )
            .await
            .unwrap_err();
        assert!(matches!(error, sqlx::Error::Database(_)));
        assert!(!installed_room.as_ref().unwrap().read().await.deleting);
        drop(controlled);
    }))
    .catch_unwind()
    .await;

    // Release only this fixture's blockers before waiting for owned mutations.
    let blocker_cleanup = if let Some(transaction) = blocker {
        Some(tokio::time::timeout(Duration::from_secs(5), transaction.rollback()).await)
    } else {
        None
    };
    if let Some(task) = caller {
        task.abort();
        let _ = tokio::time::timeout(Duration::from_secs(2), task).await;
    }
    let control_cleanup = if let Some(room) = installed_room {
        tokio::time::timeout(Duration::from_secs(16), control::lock_room(&room))
            .await
            .map(drop)
    } else {
        Ok(())
    };
    let room_cleanup = tokio::time::timeout(Duration::from_secs(10), manager.shutdown()).await;
    let row_cleanup = tokio::time::timeout(Duration::from_secs(10), async {
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
    let media_cleanup =
        tokio::time::timeout(Duration::from_secs(10), manager.media_server.shutdown()).await;
    let pool_cleanup = tokio::time::timeout(Duration::from_secs(5), pool.close()).await;
    match outcome {
        Err(panic) => std::panic::resume_unwind(panic),
        Ok(result) => result.expect("disposable room persistence fixture timed out"),
    }
    if let Some(cleanup) = blocker_cleanup {
        cleanup
            .expect("owned row blocker cleanup timed out")
            .expect("owned row blocker cleanup failed");
    }
    control_cleanup.expect("owned room mutations did not finish during cleanup");
    room_cleanup
        .expect("room cleanup timed out")
        .expect("room cleanup failed");
    row_cleanup
        .expect("row cleanup timed out")
        .expect("row cleanup failed");
    media_cleanup
        .expect("native cleanup timed out")
        .expect("native cleanup failed");
    pool_cleanup.expect("database pool cleanup timed out");
}
