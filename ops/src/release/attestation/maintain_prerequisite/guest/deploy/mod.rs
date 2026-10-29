//! Complete fixed app-only deploy fixture, with its own closed wire and verdict.

mod files;
mod history;
pub(super) mod injection;
mod injection_client;
mod native_report;
pub(super) mod operation;
pub(super) mod payload;
pub(super) mod probe;
mod protocol;
#[cfg(test)]
mod tests;
pub(super) mod validation;

use super::{
    CONFIG, Context, Error, PRIVATE, PUBLIC, ROOT, Result, control::Control, effects,
    files::Original, observation::Observations, process::Commands, sources,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    path::{Path, PathBuf},
    process::ExitCode,
    time::{Duration, Instant},
};

const CHECKPOINTS: [&str; 16] = [
    "input",
    "sources",
    "payload",
    "configuration",
    "network",
    "database",
    "tls",
    "baseline",
    "prepared",
    "positive",
    "negative",
    "injection",
    "observation",
    "probe",
    "cleanup",
    "complete",
];
fn require(value: bool) -> Result<()> {
    if value { Ok(()) } else { Err(Error::Refused) }
}
fn hex(value: &str, count: usize) -> bool {
    super::protocol::digest(value, count)
}
pub(super) fn hash(value: &[u8]) -> String {
    super::files::hex(&Sha256::digest(value))
}
fn decode(value: &[u8]) -> Result<Value> {
    crate::json::decode_limited(value, 2 * 1024 * 1024).map_err(|_| Error::Refused)?;
    serde_json::from_slice(value).map_err(|_| Error::Refused)
}

struct State {
    request: protocol::Request,
    fixture: effects::Fixture,
    payloads: Option<payload::Payloads>,
    baseline: Option<effects::deploy::Baseline>,
    first_image: Option<String>,
    current_image: Option<String>,
    trials: Vec<Value>,
    phases: BTreeMap<&'static str, Option<f64>>,
    separate: Option<Value>,
    complete: bool,
    checkpoint: &'static str,
    trial: Option<&'static str>,
    stdout: native_report::Capture,
    outcome: native_report::Capture,
}
impl State {
    fn new(request: protocol::Request) -> Self {
        Self {
            fixture: effects::Fixture::new(&request.adapter()),
            request,
            payloads: None,
            baseline: None,
            first_image: None,
            current_image: None,
            trials: Vec::new(),
            phases: ["prepare", "positive", "negative"]
                .into_iter()
                .map(|name| (name, None))
                .collect(),
            separate: None,
            complete: false,
            checkpoint: "input",
            trial: None,
            stdout: native_report::Capture::not_observed(),
            outcome: native_report::Capture::not_observed(),
        }
    }
    fn mark(&mut self, checkpoint: &'static str) {
        debug_assert!(CHECKPOINTS.contains(&checkpoint));
        self.checkpoint = checkpoint;
    }
    fn capture(&self, ctx: &Context) -> Value {
        json!({"checkpoint":self.checkpoint,
            "failure":ctx.control.first.map(Error::category).unwrap_or("refused"),
            "trial":self.trial,"nativeRawExit":self.stdout.native_raw_exit,
            "stdoutState":self.stdout.native_report_state,
            "outcomeState":self.outcome.native_report_state,
            "nativePassed":self.stdout.native_passed,
            "rollbackAttempted":self.stdout.native_rollback_attempted,
            "rollbackPassed":self.stdout.native_rollback_passed,
            "rollbackFailure":self.stdout.rollback_failure,
            "outcomeRollbackFailure":self.outcome.rollback_failure,
            "nativePhase":self.stdout.native_phase,
            "nativeFailureLabel":self.stdout.native_failure_label,
            "nativeCommandOrdinal":self.stdout.native_command_ordinal})
    }
    fn result(&self, ctx: &Context, cleanup: bool) -> Value {
        let passed = self.complete && cleanup && ctx.control.first.is_none();
        json!({"schemaVersion":1,"kind":"result","runId":self.request.run_id,
            "revision":self.request.revision,"previousRevision":self.request.previous_revision,
            "passed":passed,"cleanupPassed":cleanup,"binarySha256":self.request.binary.sha256,
            "firstImage":self.first_image,"currentImage":self.current_image,"trials":self.trials,
            "dependenciesUnchanged":self.complete,"markerUnchanged":self.complete,
            "seedAggregateUnchanged":self.complete,"schemaUnchanged":self.complete,
            "currentSelected":self.complete,"actorHistoryComplete":self.complete,
            "ownedInjectorStoppedCandidate":self.complete,
            "nativeValidatorIsolationIndependentlyInspected":false,
            "phaseSeconds":self.phases,"separateProbe":self.separate,
            "failureCapture":if passed {Value::Null} else {self.capture(ctx)}})
    }
}

pub(super) fn run(arguments: &[OsString]) -> ExitCode {
    let start = Instant::now();
    if !arguments.is_empty() {
        return ExitCode::from(2);
    }
    if !cfg!(target_os = "linux") {
        return ExitCode::FAILURE;
    }
    let previous = nix::sys::stat::umask(nix::sys::stat::Mode::from_bits_truncate(0o077));
    let mut control = match Control::new_deploy(start) {
        Ok(owner) => owner,
        Err(_) => {
            nix::sys::stat::umask(previous);
            return ExitCode::FAILURE;
        }
    };
    let mut wire = match super::protocol::Wire::new(&mut control) {
        Ok(wire) => wire,
        Err(error) => {
            control.record(error);
            control.begin_settling();
            nix::sys::stat::umask(previous);
            let _ = control.finish();
            return ExitCode::from(super::failed_status(control.status));
        }
    };
    let admitted = (|| {
        let request = protocol::Request::decode(&wire.line_bounded(&mut control, 16384)?)?;
        control.configure_deploy(request.setup_seconds, request.body_seconds)?;
        wire.no_extra(&mut control)?;
        Ok(request)
    })();
    let request = match admitted {
        Ok(request) => request,
        Err(error) => {
            control.record(error);
            control.begin_settling();
            let _ = wire.close(&mut control);
            nix::sys::stat::umask(previous);
            let _ = control.finish();
            return ExitCode::from(super::failed_status(control.status));
        }
    };
    let mut ctx = Context {
        source_root: Path::new("/srv/simplestchat-bench/sources").join(&request.revision),
        request: request.adapter(),
        control,
        commands: Commands::new_deploy(),
        sources: BTreeMap::new(),
        originals: Vec::new(),
        observed: Observations::new(),
        wire,
        socket: None,
        host_retired: BTreeSet::new(),
        host_diagnostic: None,
    };
    let mut state = State::new(request);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build();
    let report = match runtime {
        Ok(runtime) => {
            let report = runtime.block_on(state.transaction(&mut ctx));
            drop(runtime);
            report
        }
        Err(_) => {
            ctx.control.record(Error::Io);
            ctx.control.begin_settling();
            state.result(&ctx, false)
        }
    };
    if let Err(error) = ctx.wire.emit_bounded(&report, &mut ctx.control, 16384) {
        ctx.control.record(error);
    }
    let closed = ctx.wire.close(&mut ctx.control);
    super::first(&mut ctx.control, closed);
    nix::sys::stat::umask(previous);
    if ctx.control.finish().is_ok() && report["passed"] == true {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(super::failed_status(ctx.control.status))
    }
}
