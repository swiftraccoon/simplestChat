//! Fixed owned child roles exercise real late signals, never real GH/SSH/TLS.
use super::super::verify::process;
use super::*;
use std::{ffi::OsString, process::ExitCode};

const ROLE: &str = "SC_RELEASE_FINAL_SIGNAL_ROLE";
const SIGNAL: &str = "SC_RELEASE_FINAL_SIGNAL_KIND";
#[derive(Clone, Copy)]
pub(in crate::release::attestation) enum Entry {
    Prepare,
    Cached,
    Fresh,
    Fetch,
}
impl Entry {
    fn name(self) -> &'static str {
        match self {
            Self::Prepare => "prepare",
            Self::Cached => "cached",
            Self::Fresh => "fresh",
            Self::Fetch => "fetch",
        }
    }
    fn test(self) -> &'static str {
        match self {
            Self::Prepare => concat!(
                "release::attestation::prepare::tests::",
                "late_real_signals_after_runtime_restore_refuse_prepare_success"
            ),
            Self::Cached => concat!(
                "release::attestation::verify::tests::",
                "late_real_signals_after_runtime_restore_refuse_cached_success"
            ),
            Self::Fresh => concat!(
                "release::attestation::acquire::tests::",
                "late_real_signals_after_runtime_restore_refuse_fresh_success"
            ),
            Self::Fetch => concat!(
                "release::attestation::fetch::tests::",
                "late_real_signals_after_runtime_restore_refuse_fetch_success"
            ),
        }
    }
}
pub(in crate::release::attestation) fn worker(entry: Entry) -> Option<Cause> {
    let role = std::env::var_os(ROLE)?;
    assert_eq!(role, entry.name(), "fixed owned final-signal role");
    let (number, cause) = match std::env::var(SIGNAL).as_deref() {
        Ok("INT") => (signal_hook::consts::SIGINT, Cause::Interrupt),
        Ok("TERM") => (signal_hook::consts::SIGTERM, Cause::Terminate),
        _ => panic!("fixed owned final-signal selector"),
    };
    FINISH_SIGNAL.set(Some(number));
    FINISH_MASK.set(None);
    FINISHED.set(None);
    Some(cause)
}
pub(in crate::release::attestation) fn assert_finished(
    code: ExitCode,
    expected: Cause,
    mask: nix::sys::stat::Mode,
) {
    assert_eq!(code, ExitCode::FAILURE);
    assert_eq!(FINISHED.get(), Some(Err(expected)));
    assert_eq!(FINISH_MASK.get(), Some(mask));
    assert!(FINISH_SIGNAL.get().is_none());
}
pub(in crate::release::attestation) fn supervise(entry: Entry) {
    for signal in ["INT", "TERM"] {
        let program = std::env::current_exe().unwrap();
        let start = Instant::now();
        let mut owner = Owner::new(start, Duration::from_secs(50), Duration::from_secs(5)).unwrap();
        let runtime = match tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(error) => {
                let settled = owner.finish();
                panic!("owned final-signal runtime: {error}; registration cleanup {settled:?}");
            }
        };
        let completion = runtime.block_on(process::run_small(
            &mut owner,
            process::Invocation {
                program,
                arguments: vec![
                    "--exact".into(),
                    entry.test().into(),
                    "--nocapture".into(),
                    "--test-threads=1".into(),
                ],
                environment: [
                    (OsString::from(ROLE), OsString::from(entry.name())),
                    (OsString::from(SIGNAL), OsString::from(signal)),
                ]
                .into_iter()
                .collect(),
                body_end: start + Duration::from_secs(30),
                settlement_end: start + Duration::from_secs(40),
            },
        ));
        drop(runtime);
        let finished = owner.finish();
        let observed = &completion.observation;
        assert!(
            observed.group_retired && observed.reaped,
            "{signal}: child group"
        );
        assert!(
            observed.stdin_closed && observed.stdout_closed && observed.stderr_closed,
            "{signal}: all original pipes"
        );
        assert!(
            observed.stdout_eof && observed.stderr_eof,
            "{signal}: natural peer EOF"
        );
        assert_eq!(
            observed.exit_status,
            Some(0),
            "{signal}: inert child assertions"
        );
        assert!(
            finished.is_ok(),
            "{signal}: original parent registration cleanup"
        );
        let output = completion
            .result
            .unwrap_or_else(|cause| panic!("{signal}: bounded complete child {cause:?}"));
        const ONE: &[u8] = b"test result: ok. 1 passed; 0 failed;";
        assert!(
            output.windows(ONE.len()).any(|part| part == ONE),
            "{signal}: exactly one owned child test executed"
        );
    }
}
