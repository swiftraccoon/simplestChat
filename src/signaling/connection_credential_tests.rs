//! Deterministic credential-observation tests; no database or network failures.

use super::*;
use std::future::{Future, pending, ready};
use tokio::sync::broadcast;

struct ValidationFixture {
    claims: Claims,
    continuity: CredentialContinuity,
    notices: Option<broadcast::Sender<(String, i64)>>,
    receiver: broadcast::Receiver<(String, i64)>,
    drain: crate::shutdown::DrainSignal,
}

impl ValidationFixture {
    fn new() -> Self {
        let (notices, receiver) = broadcast::channel(1);
        Self {
            claims: Claims {
                sub: "credential-observer-account".to_string(),
                name: "Credential observer".to_string(),
                iss: "simplestchat".to_string(),
                aud: "simplestchat".to_string(),
                exp: unix_seconds() as usize + 60,
                auth_version: 2,
            },
            continuity: CredentialContinuity::default(),
            notices: Some(notices),
            receiver,
            drain: crate::shutdown::DrainSignal::default(),
        }
    }

    async fn observe(
        &mut self,
        validation: impl Future<Output = CredentialStatus>,
        hard_deadline: Instant,
    ) -> Option<bool> {
        tokio::time::timeout(
            Duration::from_secs(1),
            observe_account_validation(
                validation,
                &self.claims,
                &mut self.continuity,
                &mut self.receiver,
                &self.drain,
                hard_deadline,
            ),
        )
        .await
        .expect("Credential observation must finish within the owned test deadline")
    }

    async fn status(&mut self, status: CredentialStatus) -> Option<bool> {
        self.observe(ready(status), Instant::now() + Duration::from_secs(30))
            .await
    }
}

#[tokio::test]
async fn unavailable_validation_preserves_the_active_budget_across_disconnect_and_grace() {
    let mut fixture = ValidationFixture::new();
    assert!(fixture.continuity.observe(
        CredentialStatus::Unavailable,
        Instant::now() - Duration::from_secs(3),
    ));
    let original = fixture.continuity.deadline().unwrap();
    assert_eq!(
        fixture.status(CredentialStatus::Unavailable).await,
        Some(true)
    );
    assert_eq!(fixture.continuity.deadline(), Some(original));

    // Disconnect transfers established state; it must not construct a fresh
    // uncertainty allowance for the retained membership.
    let active = fixture.continuity.clone();
    fixture.continuity = active.clone();
    assert_eq!(
        fixture.status(CredentialStatus::Unavailable).await,
        Some(true)
    );
    assert_eq!(fixture.continuity.deadline(), Some(original));
    assert_eq!(active.deadline(), Some(original));
    assert!(fixture.continuity.unavailable());
}

#[tokio::test]
async fn current_validation_recovers_before_the_original_uncertainty_deadline() {
    let mut fixture = ValidationFixture::new();
    assert_eq!(
        fixture.status(CredentialStatus::Unavailable).await,
        Some(true)
    );
    let unavailable = fixture.continuity.clone();
    assert_eq!(fixture.status(CredentialStatus::Current).await, Some(true));
    assert!(fixture.continuity.deadline().is_none());
    assert!(!fixture.continuity.unavailable());
    assert!(
        unavailable.deadline().is_some(),
        "Recovery must not mutate a detached clone"
    );
    assert_eq!(
        fixture.status(CredentialStatus::Unavailable).await,
        Some(true)
    );
    assert!(fixture.continuity.unavailable());
}

#[tokio::test]
async fn queued_authoritative_revocation_wins_without_polling_ready_validation() {
    let mut fixture = ValidationFixture::new();
    for notice in [
        ("unrelated-account".to_string(), i64::MAX),
        (fixture.claims.sub.clone(), fixture.claims.auth_version),
    ] {
        fixture.notices.as_ref().unwrap().send(notice).unwrap();
        assert_eq!(fixture.status(CredentialStatus::Current).await, Some(true));
    }
    fixture
        .notices
        .as_ref()
        .unwrap()
        .send((fixture.claims.sub.clone(), fixture.claims.auth_version + 1))
        .unwrap();
    let polled = AtomicBool::new(false);
    let validation = async {
        polled.store(true, Ordering::Relaxed);
        CredentialStatus::Current
    };
    assert_eq!(
        fixture
            .observe(validation, Instant::now() + Duration::from_secs(30))
            .await,
        Some(false)
    );
    assert!(!polled.load(Ordering::Relaxed));
    assert!(fixture.continuity.expired(Instant::now()));
}

