//! The data steps of a move, each run on one machine as
//! `simplestchat-ops migrate STEP --request PATH`. A step holds the
//! machine's workload lock, admits only this move's own unfinished record,
//! keeps the output of every command it runs inside the move's private
//! directory and says one line: the phase it reached, or that it failed.
//! Why a step failed stays on the machine.

use super::deployment::{Database, Move, journal};
use super::request::{Request, request_file};
use super::source::{MAX_DUMP, freeze, recover, retire, seal};
use super::target::{abort, restore, verify};
use super::value::says;
use crate::host::command::Interruptible;
use crate::host::evidence::Evidence;
use crate::host::failure::{Failure, Result, require};
use crate::host::fault::{object, string};
use crate::host::{Host, Paths, System, emit, state};
use crate::json::{Json, python_dump};
use crate::release::assets::{self, Takeover, Workload};
use std::ffi::OsString;
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// The one line a failed step leaves the caller.
const FAILED: &str = "{\"passed\":false,\"error\":\"migration_action_failed\"}\n";
/// The account of the database inside its image, which owns its files.
const DATABASE: Database = Database {
    owner: 999,
    group: 999,
    archive: MAX_DUMP,
};

/// One step of a move.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Action {
    Freeze,
    Recover,
    Seal,
    Finalize,
    Restore,
    Verify,
    Abort,
}

impl Action {
    pub(super) const ALL: [Self; 7] = [
        Self::Freeze,
        Self::Recover,
        Self::Seal,
        Self::Finalize,
        Self::Restore,
        Self::Verify,
        Self::Abort,
    ];

    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Freeze => "freeze-source",
            Self::Recover => "recover-source",
            Self::Seal => "seal-source",
            Self::Finalize => "finalize-source",
            Self::Restore => "restore-target",
            Self::Verify => "verify-target",
            Self::Abort => "abort-target",
        }
    }

    pub(super) fn named(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|action| action.name() == name)
    }
}

/// A step that continues a move its machine has already recorded.
type Continued = for<'a> fn(&mut Move<'a>, &Json) -> Result<Json>;

/// Run one step and return what it recorded. Only a freeze and a restore
/// begin a journal; every other step needs one, and only a verification and
/// an abort belong to the destination.
pub(super) fn perform(session: &mut Move<'_>, action: Action) -> Result<Json> {
    let step: Continued = match action {
        Action::Freeze => return freeze(session),
        Action::Restore => return restore(session),
        Action::Recover => recover,
        Action::Seal => seal,
        Action::Finalize => retire,
        Action::Verify => verify,
        Action::Abort => abort,
    };
    let prior = journal(session)?.ok_or(Failure::Refused("migration_journal_missing"))?;
    let anywhere = matches!(action, Action::Verify | Action::Abort);
    require(
        anywhere || says(&prior, "role", "source"),
        "source_action_on_destination",
    )?;
    step(session, &prior)
}

/// One step under the held workload lock: its commands leave their output
/// in a new private directory of the move, and its result is the one line
/// the caller reads.
fn transact(
    host: &Host<'_>,
    action: Action,
    request: &Request,
    rearm: &dyn Fn(),
    database: Database,
) -> Result<String> {
    let directory = request.directory(host);
    let logs = state::private_directory(&directory, &format!("{}.", action.name()))?;
    let mut session = Move {
        host,
        request,
        directory,
        runner: Evidence::new(host, logs),
        rearm,
        database,
    };
    let result = perform(&mut session, action)?;
    let report = object(vec![
        ("passed", Json::Bool(true)),
        ("action", string(action.name())),
        ("operationId", string(&request.operation)),
        ("phase", result.get("phase").cloned().unwrap_or(Json::Null)),
        ("result", result),
    ]);
    Ok(format!("{}\n", python_dump(&report, false)))
}

/// Admit the caller and the request, take the workload lock through `lock`
/// for this move only, and release it whatever happens under it.
pub(super) fn execute(
    host: &Host<'_>,
    action: Action,
    path: &Path,
    privileged: bool,
    rearm: &dyn Fn(),
    database: Database,
    lock: impl FnOnce(Takeover<'_>) -> std::result::Result<Workload, assets::Failure>,
) -> Result<String> {
    require(privileged, "root_required")?;
    let request = request_file(host, path)?;
    let takeover = Takeover::Migration {
        operation: &request.operation,
        source: &request.source,
        destination: &request.destination,
        revision: &request.revision,
    };
    let workload = lock(takeover).map_err(|failure| {
        let busy = failure.busy();
        Failure::Refused(if busy {
            "migration_host_busy"
        } else {
            "unfinished_host_work"
        })
    })?;
    let outcome = transact(host, action, &request, rearm, database);
    let released = workload.finish().map_err(|_| Failure::Os);
    let line = outcome?;
    released.map(|()| line)
}

/// The step and the request file named, or nothing for any other command
/// line.
fn parse(arguments: &[OsString]) -> Option<(Action, &Path)> {
    let [action, option, path] = arguments else {
        return None;
    };
    let action = Action::named(action.to_str()?)?;
    (option == "--request").then(|| (action, Path::new(path)))
}

/// Run one data step of a move on this machine. A command line of another
/// shape returns 2 before any effect.
pub(super) fn run(arguments: &[OsString]) -> ExitCode {
    let Some((action, path)) = parse(arguments) else {
        return ExitCode::from(2);
    };
    rustix::process::umask(rustix::fs::Mode::from_raw_mode(0o077));
    let stop = Arc::new(AtomicBool::new(false));
    let handled = [signal_hook::consts::SIGTERM, signal_hook::consts::SIGINT]
        .iter()
        .all(|signal| signal_hook::flag::register(*signal, Arc::clone(&stop)).is_ok());
    let commands = Interruptible {
        stop: &stop,
        refusal: "migration_interrupted",
    };
    let clock = System::new();
    let host = Host {
        commands: &commands,
        clock: &clock,
        paths: Paths::fixed(),
        owner: 0,
    };
    let privileged = rustix::process::geteuid().is_root();
    let rearm = || stop.store(false, Ordering::SeqCst);
    let outcome = if handled {
        let lock = Workload::acquire_for;
        execute(&host, action, path, privileged, &rearm, DATABASE, lock)
    } else {
        Err(Failure::Os)
    };
    match outcome {
        Ok(line) if emit(std::io::stdout(), &line) => ExitCode::SUCCESS,
        _ => {
            let _ = emit(std::io::stderr(), FAILED);
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
#[path = "data_tests.rs"]
mod tests;
