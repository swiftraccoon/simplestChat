//! Fixed standard-library unittest discovery with two process-local shards.
//!
//! One N600 interval begins before inventory/setup, reserves30 seconds for
//! retirement/log completion/replay and remains authoritative through runtime
//! teardown and the final original signal fence. The workflow's independent
//! ten-minute job interval may expire earlier after preceding gates.
//! No source-only guarantee covers blocked kernel I/O or escaped sessions.
//!
//! API review: C-CUSTOM-TYPE/C-GOOD-ERR keep closed causes and real child status;
//! C-DTOR-FAIL/C-DTOR-BLOCK require explicit all-known finalization; C-RW-VALUE
//! borrows the output writer. No new public API, dependency or Python loader.

use super::{python_captures::Captures, python_inventory::Inventory};
use crate::{
    ci::bounded_log::{Finished, Log},
    tooling::{self, Environment, Failure},
};
use rustix::process::{WaitId, WaitIdOptions};
use simplestchat_ops::owned_process::{CompletionSignals, Group, finish_before};
use std::{
    ffi::OsString,
    io::Write,
    os::unix::process::CommandExt,
    path::Path,
    process::Stdio,
    time::{Duration, Instant},
};

pub(super) const BASE_ARGUMENTS: [&str; 6] = [
    "-m",
    "unittest",
    "discover",
    "-s",
    "ops/ansible/tests",
    "-v",
];
const WHOLE: Duration = Duration::from_secs(600);
const FINAL: Duration = Duration::from_secs(30);
const GRACE: Duration = Duration::from_secs(5);
const POLL: Duration = Duration::from_millis(10);
pub(super) type Outcome<T> = std::result::Result<T, Cause>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Cause {
    Inventory,
    Io,
    Spawn(u8),
    Close,
    Cleanup,
    Deadline,
    Signal(u8),
    Child { ordinal: usize, status: u8 },
}
impl Cause {
    fn failure(self) -> Failure {
        let (code, message) = match self {
            Self::Inventory => (
                1,
                "Python test inventory changed or has an unsupported layout.",
            ),
            Self::Io => (1, "Python regression capture or output failed."),
            Self::Spawn(code) => (code, "Python regression child could not start."),
            Self::Close => (1, "Python regression original close failed."),
            Self::Cleanup => (1, "Python regression child cleanup is unproven."),
            Self::Deadline => (124, "Python regression operation expired."),
            Self::Signal(code) => (code, "Python regression operation was interrupted."),
            Self::Child { status, .. } => (status, "Python regression discovery failed."),
        };
        Failure::new(code, message)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Profile {
    Parallel,
    Database,
}
impl Profile {
    fn from_env(environment: &Environment) -> Self {
        if environment
            .get(std::ffi::OsStr::new("DISPOSABLE_TEST_DATABASE"))
            .is_some_and(|value| value == "1")
        {
            Self::Database
        } else {
            Self::Parallel
        }
    }
    fn count(self) -> usize {
        if self == Self::Parallel { 2 } else { 1 }
    }
    fn arguments(self, inventory: &Inventory) -> Vec<Vec<OsString>> {
        (0..self.count())
            .map(|ordinal| {
                let mut args = BASE_ARGUMENTS
                    .iter()
                    .map(OsString::from)
                    .collect::<Vec<_>>();
                if self == Self::Parallel {
                    for module in &inventory.groups[ordinal] {
                        args.extend([OsString::from("-k"), OsString::from(format!("{module}.*"))]);
                    }
                }
                args
            })
            .collect()
    }
}

pub(super) struct Control {
    signals: CompletionSignals,
    body: Instant,
    end: Instant,
    first: Option<Cause>,
    stop: bool,
    #[cfg(test)]
    signal: Option<std::sync::Arc<std::sync::atomic::AtomicU8>>,
    #[cfg(test)]
    spawn_refusal: Option<usize>,
    #[cfg(test)]
    worker: bool,
    #[cfg(test)]
    admitted: Option<std::sync::Arc<[std::sync::atomic::AtomicI32; 2]>>,
}
impl Control {
    fn new(started: Instant) -> Outcome<Self> {
        Ok(Self {
            signals: CompletionSignals::new().map_err(|_| Cause::Io)?,
            body: started + WHOLE - FINAL,
            end: started + WHOLE,
            first: None,
            stop: false,
            #[cfg(test)]
            signal: None,
            #[cfg(test)]
            spawn_refusal: None,
            #[cfg(test)]
            worker: false,
            #[cfg(test)]
            admitted: None,
        })
    }
    fn note(&mut self, cause: Cause) {
        if !matches!(cause, Cause::Child { .. }) {
            self.stop = true;
        }
        if self.first.is_none() {
            self.first = Some(cause);
        }
    }
    fn remember<T>(&mut self, result: Outcome<T>) -> Option<T> {
        match result {
            Ok(value) => Some(value),
            Err(cause) => {
                self.note(cause);
                None
            }
        }
    }
    fn observed_signal(&self) -> Option<u8> {
        #[cfg(test)]
        if let Some(value) = &self.signal {
            let value = value.load(std::sync::atomic::Ordering::SeqCst);
            if value != 0 {
                return Some(value);
            }
        }
        self.signals.status()
    }
    fn fence(&mut self, end: Instant) -> Outcome<()> {
        if let Some(code) = self.observed_signal() {
            let cause = Cause::Signal(code);
            self.note(cause);
            return Err(cause);
        }
        if Instant::now() >= end {
            self.note(Cause::Deadline);
            return Err(Cause::Deadline);
        }
        Ok(())
    }
    fn body_fence(&mut self) -> Outcome<()> {
        self.fence(self.body)
    }
    pub(super) fn final_fence(&mut self) -> Outcome<()> {
        self.fence(self.end)
    }
    fn finish(&mut self) -> Outcome<()> {
        let _ = self.final_fence();
        match self.signals.finish() {
            Ok(Some(code)) => self.note(Cause::Signal(code)),
            Ok(None) => {}
            Err(_) => self.note(Cause::Close),
        }
        let _ = self.final_fence();
        self.first.map_or(Ok(()), Err)
    }
}

fn terminal(group: &Group) -> Outcome<Option<u8>> {
    match rustix::process::waitid(
        WaitId::Pid(group.pid()),
        WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
    ) {
        Ok(Some(status)) => status
            .exit_status()
            .or_else(|| {
                status
                    .terminating_signal()
                    .and_then(|signal| signal.checked_add(128))
            })
            .and_then(|status| u8::try_from(status).ok())
            .map(Some)
            .ok_or(Cause::Cleanup),
        Ok(None) | Err(rustix::io::Errno::INTR) => Ok(None),
        Err(_) => Err(Cause::Cleanup),
    }
}

fn observe(groups: &[Option<Group>; 2], statuses: &mut [Option<u8>; 2], control: &mut Control) {
    for (ordinal, group) in groups.iter().enumerate() {
        if statuses[ordinal].is_some() {
            continue;
        }
        if let Some(group) = group
            && let Some(Some(status)) = control.remember(terminal(group))
        {
            statuses[ordinal] = Some(status);
            if status != 0 {
                control.note(Cause::Child { ordinal, status });
            }
        }
    }
}

fn spawn(
    root: &Path,
    environment: &Environment,
    arguments: &[OsString],
    log: &Log,
    control: &Control,
) -> Outcome<Group> {
    if let Some(code) = control.observed_signal() {
        return Err(Cause::Signal(code));
    }
    if Instant::now() >= control.body {
        return Err(Cause::Deadline);
    }
    let mut command = tooling::command(root, environment, "python", arguments);
    #[cfg(test)]
    if control.worker {
        // The inert worker is this same Rust test executable with one fixed
        // libtest entry. The recorded planned Command remains passive evidence;
        // it is never executable or process-group authority.
        let planned = serde_json::json!({
            "program": command.get_program().to_str().ok_or(Cause::Io)?,
            "arguments": command.get_args().map(|value| value.to_str().ok_or(Cause::Io))
                .collect::<Outcome<Vec<_>>>()?,
        })
        .to_string();
        command = tooling::command(
            root,
            environment,
            std::env::current_exe().map_err(|_| Cause::Io)?,
            [
                "--exact",
                "ci::automation::python_shards::tests::rust_inert_worker",
                "--nocapture",
                "--test-threads=1",
            ],
        );
        command.env("SIMPLESTCHAT_TEST_SHARD_PLAN", planned);
    }
    command
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.writer().map_err(|_| Cause::Io)?))
        .stderr(Stdio::from(log.writer().map_err(|_| Cause::Io)?));
    // Register the admitted child before any fallible observation. Command owns
    // stdio clones too, so drop it before releasing the Log's original writer.
    let result = command.spawn().map(Group::new).map_err(|error| {
        Cause::Spawn(match error.kind() {
            std::io::ErrorKind::NotFound => 127,
            std::io::ErrorKind::PermissionDenied => 126,
            _ => 1,
        })
    });
    drop(command);
    result
}

