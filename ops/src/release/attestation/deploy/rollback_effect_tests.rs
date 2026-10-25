//! Physical retained-record refusals and passive recovery facts, without a daemon.

use super::*;
use std::{fs, io, path::PathBuf};

struct Daemon {
    calls: usize,
    attempt: Option<PathBuf>,
    error: Option<Cause>,
    changed_log: io::Result<()>,
}
impl Executor for Daemon {
    async fn run(&mut self, _owner: &mut Owner, _request: Invocation<'_>) -> CommandOutput {
        self.calls += 1;
        if let Some(attempt) = &self.attempt {
            self.changed_log =
                fs::rename(attempt.join("001.stderr"), attempt.join("original-stderr"));
        }
        CommandOutput {
            stdout: Vec::new(),
            stderr: b"private fixture error".to_vec(),
            status: Some(7),
            error: self.error,
            settled: true,
            stdout_complete: true,
            bulk_bytes: 0,
            bulk_sha256: String::new(),
            bulk_complete: false,
        }
    }
}

#[test]
fn recovery_refusal_precedes_evidence_failure_without_replacing_original_primary() {
    for (pre_changed, error) in [
        (true, None),
        (false, None),
        (false, Some(Cause::Command)),
        (false, Some(Cause::Io)),
    ] {
        let data = effect_tests::Data::new();
        let mut owner = Owner::new(
            Instant::now(),
            Duration::from_secs(1800),
            Duration::from_secs(60),
        )
        .unwrap();
        let runtime = match tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(_) => {
                owner.record(Cause::Io);
                let finished = owner.finish();
                drop(data);
                panic!("runtime unavailable after finalization: {finished:?}");
            }
        };
        super::super::super::files::start_probe(None);
        super::super::files::start_probe();
        let mut state = State::new(&data.access);
        let admitted: Result<()> = (|| {
            state
                .stage
                .setup(&stage_access(&data.access), &"a".repeat(40), &mut owner)?;
            state.config = Some(Dir::open(&data.access.config, &mut owner)?);
            state.input("app.env", 64, &data.access, &mut owner)?;
            Ok(())
        })();
        owner.record(Cause::Policy); // The original application readiness refusal.
        let mut daemon = Daemon {
            calls: 0,
            attempt: state.stage.attempt().map(|dir| dir.path().to_path_buf()),
            error,
            changed_log: Ok(()),
        };
        let result = runtime.block_on(async {
            admitted?;
            owner.begin_deploy_recovery()?;
            if pre_changed {
                control::io(
                    &mut owner,
                    fs::write(data.access.config.join("app.env"), b"changed original\n"),
                )?;
            }
            state.rollback.start(Stage::Stop);
            state
                .docker(
                    &data.access,
                    Role::Stop,
                    argv(&["stop", "exact-fixture"]),
                    true,
                    &mut owner,
                    &mut daemon,
                )
                .await
        });
        let diagnostic = serde_json::to_value(state.rollback.finish());
        owner.end_deploy_recovery();
        state.stage.unlock(&mut owner);
        state.records.finish(&mut owner);
        if let Some(directory) = state.config.take() {
            directory.finish(&mut owner);
        }
        state.stage.finish(&mut owner);
        let shared = super::super::super::files::take_probe();
        let records = super::super::files::take_probe();
        drop(runtime);
        let primary = owner.error();
        let finished = owner.finish();
        drop(data);
        assert_eq!(admitted, Ok(()));
        assert!(result.is_err());
        assert_eq!(primary, Some(Cause::Policy));
        assert_eq!(finished, Err(Cause::Policy));
        assert!(shared.0 > 0 && records.0 > 0);
        assert_eq!((shared.1, records.1), (0, 0));
        if pre_changed {
            assert_eq!(daemon.calls, 0);
            assert_eq!(
                diagnostic.unwrap(),
                json!({"stage":"stop","boundary":"originals-before",
                    "cause":"changed","logOrdinal":null})
            );
        } else {
            assert_eq!(daemon.calls, 1);
            assert!(daemon.changed_log.is_ok());
            assert_eq!(
                diagnostic.unwrap(),
                json!({"stage":"stop", "boundary":if error == Some(Cause::Io) {
                    "executor"} else {"exit-status"},
                    "cause":if error == Some(Cause::Io) {"io"} else {"command"},"logOrdinal":1})
            );
        }
    }
}
