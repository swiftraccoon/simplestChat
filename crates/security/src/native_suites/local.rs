//! The local engine, reached through the fixed prefix that selects it: each
//! command leads a process group of its own, is bounded in time and in what
//! it prints, and is stopped when the run is cancelled. Once it is, only the
//! removal of the run's sandbox is still asked.

use super::{MIB, engine::Engine};
use crate::{Fault, Result, context, require};
use std::{
    ffi::OsString, fs::OpenOptions, os::unix::fs::OpenOptionsExt, path::Path, process::Command,
    time::Duration,
};

/// How long a command past its bounds has to end.
const GRACE: Duration = Duration::from_secs(15);

/// The local engine of `prefix`, whose commands stop once `cancelled` says
/// the run was cancelled.
pub struct Local<'a> {
    /// The engine's program and the arguments that select it.
    pub prefix: Vec<OsString>,
    /// Whether the run was cancelled.
    pub cancelled: &'a dyn Fn() -> bool,
}

impl Local<'_> {
    /// The engine's command of `arguments`.
    fn command(&self, arguments: &[String]) -> Result<Command> {
        let (program, selection) = self.prefix.split_first().ok_or(Fault("engine_missing"))?;
        let mut command = Command::new(program);
        command.args(selection).args(arguments);
        Ok(command)
    }

    /// What the engine printed for `arguments` within `within`, once it
    /// ended with 0, whether or not the run was cancelled.
    fn asked(&self, arguments: &[String], within: Duration) -> Result<Vec<u8>> {
        let mut command = self.command(arguments)?;
        let asked = context::asked(&mut command, &[], within, (16 * MIB, MIB));
        let (status, printed) = asked.map_err(|failure| Fault(failure_code(&failure)))?;
        require(status == 0, "command_failed")?;
        Ok(printed)
    }
}

/// The private file `path`, opened for appending.
fn appended(path: &Path) -> Result<std::fs::File> {
    let opened = OpenOptions::new()
        .append(true)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(path);
    opened.map_err(|_| Fault("native_log_unwritable"))
}

impl Engine for Local<'_> {
    fn ask(&mut self, arguments: &[String], within: Duration) -> Result<Vec<u8>> {
        require(!(self.cancelled)(), "command_lease_lost")?;
        self.asked(arguments, within)
    }

    fn removing(&mut self, arguments: &[String], within: Duration) -> Result<Vec<u8>> {
        self.asked(arguments, within)
    }

    fn run(
        &mut self,
        arguments: &[String],
        input: &[u8],
        within: Duration,
        logs: (&Path, &Path),
        room: (u64, u64),
    ) -> Result<i32> {
        require(!(self.cancelled)(), "command_lease_lost")?;
        let mut command = self.command(arguments)?;
        let kept = (appended(logs.0)?, appended(logs.1)?);
        let cancelled = self.cancelled;
        let mut healthy = || !cancelled();
        let ended = context::ended(
            &mut command,
            input,
            (within, GRACE),
            kept,
            room,
            &mut healthy,
        );
        ended
            .outcome
            .map_err(|failure| Fault(failure_code(&failure)))
    }
}

/// The fixed code of why a command stopped.
pub(crate) fn failure_code(failure: &context::Failure) -> &'static str {
    match failure {
        context::Failure::Fixed(code) => code,
        context::Failure::Command(_) => "command_failed",
        context::Failure::Prerequisite(_) => "engine_missing",
    }
}