#[tokio::test]
async fn revocation_inside_a_query_interrupts_pending_work_and_wins_over_ready_current() {
    for stay_pending in [false, true] {
        let mut fixture = ValidationFixture::new();
        let notices = fixture.notices.as_ref().unwrap().clone();
        let subject = fixture.claims.sub.clone();
        let version = fixture.claims.auth_version + 1;
        let validation = async move {
            notices.send((subject, version)).unwrap();
            if stay_pending {
                pending::<()>().await;
            }
            CredentialStatus::Current
        };
        assert_eq!(
            fixture
                .observe(validation, Instant::now() + Duration::from_secs(30))
                .await,
            Some(false)
        );
        assert!(fixture.continuity.expired(Instant::now()));
    }
}

#[tokio::test]
async fn revocation_notice_loss_during_a_query_cannot_be_cleared_by_its_ready_result() {
    for stay_pending in [false, true] {
        let mut fixture = ValidationFixture::new();
        let notices = fixture.notices.as_ref().unwrap().clone();
        let validation = async move {
            // Capacity one makes this an exact lag, without scheduling or sleeps.
            notices.send(("other-account-a".to_string(), 1)).unwrap();
            notices.send(("other-account-b".to_string(), 1)).unwrap();
            if stay_pending {
                pending::<()>().await;
            }
            CredentialStatus::Current
        };
        assert_eq!(
            fixture
                .observe(validation, Instant::now() + Duration::from_secs(30))
                .await,
            Some(false)
        );
        assert!(fixture.continuity.expired(Instant::now()));
    }
}

#[tokio::test]
async fn closed_revocation_channel_wins_before_or_during_ready_validation() {
    for close_during_query in [false, true] {
        let mut fixture = ValidationFixture::new();
        let notices = fixture.notices.take().unwrap();
        let notices = if close_during_query {
            Some(notices)
        } else {
            drop(notices);
            None
        };
        let validation = async move {
            drop(notices);
            CredentialStatus::Current
        };
        assert_eq!(
            fixture
                .observe(validation, Instant::now() + Duration::from_secs(30))
                .await,
            Some(false)
        );
        assert!(fixture.continuity.expired(Instant::now()));
    }
}

#[tokio::test]
async fn known_prequery_notice_loss_requires_current_proof_not_uncertainty_allowance() {
    let mut fixture = ValidationFixture::new();
    fixture.continuity.require_revalidation();
    let mut unavailable = ValidationFixture::new();
    unavailable.continuity = fixture.continuity.clone();
    assert_eq!(fixture.status(CredentialStatus::Current).await, Some(true));
    assert_eq!(
        unavailable.status(CredentialStatus::Unavailable).await,
        Some(false)
    );
    assert!(unavailable.continuity.expired(Instant::now()));
    assert_eq!(
        unavailable.status(CredentialStatus::Current).await,
        Some(false)
    );
    assert_eq!(
        fixture.status(CredentialStatus::Unavailable).await,
        Some(true)
    );
}

#[tokio::test]
async fn expired_claims_hard_deadline_or_uncertainty_reject_even_ready_current_validation() {
    for expired_boundary in ["jwt", "retained-grace", "uncertainty"] {
        let mut fixture = ValidationFixture::new();
        let mut deadline = Instant::now() + Duration::from_secs(30);
        match expired_boundary {
            "jwt" => fixture.claims.exp = unix_seconds() as usize,
            "retained-grace" => deadline = Instant::now() - Duration::from_millis(1),
            "uncertainty" => {
                assert!(fixture.continuity.observe(
                    CredentialStatus::Unavailable,
                    Instant::now() - Duration::from_secs(16),
                ));
            }
            _ => unreachable!(),
        }
        let polled = AtomicBool::new(false);
        let validation = async {
            polled.store(true, Ordering::Relaxed);
            CredentialStatus::Current
        };
        assert_eq!(fixture.observe(validation, deadline).await, Some(false));
        assert!(
            !polled.load(Ordering::Relaxed),
            "Expired {expired_boundary} must skip the query"
        );
    }
}

