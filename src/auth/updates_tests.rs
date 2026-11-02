use super::*;
use serde_json::json;

#[test]
fn account_update_queues_are_scoped_bounded_and_release_all_leases() {
    let hub = Hub::with_capacity(ACCOUNT_STREAMS + 1);
    let account = Uuid::new_v4();
    let peer = Uuid::new_v4();
    let mut subscriptions: Vec<_> = (0..ACCOUNT_STREAMS)
        .map(|_| hub.subscribe(UserId::from(account)).unwrap())
        .collect();
    assert!(matches!(
        hub.subscribe(UserId::from(account)),
        Err(AuthError::RateLimited)
    ));
    let mut other = hub.subscribe(UserId::from(peer)).unwrap();
    assert!(matches!(
        hub.subscribe(UserId::random()),
        Err(AuthError::ServiceBusy)
    ));
    hub.changed(account, Scope::Preferences, None);
    for subscription in &mut subscriptions {
        assert!(matches!(
            subscription.receiver.try_recv(),
            Ok(Notice::Change(Update::Change {
                scope: Scope::Preferences,
                peer_id: None
            }))
        ));
    }
    assert!(matches!(
        other.receiver.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));
    drop(subscriptions);
    let state = hub.inner.state.lock();
    assert_eq!(state.total, 1);
    assert!(!state.accounts.contains_key(&account));
    drop(state);
    drop(other);
    assert_eq!(hub.inner.state.lock().total, 0);
    assert!(hub.inner.state.lock().accounts.is_empty());
}

#[test]
fn slow_account_updates_report_a_gap_without_an_unbounded_backlog() {
    let hub = Hub::with_capacity(1);
    let account = Uuid::new_v4();
    let mut subscription = hub.subscribe(UserId::from(account)).unwrap();
    for _ in 0..=QUEUE {
        hub.changed(account, Scope::Inbox, None);
    }
    assert!(matches!(
        subscription.receiver.try_recv(),
        Err(broadcast::error::TryRecvError::Lagged(_))
    ));
    assert_eq!(subscription.receiver.len(), QUEUE);
}

#[test]
fn account_update_wire_payloads_are_content_free_and_pairs_are_private() {
    assert_eq!(
        serde_json::to_value(Update::Ready).unwrap(),
        json!({"type":"ready"})
    );
    let account = Uuid::new_v4();
    let peer = Uuid::new_v4();
    let hub = Hub::with_capacity(3);
    let mut own = hub.subscribe(UserId::from(account)).unwrap();
    let mut other = hub.subscribe(UserId::from(peer)).unwrap();
    let mut outsider = hub.subscribe(UserId::random()).unwrap();
    hub.pair(account, peer, Scope::Inbox);
    for (subscription, expected) in [(&mut own, peer), (&mut other, account)] {
        let Notice::Change(update) = subscription.receiver.try_recv().unwrap() else {
            panic!("change")
        };
        assert_eq!(
            serde_json::to_value(update).unwrap(),
            json!({"type":"change","scope":"inbox","peerId":expected})
        );
    }
    assert!(matches!(
        outsider.receiver.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));
}

#[tokio::test]
async fn account_update_absolute_lifetime_closes_a_quiet_stream_without_replaying_ready() {
    let hub = Hub::with_capacity(1);
    let account = Uuid::new_v4();
    // No query is made: this connection already completed validation and its
    // absolute deadline arrives before the next validation interval.
    let pool = sqlx::postgres::PgPoolOptions::new()
        .connect_lazy_with(sqlx::postgres::PgConnectOptions::new());
    let mut connection = Connection {
        subscription: hub.subscribe(UserId::from(account)).unwrap(),
        principal: Principal {
            user: UserId::from(account),
            sid: Uuid::new_v4(),
            auth_version: 0,
            expires_at: chrono::Utc::now() + chrono::TimeDelta::seconds(600),
        },
        pool,
        drain: DrainSignal::default(),
        next_validation: Instant::now() + REVALIDATE,
        deadline: Instant::now() + Duration::from_millis(10),
        validated_generation: 0,
        ready: false,
    };
    assert_eq!(LIFETIME, Duration::from_secs(300));
    assert!(
        tokio::time::timeout(Duration::from_secs(1), connection.next())
            .await
            .unwrap()
            .is_none()
    );
    drop(connection);
    assert_eq!(hub.inner.state.lock().total, 0);
}

#[tokio::test]
async fn socket_revalidation_reaches_only_its_account_and_leaves_with_the_last_subscriber() {
    // Sockets are bounded by the connection limit, not by the stream budget.
    let hub = Hub::with_capacity(0);
    let account = Uuid::new_v4();
    let mut first = hub.subscribe_revalidation(UserId::from(account));
    let mut second = hub.subscribe_revalidation(UserId::from(account));
    let mut outsider = hub.subscribe_revalidation(UserId::random());
    assert!(matches!(
        hub.subscribe(UserId::from(account)),
        Err(AuthError::ServiceBusy)
    ));
    hub.revalidate(UserId::from(account));
    for notices in [&mut first, &mut second] {
        tokio::time::timeout(Duration::from_secs(1), notices.notice())
            .await
            .expect("every socket of the account is told to re-check");
    }
    assert!(matches!(
        outsider.receiver.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));
    drop(first);
    assert_eq!(hub.inner.state.lock().sockets[&account].subscribers, 1);
    drop(second);
    assert!(
        !hub.inner.state.lock().sockets.contains_key(&account),
        "the account's entry goes with its last subscriber"
    );
    drop(outsider);
    assert!(hub.inner.state.lock().sockets.is_empty());
    assert_eq!(hub.inner.state.lock().total, 0);
}
