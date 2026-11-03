//! Room-control ordering and persistence isolation regressions.
//!
//! Future gates establish exact phases without scheduling sleeps. The ignored
//! integration fixture locks only its generated row in an explicitly disposable
//! PostgreSQL database; no service failure or public workload is induced.
use super::*;
use crate::ids::fixtures::{pid, rid};
use crate::ids::{MessageId, RoomId};
use crate::room::refusal::{Refusal, RoomError};
use crate::signaling::protocol::ErrorCode;
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
        rid(id),
        Some(crate::room::RoomSettings::defaults(&rid(id))),
        false,
        None,
    )))
}

fn participant(role: roles::Role) -> (Participant, mpsc::Receiver<crate::OutboundJson>) {
    let (sender, receiver) = mpsc::channel(32);
    (
        Participant {
            id: ParticipantId::random(),
            name: "Room control fixture".to_string(),
            social: social::ParticipantSocial::new(0),
            sender,
            media_session_id: Uuid::new_v4(),
            producers: HashMap::new(),
            role,
            punitive: moderation::PunitiveState::default(),
            authenticated: true,
            ip: None,
            chat_style: Default::default(),
        },
        receiver,
    )
}

struct Fixture {
    manager: RoomManager,
    room_id: RoomId,
    room: Arc<TokioRwLock<Room>>,
    owner: Participant,
    owner_messages: mpsc::Receiver<crate::OutboundJson>,
}

/// Native construction is necessary only for tests of RoomManager's actual
/// coordinator. Cleanup runs even when assertions or the outer deadline fail.
async fn with_fixture<F, Fut>(test: F)
where
    F: FnOnce(Fixture) -> Fut,
    Fut: Future<Output = ()>,
{
    let manager = RoomManager::new_for_connection_tests(ServerMetrics::new()).await;
    let room_id = rid(&format!("control-{}", Uuid::new_v4()));
    let room = runtime_room(room_id.as_str());
    let (owner, owner_messages) = participant(roles::Role::Owner);
    room.write()
        .await
        .participants
        .insert(owner.id, owner.clone());
    manager.rooms.write().insert(room_id.clone(), room.clone());
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
            std::future::pending::<Result<(), RoomError>>().await
        };
        let error = bounded(fixture.manager.persist_room_until(
            &fixture.room_id,
            &fixture.room,
            tokio::time::Instant::now(),
            persistence,
        ))
        .await
        .unwrap_err();
        assert!(matches!(
            error,
            RoomError::Database(sqlx::Error::Protocol(_))
        ));
        assert!(cancelled.load(Ordering::Relaxed));
        assert!(fixture.room.read().await.deleting);
        assert!(fixture.room.read().await.participants.is_empty());
        assert!(
            fixture
                .manager
                .deleting_rooms
                .read()
                .contains_key(&fixture.room_id)
        );
        let notice: Value =
            serde_json::from_str(&bounded(fixture.owner_messages.recv()).await.unwrap()).unwrap();
        assert_eq!(notice["type"], "roomClosed");
        drop(control);
    })
    .await;
}

/// A save that runs out of time may have committed: its requester is told so,
/// never that it could not be saved.
#[tokio::test]
async fn a_save_past_its_deadline_answers_that_it_may_not_have_been_saved() {
    with_fixture(|fixture| async move {
        let control = control::lock_room(&fixture.room).await;
        let error = bounded(fixture.manager.persist_room_until(
            &fixture.room_id,
            &fixture.room,
            tokio::time::Instant::now(),
            std::future::pending::<Result<(), RoomError>>(),
        ))
        .await
        .unwrap_err();
        let answer = control::Save::Settings.failure(&fixture.room_id, error);
        assert_eq!(answer.code(), ErrorCode::TimedOut);
        assert_eq!(
            answer.client_message(),
            "Room settings may not have been saved"
        );
        assert!(fixture.room.read().await.deleting);
        drop(control);
    })
    .await;
}