async fn retire(group: Option<&Group>, end: Instant) -> bool {
    match group {
        Some(group) => finish_before(group.pid(), end).await,
        None => true,
    }
}

async fn reap(group: Option<Group>) -> Outcome<Option<u8>> {
    match group {
        Some(mut group) => group
            .reap()
            .await
            .map(tooling::shell_status)
            .map(Some)
            .map_err(|_| Cause::Cleanup),
        None => Ok(None),
    }
}
async fn finish_log(log: Option<Log>) -> Outcome<Option<Finished>> {
    match log {
        Some(log) => {
            let finished = log.finish().await.map_err(|_| Cause::Io)?;
            if !finished.complete {
                return Err(Cause::Io);
            }
            Ok(Some(finished))
        }
        None => Ok(None),
    }
}

async fn perform(
    root: &Path,
    environment: &Environment,
    inventory: &Inventory,
    captures: &mut Captures,
    profile: Profile,
    control: &mut Control,
) -> [Option<Finished>; 2] {
    let mut groups: [Option<Group>; 2] = [None, None];
    let mut logs: [Option<Log>; 2] = [None, None];
    let mut statuses = [None, None];
    let arguments = profile.arguments(inventory);
    for ordinal in 0..profile.count() {
        if control.body_fence().is_err() || control.stop {
            break;
        }
        if control.remember(inventory.current()).is_none() {
            break;
        }
        logs[ordinal] = control.remember(captures.log(ordinal));
        let Some(log) = logs[ordinal].as_mut() else {
            break;
        };
        if control.body_fence().is_err() {
            break;
        }
        #[cfg(test)]
        let launched = if control.spawn_refusal == Some(ordinal) {
            Err(Cause::Spawn(127))
        } else {
            spawn(root, environment, &arguments[ordinal], log, control)
        };
        #[cfg(not(test))]
        let launched = spawn(root, environment, &arguments[ordinal], log, control);
        groups[ordinal] = control.remember(launched);
        #[cfg(test)]
        if let (Some(group), Some(pids)) = (&groups[ordinal], &control.admitted) {
            pids[ordinal].store(
                group.pid().as_raw_nonzero().get(),
                std::sync::atomic::Ordering::SeqCst,
            );
        }
        control.remember(log.release_writer().map_err(|_| Cause::Close));
        let _ = control.body_fence();
        if groups[ordinal].is_none() || control.stop {
            break;
        }
    }
    while !control.stop {
        if control.body_fence().is_err() {
            break;
        }
        observe(&groups, &mut statuses, control);
        let _ = control.body_fence();
        for log in logs.iter().flatten() {
            if log.failed() {
                control.note(Cause::Io);
            }
        }
        if (0..profile.count()).all(|ordinal| statuses[ordinal].is_some()) {
            break;
        }
        tokio::time::sleep(POLL).await;
    }
    // A nonzero shard still lets its admitted peer complete naturally, matching
    // unittest's non-failfast policy. Signals/I/O/setup/deadline refuse the body.
    if control.stop {
        for group in groups.iter().flatten() {
            if simplestchat_ops::owned_process::reserved(group.pid())
                && let Err(error) =
                    rustix::process::kill_process_group(group.pid(), rustix::process::Signal::TERM)
                && error != rustix::io::Errno::SRCH
            {
                control.note(Cause::Cleanup);
            }
        }
        let grace = (Instant::now() + GRACE).min(control.end);
        while Instant::now() < grace {
            observe(&groups, &mut statuses, control);
            if groups
                .iter()
                .enumerate()
                .all(|(i, g)| g.is_none() || statuses[i].is_some())
            {
                break;
            }
            tokio::time::sleep(POLL).await;
        }
    }
    let _ = control.final_fence();
    let end = control.end;
    let retired = tokio::join!(
        retire(groups[0].as_ref(), end),
        retire(groups[1].as_ref(), end)
    );
    if !retired.0 || !retired.1 {
        control.note(Cause::Cleanup);
    }
    let _ = control.final_fence();
    // Both exact groups have received final signals before either consuming
    // wait. Await both reaps without timeout/drop of the disarmed wait owner.
    let reaped = tokio::join!(reap(groups[0].take()), reap(groups[1].take()));
    for (ordinal, result) in [reaped.0, reaped.1].into_iter().enumerate() {
        if let Some(status) = control.remember(result).flatten() {
            if statuses[ordinal].is_some_and(|prior| prior != status) {
                control.note(Cause::Cleanup);
            }
            if status != 0 {
                control.note(Cause::Child { ordinal, status });
            }
        }
    }
    let _ = control.final_fence();
    // Shared Log retains its existing2s EOF +1s stop tail. Both are joined
    // inside the original reserve and post-fenced; no new caller budget starts.
    let _ = control.fence(control.end - Duration::from_secs(6));
    let finished = tokio::join!(finish_log(logs[0].take()), finish_log(logs[1].take()));
    let finished = [
        control.remember(finished.0).flatten(),
        control.remember(finished.1).flatten(),
    ];
    let _ = control.final_fence();
    for item in finished.iter().take(profile.count()) {
        if item.is_none() {
            control.note(Cause::Io);
        }
    }
    finished
}

