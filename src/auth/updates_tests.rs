use super::*;
use serde_json::json;

#[test]
fn account_update_queues_are_scoped_bounded_and_release_all_leases() {
    let hub = Hub::with_capacity(ACCOUNT_STREAMS + 1);
    let account = Uuid::new_v4();
    let peer = Uuid::new_v4();
    let mut subscriptions: Vec<_> = (0..ACCOUNT_STREAMS)
        .map(|_| hub.subscribe(account).unwrap())
        .collect();
    assert!(matches!(
        hub.subscribe(account),
        Err(AuthError::RateLimited)
    ));
    let mut other = hub.subscribe(peer).unwrap();
    assert!(matches!(
        hub.subscribe(Uuid::new_v4()),
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
    let state = hub.inner.state.lock().unwrap();
    assert_eq!(state.total, 1);
    assert!(!state.accounts.contains_key(&account));
    drop(state);
    drop(other);
    assert_eq!(hub.inner.state.lock().unwrap().total, 0);
    assert!(hub.inner.state.lock().unwrap().accounts.is_empty());
}

#[test]
fn slow_account_updates_report_a_gap_without_an_unbounded_backlog() {
    let hub = Hub::with_capacity(1);
    let account = Uuid::new_v4();
    let mut subscription = hub.subscribe(account).unwrap();
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
    let mut own = hub.subscribe(account).unwrap();
    let mut other = hub.subscribe(peer).unwrap();
    let mut outsider = hub.subscribe(Uuid::new_v4()).unwrap();
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
        subscription: hub.subscribe(account).unwrap(),
        claims: Claims {
            sub: account.to_string(),
            name: String::new(),
            iss: "simplestchat".into(),
            aud: "simplestchat".into(),
            exp: (chrono::Utc::now().timestamp() + 600) as usize,
            auth_version: 0,
            sid: Uuid::new_v4(),
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
    assert_eq!(hub.inner.state.lock().unwrap().total, 0);
}