#[tokio::test]
async fn drain_wins_over_ready_validation_and_interrupts_an_inflight_pending_query() {
    let mut fixture = ValidationFixture::new();
    fixture.drain.begin_draining();
    assert_eq!(fixture.status(CredentialStatus::Current).await, None);

    for stay_pending in [false, true] {
        let mut fixture = ValidationFixture::new();
        assert_eq!(
            fixture.status(CredentialStatus::Unavailable).await,
            Some(true)
        );
        let original_deadline = fixture.continuity.deadline();
        let drain = fixture.drain.clone();
        let validation = async move {
            drain.begin_draining();
            if stay_pending {
                pending::<()>().await;
            }
            CredentialStatus::Current
        };
        assert_eq!(
            fixture
                .observe(validation, Instant::now() + Duration::from_secs(30))
                .await,
            None
        );
        assert_eq!(fixture.continuity.deadline(), original_deadline);
        assert!(
            fixture.continuity.unavailable(),
            "Drain must not commit the Current result"
        );
    }
}

#[tokio::test]
async fn pending_validation_is_bounded_by_the_original_hard_deadline() {
    let mut fixture = ValidationFixture::new();
    let deadline = Instant::now() + Duration::from_millis(10);
    assert_eq!(fixture.observe(pending(), deadline).await, Some(false));
    assert!(Instant::now() >= deadline);
    assert!(fixture.continuity.deadline().is_none());
}

fn grace_entry_with_timer(token: &str, timer: tokio::task::JoinHandle<()>) -> GraceEntry {
    let (sender, _receiver) = mpsc::channel(1);
    GraceEntry {
        reconnect_token: token.to_string(),
        authenticated_subject: Some("credential-observer-account".to_string()),
        authenticated_version: Some(2),
        media_rate_state: MediaSessionRateState::new(),
        sender,
        timer,
    }
}

#[tokio::test]
async fn grace_activation_publishes_the_entry_before_immediately_ready_cleanup() {
    let map = GracePeriodMap::new();
    let owned_map = map.clone();
    let (activate, activation) = tokio::sync::oneshot::channel();
    let (started, startup) = tokio::sync::oneshot::channel();
    let (finished, completion) = tokio::sync::oneshot::channel();
    let timer = tokio::spawn(async move {
        let _ = started.send(());
        if activation.await.is_err() {
            return;
        }
        // Cleanup is ready immediately after activation, as with a retained
        // session whose credential or grace deadline has already expired.
        let entry = owned_map.remove_if_token_matches(
            "credential-room",
            "credential-participant",
            "accepted-reconnect-token",
            Some("credential-observer-account"),
        );
        let _ = finished.send(entry.is_some());
    });
    tokio::time::timeout(Duration::from_secs(1), startup)
        .await
        .unwrap()
        .unwrap();
    assert!(map.inner.read().unwrap().is_empty());
    assert!(map.insert_activated(
        "credential-room".to_string(),
        "credential-participant".to_string(),
        grace_entry_with_timer("accepted-reconnect-token", timer),
        activate,
    ));
    assert!(
        tokio::time::timeout(Duration::from_secs(1), completion)
            .await
            .unwrap()
            .unwrap(),
        "An activated cleanup must find and remove its already-published entry"
    );
    assert!(map.inner.read().unwrap().is_empty());
}

#[tokio::test]
async fn rejected_grace_insertion_aborts_the_owned_timer_without_activating_cleanup() {
    let map = GracePeriodMap::new();
    assert_eq!(map.close(), 0);
    let activated = Arc::new(AtomicBool::new(false));
    let cleanup_activated = activated.clone();
    let (activate, activation) = tokio::sync::oneshot::channel();
    let (started, startup) = tokio::sync::oneshot::channel();
    let (finished, completion) = tokio::sync::oneshot::channel();
    let timer = tokio::spawn(async move {
        let _ = started.send(());
        if activation.await.is_ok() {
            cleanup_activated.store(true, Ordering::Relaxed);
        }
        let _ = finished.send(());
    });
    tokio::time::timeout(Duration::from_secs(1), startup)
        .await
        .unwrap()
        .unwrap();
    let abort = timer.abort_handle();
    assert!(!map.insert_activated(
        "credential-room".to_string(),
        "credential-participant".to_string(),
        grace_entry_with_timer("rejected-reconnect-token", timer),
        activate,
    ));
    // Cancellation drops the completion sender; normal completion can also
    // observe activation rejection. Either must finish without doing cleanup.
    let _ = tokio::time::timeout(Duration::from_secs(1), completion)
        .await
        .expect("Rejected grace timer must stop");
    assert!(abort.is_finished());
    assert!(!activated.load(Ordering::Relaxed));
    assert!(map.inner.read().unwrap().is_empty());
}