fn run_with(
    root: &Path,
    environment: &Environment,
    mut control: Control,
    output: &mut dyn Write,
) -> Outcome<()> {
    let mut inventory = None;
    let mut captures = None;
    let mut finished = [None, None];
    let profile = Profile::from_env(environment);
    if control.body_fence().is_ok() {
        inventory = control.remember(Inventory::read(root));
    }
    if inventory.is_some() && control.body_fence().is_ok() {
        captures = control.remember(Captures::create(environment));
    }
    if let (Some(inventory), Some(captures)) = (inventory.as_ref(), captures.as_mut()) {
        match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => {
                finished = runtime.block_on(perform(
                    root,
                    environment,
                    inventory,
                    captures,
                    profile,
                    &mut control,
                ));
                drop(runtime);
            }
            Err(_) => control.note(Cause::Io),
        }
    }
    let _ = control.final_fence();
    if let Some(inventory) = &inventory {
        control.remember(inventory.current());
    }
    if let Some(captures) = &captures {
        for (ordinal, item) in finished.iter().enumerate().take(profile.count()) {
            if let Some(item) = item {
                let replayed = captures.replay(ordinal, item, output, &mut control);
                control.remember(replayed);
            }
        }
    }
    control.remember(output.flush().map_err(|_| Cause::Io));
    let _ = control.final_fence();
    if let Some(captures) = &mut captures {
        control.remember(captures.close());
    }
    if let Some(inventory) = &mut inventory {
        control.remember(inventory.current());
        control.remember(inventory.close());
    }
    control.finish()
}

pub(super) fn run(root: &Path, environment: &Environment) -> tooling::Result<()> {
    let started = Instant::now();
    let control = Control::new(started).map_err(Cause::failure)?;
    run_with(root, environment, control, &mut std::io::stderr().lock()).map_err(Cause::failure)
}

#[cfg(test)]
#[path = "python_shards_tests.rs"]
mod tests;