/// Every save, the room's and the account's, answers by what is known of its
/// commit: a refusal as it is, a definite database failure as unavailable, an
/// unproven commit (a missing backing row among them) as timed out and any
/// other definite failure as a failed request.
#[test]
fn every_save_answers_by_what_is_known_of_its_commit() {
    let room_id = rid("save-answers");
    for save in [
        control::Save::Sanction,
        control::Save::Ban,
        control::Save::Unban,
        control::Save::Role,
        control::Save::Settings,
        control::Save::Topic,
        control::Save::Kick,
        control::Save::ChatLook,
        control::Save::ReadMarker,
    ] {
        let refused = save.failure(&room_id, Refusal::Invalid("Fixture refusal".into()).into());
        assert_eq!(refused.code(), ErrorCode::Invalid, "{save:?}");
        assert_eq!(refused.client_message(), "Fixture refusal", "{save:?}");

        let definite = save.failure(&room_id, sqlx::Error::PoolTimedOut.into());
        assert_eq!(definite.code(), ErrorCode::Unavailable, "{save:?}");
        assert!(
            definite
                .client_message()
                .ends_with(" could not be saved; try again"),
            "{save:?}: {definite}"
        );

        let uncertain = save.failure(
            &room_id,
            sqlx::Error::Io(std::io::Error::from(std::io::ErrorKind::ConnectionReset)).into(),
        );
        assert_eq!(uncertain.code(), ErrorCode::TimedOut, "{save:?}");
        assert!(
            uncertain
                .client_message()
                .ends_with(" may not have been saved"),
            "{save:?}: {uncertain}"
        );

        // A missing backing row leaves the saved state unproven; a report
        // from another room arrives as a refusal and passes through above.
        let missing = save.failure(&room_id, sqlx::Error::RowNotFound.into());
        assert_eq!(missing.code(), ErrorCode::TimedOut, "{save:?}");

        let other = save.failure(&room_id, RoomError::PasswordRequired);
        assert_eq!(other.code(), ErrorCode::RequestFailed, "{save:?}");
        assert_eq!(
            other.client_message(),
            definite.client_message(),
            "{save:?}"
        );
    }
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
                    persistence
                        .await
                        .map_err(|error| RoomError::Internal(error.into()))?;
                    let room = manager.get_room(&id)?;
                    manager
                        .persist_room(&id, &room, ready(Ok::<(), RoomError>(())))
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
async fn role_changes_name_the_granting_participant() {
    with_fixture(|fixture| async move {
        let (member, mut member_messages) = participant(roles::Role::User);
        fixture
            .room
            .write()
            .await
            .participants
            .insert(member.id, member.clone());
        bounded(fixture.manager.set_participant_role(
            &fixture.room_id,
            &fixture.owner.id,
            &fixture.owner.sender,
            &member.id,
            roles::Role::Moderator,
        ))
        .await
        .unwrap();
        let frame = bounded(member_messages.recv()).await.unwrap();
        assert_eq!(
            frame.as_str(),
            format!(
                "{{\"type\":\"roleChanged\",\"participantId\":\"{}\",\"newRole\":\"moderator\",\
                 \"grantedBy\":\"{}\"}}",
                member.id, fixture.owner.id
            )
        );
        let ServerMessage::RoleChanged { granted_by, .. } = serde_json::from_str(&frame).unwrap()
        else {
            panic!("a role change broadcasts roleChanged");
        };
        assert_eq!(granted_by, fixture.owner.id);
        let named = frame.replace(&fixture.owner.id.to_string(), &fixture.owner.name);
        assert!(
            serde_json::from_str::<ServerMessage>(&named).is_err(),
            "grantedBy carries a participant id, never a name"
        );
    })
    .await;
}

#[tokio::test]
async fn chat_preference_ack_waits_for_inflight_durable_control() {
    with_fixture(|mut fixture| async move {
        // This proves the room-control ordering of a session preference, while
        // account preferences are authoritative in the database integration
        // fixtures. A synthetic account without a database must fail closed.
        fixture
            .room
            .write()
            .await
            .participants
            .get_mut(&fixture.owner.id)
            .unwrap()
            .authenticated = false;
        let held = control::lock_room(&fixture.room).await;
        let command = crate::signaling::protocol::ClientMessage::SetChatPreferences {
            request_id: "disable-private".into(),
            allow_private_messages: false,
            ignored_participant_ids: vec![],
        };
        let mut preference = std::pin::pin!(fixture.manager.handle_social_request(
            &fixture.room_id,
            &fixture.owner.id,
            &fixture.owner.sender,
            &command,
        ));
        assert!(
            futures_util::poll!(&mut preference).is_pending(),
            "consent cannot overtake the durable send's control phase"
        );
        assert!(
            fixture.owner_messages.try_recv().is_err(),
            "no early opt-out acknowledgement"
        );
        assert!(
            bounded(fixture.room.read())
                .await
                .participants
                .get(&fixture.owner.id)
                .unwrap()
                .social
                .accepts_inbox_from(&pid("peer"))
        );
        drop(held);
        bounded(preference).await.unwrap();
        let response: Value =
            serde_json::from_str(&bounded(fixture.owner_messages.recv()).await.unwrap()).unwrap();
        assert_eq!(response["action"], "setChatPreferences");
        assert_eq!(response["data"]["allowPrivateMessages"], false);
        assert!(
            !bounded(fixture.room.read())
                .await
                .participants
                .get(&fixture.owner.id)
                .unwrap()
                .social
                .accepts_inbox_from(&pid("peer"))
        );
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
        let replacement = runtime_room(fixture.room_id.as_str());
        fixture
            .manager
            .rooms
            .write()
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
            RoomError::from(sqlx::Error::PoolClosed),
            RoomError::from(sqlx::Error::PoolTimedOut),
            RoomError::from(sqlx::Error::InvalidArgument(
                "controlled fixture rejection".into(),
            )),
            Refusal::Invalid("controlled fixture refusal".into()).into(),
            RoomError::PasswordRequired,
        ] {
            assert!(!control::persistence_is_indeterminate(&error));
            assert!(
                fixture
                    .manager
                    .persist_room(
                        &fixture.room_id,
                        &fixture.room,
                        ready(Err::<(), RoomError>(error)),
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
            RoomError::from(sqlx::Error::Io(std::io::Error::new(
                std::io::ErrorKind::ConnectionReset,
                "fixture",
            ))),
            RoomError::from(sqlx::Error::Protocol("fixture".into())),
            RoomError::from(sqlx::Error::RowNotFound),
            // No persistence future fails this way; one that did would leave
            // its commit unproven.
            RoomError::Media(crate::media::MediaError::TransportError("fixture".into())),
            RoomError::Internal(anyhow::anyhow!("fixture")),
        ] {
            assert!(control::persistence_is_indeterminate(&error), "{error:?}");
        }
        let replacement = runtime_room(fixture.room_id.as_str());
        fixture
            .manager
            .rooms
            .write()
            .insert(fixture.room_id.clone(), replacement.clone());
        assert!(
            fixture
                .manager
                .persist_room(
                    &fixture.room_id,
                    &fixture.room,
                    ready(Err::<(), RoomError>(
                        sqlx::Error::Protocol("fixture".into()).into()
                    )),
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
                .contains_key(&fixture.room_id)
        );

        assert!(
            fixture
                .manager
                .persist_room(
                    &fixture.room_id,
                    &replacement,
                    ready(Err::<(), RoomError>(sqlx::Error::RowNotFound.into())),
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
    let room_id = rid(&format!("control-sql-{}", Uuid::new_v4()));
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
    let manager = RoomManager::new(
        crate::room::ManagerSettings::default(),
        config,
        ServerMetrics::new(),
        Some(pool.clone()),
    )
    .await
    .unwrap();
    let (owner, mut owner_messages) = participant(roles::Role::Owner);
    let (member, mut member_messages) = participant(roles::Role::User);
    let owner_id = Uuid::parse_str(&owner.id.to_string()).unwrap();
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
            Some(settings),
            true,
            hash,
        )));
        {
            let mut room = room.write().await;
            room.participants.insert(owner.id, owner.clone());
            room.participants.insert(member.id, member.clone());
        }
        manager.rooms.write().insert(room_id.clone(), room.clone());
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
        bounded(manager.send_social_chat(
            &room_id,
            &owner.id,
            &owner.sender,
            "chat remains available during owned SQL wait".into(),
            None,
            None,
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
            .persist_room(&room_id, installed_room.as_ref().unwrap(), async {
                Ok::<_, RoomError>(
                    sqlx::query("UPDATE rooms SET display_name=NULL WHERE id=$1")
                        .bind(&room_id)
                        .execute(&pool)
                        .await?,
                )
            })
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            RoomError::Database(sqlx::Error::Database(_))
        ));
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

/// A persisted room for the refusal tests: an owner and a member with
/// accounts, a guest, the room row with saved history, a second room whose
/// reports this one must not answer, and the live room all three are in.
struct DatabaseRoom {
    pool: sqlx::PgPool,
    manager: RoomManager,
    room_id: RoomId,
    other_room_id: RoomId,
    room: Arc<TokioRwLock<Room>>,
    owner: Participant,
    member: Participant,
    guest: Participant,
}

impl DatabaseRoom {
    /// A definite refusal leaves the room live: not quarantined, its ID not
    /// reserved, and the same incarnation still mapped.
    async fn assert_live(&self) {
        assert!(!self.room.read().await.deleting, "the room was quarantined");
        assert!(
            !self
                .manager
                .deleting_rooms
                .read()
                .contains_key(&self.room_id)
        );
        assert!(
            self.manager
                .rooms
                .read()
                .get(&self.room_id)
                .is_some_and(|live| Arc::ptr_eq(live, &self.room))
        );
    }

    /// An open report filed in the other room about the member.
    async fn foreign_report(&self) -> Uuid {
        let report = Uuid::new_v4();
        sqlx::query("INSERT INTO room_reports(id,room_id,reporter_id,reporter_name,target_participant_id,target_name,reason) VALUES($1,$2,$3,'Reporter',$4,'Member','spam')")
            .bind(report)
            .bind(&self.other_room_id)
            .bind(self.owner.id.to_string())
            .bind(self.member.id.to_string())
            .execute(&self.pool)
            .await
            .unwrap();
        report
    }

    async fn report_status(&self, report: Uuid) -> String {
        sqlx::query_scalar("SELECT status FROM room_reports WHERE id=$1")
            .bind(report)
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }

    /// A public message by `author`, sent from `sender_session`, saved to the
    /// room's history; the live room does not hold it.
    async fn save_message(
        &self,
        author: &Participant,
        sender_session: Uuid,
        content: &str,
    ) -> crate::signaling::protocol::ChatEntry {
        let message = crate::signaling::protocol::ChatEntry {
            attachments: Vec::new(),
            message_id: MessageId::random(),
            client_message_id: Uuid::new_v4().to_string(),
            participant_id: author.id,
            participant_name: author.name.clone(),
            recipient_id: None,
            account_conversation: None,
            recipient_name: None,
            content: content.into(),
            sent_at: chrono::Utc::now(),
            removed_at: None,
            revision: 0,
            edited_at: None,
            chat_style: Default::default(),
            reply_to: None,
            reactions: Vec::new(),
        };
        history::persist_message(
            &self.pool,
            Some(self.room_id.as_str()),
            sender_session,
            author.authenticated,
            7,
            &message,
            &[],
        )
        .await
        .unwrap()
    }

    /// `editor` edits `message` as its socket would; the tests expect a
    /// refusal.
    async fn edit(&self, editor: &Participant, message: MessageId) -> RoomError {
        self.manager
            .handle_social_request(
                &self.room_id,
                &editor.id,
                &editor.sender,
                &crate::signaling::protocol::ClientMessage::EditChatMessage {
                    request_id: "edit".into(),
                    message_id: message,
                    content: "Edited".into(),
                    expected_revision: 0,
                },
            )
            .await
            .unwrap_err()
    }

    /// The saved body of `message`, expired or not.
    async fn stored_body(&self, message: MessageId) -> Value {
        sqlx::query_scalar("SELECT body FROM chat_messages WHERE id=$1")
            .bind(message.as_uuid())
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }
}

/// Native construction and a disposable database; cleanup runs even when
/// assertions or the outer deadline fail.
async fn with_database_room<F, Fut>(test: F)
where
    F: FnOnce(DatabaseRoom) -> Fut,
    Fut: Future<Output = ()>,
{
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
            .is_some_and(|database| database.ends_with("_test"))
    );
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .acquire_timeout(Duration::from_secs(3))
        .connect_with(options)
        .await
        .unwrap();
    let mut config = MediaConfig::default();
    config.worker_config.num_workers = 1;
    config.webrtc_server_port_base = 0;
    let manager = RoomManager::new(
        crate::room::ManagerSettings::default(),
        config,
        ServerMetrics::new(),
        Some(pool.clone()),
    )
    .await
    .unwrap();
    let (owner, _owner_messages) = participant(roles::Role::Owner);
    let (member, _member_messages) = participant(roles::Role::User);
    let (mut guest, _guest_messages) = participant(roles::Role::Guest);
    guest.authenticated = false;
    let accounts = [owner.id.as_uuid(), member.id.as_uuid()];
    let room_id = rid(&format!("refusal-{}", Uuid::new_v4()));
    let other_room_id = rid(&format!("refusal-other-{}", Uuid::new_v4()));
    let outcome = AssertUnwindSafe(tokio::time::timeout(Duration::from_secs(30), async {
        for account in accounts {
            sqlx::query("INSERT INTO users(id,email,display_name) VALUES($1,$2,'Refusal tester')")
                .bind(account)
                .bind(format!("{account}@refusal.invalid"))
                .execute(&pool)
                .await
                .unwrap();
        }
        for id in [&room_id, &other_room_id] {
            sqlx::query(
                "INSERT INTO rooms(id,owner_id,display_name,history_retention_days) VALUES($1,$2,$1,7)",
            )
            .bind(id)
            .bind(accounts[0])
            .execute(&pool)
            .await
            .unwrap();
        }
        let (settings, hash) = settings::load_room(&pool, &room_id).await.unwrap().unwrap();
        let room = Arc::new(TokioRwLock::new(Room::new(
            room_id.clone(),
            Some(settings),
            true,
            hash,
        )));
        {
            let mut state = room.write().await;
            state.participants.insert(owner.id, owner.clone());
            state.participants.insert(member.id, member.clone());
            state.participants.insert(guest.id, guest.clone());
        }
        manager.rooms.write().insert(room_id.clone(), room.clone());
        test(DatabaseRoom {
            pool: pool.clone(),
            manager: manager.clone(),
            room_id: room_id.clone(),
            other_room_id: other_room_id.clone(),
            room,
            owner: owner.clone(),
            member: member.clone(),
            guest: guest.clone(),
        })
        .await;
    }))
    .catch_unwind()
    .await;
    let room_cleanup = tokio::time::timeout(Duration::from_secs(10), manager.shutdown()).await;
    let row_cleanup = tokio::time::timeout(Duration::from_secs(10), async {
        for id in [&room_id, &other_room_id] {
            sqlx::query("DELETE FROM rooms WHERE id=$1 AND owner_id=$2")
                .bind(id)
                .bind(accounts[0])
                .execute(&pool)
                .await?;
        }
        for account in accounts {
            sqlx::query("DELETE FROM users WHERE id=$1")
                .bind(account)
                .execute(&pool)
                .await?;
        }
        Ok::<(), sqlx::Error>(())
    })
    .await;
    let media_cleanup =
        tokio::time::timeout(Duration::from_secs(10), manager.media_server.shutdown()).await;
    let pool_cleanup = tokio::time::timeout(Duration::from_secs(5), pool.close()).await;
    match outcome {
        Err(panic) => std::panic::resume_unwind(panic),
        Ok(result) => result.expect("database refusal fixture timed out"),
    }
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

/// Pinning a message that was removed writes nothing: the moderator is told
/// it is unavailable, as in a room without saved history, and the room stays
/// live.
#[tokio::test]
#[ignore = "requires migrated disposable TEST_DATABASE_URL and mediasoup worker"]
async fn pinning_a_removed_message_is_refused_and_leaves_the_room_live() {
    with_database_room(|fixture| async move {
        let message = fixture
            .save_message(&fixture.owner, Uuid::new_v4(), "Soon removed")
            .await;
        let mut transaction = fixture.pool.begin().await.unwrap();
        history::remove_public(
            &mut transaction,
            fixture.room_id.as_str(),
            &message.message_id,
            chrono::Utc::now(),
        )
        .await
        .unwrap();
        transaction.commit().await.unwrap();

        let error = fixture
            .manager
            .handle_social_request(
                &fixture.room_id,
                &fixture.owner.id,
                &fixture.owner.sender,
                &crate::signaling::protocol::ClientMessage::SetPinnedMessage {
                    request_id: "pin".into(),
                    message_id: message.message_id,
                    pinned: true,
                },
            )
            .await
            .unwrap_err();
        assert_eq!(error.code(), ErrorCode::Unavailable, "{error:?}");
        assert_eq!(error.client_message(), "Public message unavailable");
        fixture.assert_live().await;
    })
    .await;
}

/// A kick citing another room's report writes nothing: the moderator is told
/// the report is not found, the member stays, the report stays open and the
/// room stays live.
#[tokio::test]
#[ignore = "requires migrated disposable TEST_DATABASE_URL and mediasoup worker"]
async fn a_kick_citing_another_rooms_report_is_refused_and_leaves_the_room_live() {
    with_database_room(|fixture| async move {
        let report = fixture.foreign_report().await;
        let error = fixture
            .manager
            .kick_participant(
                &fixture.room_id,
                &fixture.owner.id,
                &fixture.owner.sender,
                &fixture.member.id,
                None,
                Some(report),
            )
            .await
            .unwrap_err();
        assert_eq!(error.code(), ErrorCode::NotFound, "{error:?}");
        assert_eq!(error.client_message(), "Report not found");
        fixture.assert_live().await;
        assert!(
            fixture
                .room
                .read()
                .await
                .participants
                .contains_key(&fixture.member.id)
        );
        assert_eq!(fixture.report_status(report).await, "open");
    })
    .await;
}

/// A ban citing another room's report writes nothing: no ban row, the report
/// stays open, and the room stays live.
#[tokio::test]
#[ignore = "requires migrated disposable TEST_DATABASE_URL and mediasoup worker"]
async fn a_ban_citing_another_rooms_report_is_refused_and_leaves_the_room_live() {
    with_database_room(|fixture| async move {
        let report = fixture.foreign_report().await;
        let error = fixture
            .manager
            .ban_participant(
                &fixture.room_id,
                &fixture.owner.id,
                &fixture.owner.sender,
                &fixture.member.id,
                Some("spam"),
                None,
                Some(report),
            )
            .await
            .unwrap_err();
        assert_eq!(error.code(), ErrorCode::NotFound, "{error:?}");
        assert_eq!(error.client_message(), "Report not found");
        fixture.assert_live().await;
        assert!(
            fixture
                .room
                .read()
                .await
                .participants
                .contains_key(&fixture.member.id)
        );
        assert_eq!(fixture.report_status(report).await, "open");
        assert!(
            !moderation::is_banned(
                &fixture.pool,
                &fixture.room_id,
                Some(fixture.member.id.as_uuid()),
                None,
                true,
            )
            .await
            .unwrap()
        );
    })
    .await;
}

/// The live room still holds a member's message whose saved row has expired,
/// as it does once the retention period passes or after the room shortens its
/// retention: the edit writes nothing, the author is told the message is
/// unavailable, and the room stays live.
#[tokio::test]
#[ignore = "requires migrated disposable TEST_DATABASE_URL and mediasoup worker"]
async fn editing_a_message_whose_saved_row_expired_is_refused_and_leaves_the_room_live() {
    with_database_room(|fixture| async move {
        let client_message_id = Uuid::new_v4().to_string();
        let sequence = fixture.room.read().await.social.next_sequence;
        fixture
            .manager
            .send_social_chat(
                &fixture.room_id,
                &fixture.member.id,
                &fixture.member.sender,
                "Soon expired".into(),
                Some(client_message_id.clone()),
                None,
            )
            .await
            .unwrap();
        // The room remembers the message, so the edit is checked in memory
        // and only then saved.
        assert_eq!(fixture.room.read().await.social.next_sequence, sequence + 1);
        let message: Uuid = sqlx::query_scalar(
            "UPDATE chat_messages SET expires_at=now()-interval '1 second'
             WHERE room_id=$1 AND client_message_id=$2 RETURNING id",
        )
        .bind(&fixture.room_id)
        .bind(&client_message_id)
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
        let message = MessageId::from(message);
        let saved = fixture.stored_body(message).await;

        let error = fixture.edit(&fixture.member, message).await;
        assert_eq!(error.code(), ErrorCode::Unavailable, "{error:?}");
        assert_eq!(error.client_message(), "Message unavailable");
        fixture.assert_live().await;
        assert_eq!(fixture.stored_body(message).await, saved);
    })
    .await;
}

/// A guest keeps its participant ID when it leaves and joins again on the
/// same socket, but each membership has a new media session, and a guest's
/// saved message belongs to the session that sent it. Once the live room no
/// longer holds the message, the edit is checked against the saved row alone:
/// it writes nothing, the guest is told only the author can edit, and the
/// room stays live.
#[tokio::test]
#[ignore = "requires migrated disposable TEST_DATABASE_URL and mediasoup worker"]
async fn a_guest_editing_an_earlier_memberships_message_is_refused_and_leaves_the_room_live() {
    with_database_room(|fixture| async move {
        // Sent from the guest's earlier membership.
        let message = fixture
            .save_message(&fixture.guest, Uuid::new_v4(), "Earlier membership")
            .await;
        let saved = fixture.stored_body(message.message_id).await;

        let error = fixture.edit(&fixture.guest, message.message_id).await;
        assert_eq!(error.code(), ErrorCode::Forbidden, "{error:?}");
        assert_eq!(
            error.client_message(),
            "Only the author can edit this message"
        );
        fixture.assert_live().await;
        assert_eq!(fixture.stored_body(message.message_id).await, saved);
    })
    .await;
}
