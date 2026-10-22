//! Existing inert fork fixture plus a deterministic in-process action handshake.

use super::*;
use crate::test_env::Companion;

struct Hook<'a> {
    fixture: &'a Fixture,
    fired: bool,
    failure: Option<u8>,
    ended: bool,
}
impl Companion for Hook<'_> {
    fn workload_ended(&mut self) {
        self.ended = true;
    }
    async fn wait(&self) {
        if self.fired {
            std::future::pending::<()>().await;
        }
        ready(&self.fixture.root.join("leader")).await;
    }
    async fn advance(&mut self, signals: &mut Signals) -> anyhow::Result<()> {
        self.fired = true;
        assert!(signals.pending().await.is_none());
        assert!(owned_process::reserved(
            rustix::process::Pid::from_raw(self.fixture.pid("leader")).unwrap()
        ));
        if let Some(code) = self.failure {
            return Err(crate::test_env::Stop {
                code,
                message: "Owned companion failed.".into(),
            }
            .into());
        }
        self.fixture.release();
        Ok(())
    }
    async fn finish(&mut self, status: i32, _: &mut Signals) -> i32 {
        status
    }
}

#[tokio::test]
async fn companion_action_borrows_same_signals_while_workload_group_is_reserved() {
    for code in [0, 23] {
        let fixture = Fixture::new();
        let mut hook = Hook {
            fixture: &fixture,
            fired: false,
            failure: None,
            ended: false,
        };
        let result = run_test_child_with_companion(
            fixture.command(code, false),
            Duration::from_millis(20),
            &mut signals(),
            &mut hook,
        )
        .await
        .unwrap();
        assert_eq!(result, code);
        assert!(hook.fired);
        assert!(hook.ended);
        retired(fixture.pid("leader")).await;
        retired(fixture.pid("worker")).await;
    }
}

#[tokio::test]
async fn companion_known_failures_survive_unproven_group_cleanup() {
    for code in [23, 124, 130, 143] {
        let fixture = Fixture::new();
        let mut hook = Hook {
            fixture: &fixture,
            fired: false,
            failure: Some(code),
            ended: false,
        };
        let error = run_child(
            fixture.command(0, false),
            Duration::from_millis(20),
            &mut signals(),
            |pid| async move {
                let _ = owned_process::finish(pid).await;
                false
            },
            &mut hook,
        )
        .await
        .unwrap_err();
        assert_eq!(
            error.downcast_ref::<crate::test_env::Stop>().unwrap().code,
            code
        );
        assert!(hook.fired);
        // Real retirement precedes the injected false proof and leader reap.
        retired(fixture.pid("leader")).await;
        retired(fixture.pid("worker")).await;
    }
}
