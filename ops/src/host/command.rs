//! Fixed local commands with a deadline, a fixed environment and bounded
//! output. The child leads its own process group, which is killed and reaped
//! when the deadline passes; nothing it printed is kept after a failure.

use super::failure::{Failure, Result, require};
use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// One invocation: explicit argv, the whole environment, optional input, a
/// deadline, and the most standard output the caller accepts.
pub(crate) struct Request<'a> {
    pub argv: Vec<String>,
    pub environment: &'a [(&'a str, &'a str)],
    pub input: Option<&'a [u8]>,
    /// A file given as standard input instead of `input`.
    pub source: Option<&'a std::path::Path>,
    /// A new private file that receives standard output instead of the
    /// caller; `limit` does not apply to it.
    pub archive: Option<&'a std::path::Path>,
    /// The most bytes `archive` may receive. The runner then copies standard
    /// output itself and stops a command that says more; without a ceiling
    /// the command writes the file directly.
    pub ceiling: Option<u64>,
    /// A new private file that keeps at most 2 MiB of standard error as
    /// evidence; otherwise it is discarded.
    pub errors: Option<&'a std::path::Path>,
    pub seconds: f64,
    pub limit: usize,
    /// The refusal named when the command says more than `limit`.
    pub oversized: &'static str,
}

/// The machine a command works on: it runs fixed commands and says how much
/// room a filesystem has left.
pub(crate) trait Commands {
    /// Standard output of a command that exited with status zero in time.
    fn run(&self, request: &Request<'_>) -> Result<Vec<u8>>;

    /// Bytes an unprivileged writer may still use on the filesystem holding
    /// `path`.
    fn free(&self, path: &std::path::Path) -> Result<u64> {
        let usage = rustix::fs::statvfs(path).map_err(|_| Failure::Os)?;
        Ok(usage.f_bavail.saturating_mul(usage.f_frsize))
    }
}

/// The local machine.
pub(crate) struct Local;

/// Read at most `limit` bytes and drain the rest, reporting the total seen.
fn bounded(mut source: impl Read, limit: usize) -> (Vec<u8>, usize) {
    let mut kept = Vec::new();
    let mut total = 0;
    let mut chunk = [0_u8; 8192];
    while let Ok(count) = source.read(&mut chunk) {
        if count == 0 {
            break;
        }
        total += count;
        let room = limit.saturating_sub(kept.len());
        kept.extend_from_slice(&chunk[..count.min(room)]);
    }
    (kept, total)
}

/// The most standard error a command may leave as evidence.
const MAX_ERROR_BYTES: u64 = 2 * 1024 * 1024;

/// Copy `source` into `file` up to `ceiling` bytes, then only drain it and
/// raise `over`, so the writer never blocks before its group is stopped.
fn pumped(
    mut source: impl Read,
    mut file: &std::fs::File,
    ceiling: u64,
    over: &AtomicBool,
) -> std::io::Result<()> {
    let mut written = 0_u64;
    let mut chunk = [0_u8; 65536];
    loop {
        let count = source.read(&mut chunk)?;
        if count == 0 {
            return Ok(());
        }
        let room = usize::try_from(ceiling - written).map_or(count, |room| room.min(count));
        file.write_all(&chunk[..room])?;
        written += room as u64;
        if room < count {
            over.store(true, Ordering::SeqCst);
        }
    }
}

/// A new private file; a name that exists is never reused.
fn created(path: &std::path::Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true).mode(0o600).open(path)
}

/// The environment of every fixed command.
pub(crate) const ENVIRONMENT: [(&str, &str); 2] = [("PATH", "/usr/bin:/bin"), ("HOME", "/root")];

impl Request<'_> {
    /// A command with the fixed environment, no input and captured output.
    pub(crate) fn fixed(
        argv: Vec<String>,
        seconds: f64,
        limit: usize,
        oversized: &'static str,
    ) -> Self {
        Self {
            argv,
            environment: &ENVIRONMENT,
            input: None,
            source: None,
            archive: None,
            ceiling: None,
            errors: None,
            seconds,
            limit,
            oversized,
        }
    }
}

impl Commands for Local {
    fn run(&self, request: &Request<'_>) -> Result<Vec<u8>> {
        execute(request, None)
    }
}

/// The local machine for a command that a termination signal must stop: once
/// `stop` is raised no further command starts, and a running one is killed
/// with its group. Either is the caller's named refusal, so the caller still
/// records how far it got.
pub(crate) struct Interruptible<'a> {
    pub stop: &'a AtomicBool,
    pub refusal: &'static str,
}

impl Commands for Interruptible<'_> {
    fn run(&self, request: &Request<'_>) -> Result<Vec<u8>> {
        require(!self.stop.load(Ordering::SeqCst), self.refusal)?;
        let outcome = execute(request, Some(self.stop));
        require(!self.stop.load(Ordering::SeqCst), self.refusal)?;
        outcome
    }
}

/// Run one request on this machine, watching `stop` while it runs.
fn execute(request: &Request<'_>, stop: Option<&AtomicBool>) -> Result<Vec<u8>> {
    let (program, arguments) = request.argv.split_first().ok_or(Failure::Command)?;
    let input = match (request.source, request.input) {
        (Some(path), _) => Stdio::from(std::fs::File::open(path)?),
        (None, Some(_)) => Stdio::piped(),
        (None, None) => Stdio::null(),
    };
    let archive = request.archive.map(created).transpose()?;
    let evidence = request.errors.map(created).transpose()?;
    let output = match (&archive, request.ceiling) {
        (Some(file), None) => Stdio::from(file.try_clone()?),
        _ => Stdio::piped(),
    };
    let mut command = Command::new(program);
    command
        .args(arguments)
        .env_clear()
        .envs(request.environment.iter().copied())
        .stdin(input)
        .stdout(output)
        .stderr(Stdio::piped())
        .process_group(0);
    let mut child = command.spawn().map_err(|_| Failure::Command)?;
    let group = i32::try_from(child.id()).map(Pid::from_raw);
    let input = child.stdin.take();
    let output = child.stdout.take();
    let error = child.stderr.take();
    let limit = request.limit;
    let (much, noisy) = (AtomicBool::new(false), AtomicBool::new(false));
    let finished = std::thread::scope(|scope| {
        let stdout = scope.spawn(|| match (output, &archive, request.ceiling) {
            (Some(output), Some(file), Some(ceiling)) => {
                pumped(output, file, ceiling, &much).map(|()| (Vec::new(), 0))
            }
            (Some(output), _, _) => Ok(bounded(output, limit)),
            (None, _, _) => Ok((Vec::new(), 0)),
        });
        let stderr = scope.spawn(|| match (error, &evidence) {
            (Some(error), Some(file)) => pumped(error, file, MAX_ERROR_BYTES, &noisy),
            (Some(error), None) => {
                bounded(error, 0);
                Ok(())
            }
            (None, _) => Ok(()),
        });
        if let (Some(mut pipe), Some(bytes)) = (input, request.input) {
            scope.spawn(move || {
                let _ = pipe.write_all(bytes);
            });
        }
        let deadline = Instant::now() + Duration::from_secs_f64(request.seconds);
        let said_too_much = || much.load(Ordering::SeqCst) || noisy.load(Ordering::SeqCst);
        let stopped = || stop.is_some_and(|stop| stop.load(Ordering::SeqCst));
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) if Instant::now() < deadline && !said_too_much() && !stopped() => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                _ => break None,
            }
        };
        // Whatever the outcome, no member of the group keeps a pipe open.
        if let Ok(group) = group {
            let _ = killpg(group, Signal::SIGKILL);
        }
        let _ = child.wait();
        (status, stdout.join(), stderr.join())
    });
    let (status, stdout, stderr) = finished;
    let (kept, total) = stdout.map_err(|_| Failure::Command)??;
    stderr.map_err(|_| Failure::Command)??;
    require(
        !much.load(Ordering::SeqCst),
        "command_stdout_limit_exceeded",
    )?;
    require(
        !noisy.load(Ordering::SeqCst),
        "command_stderr_limit_exceeded",
    )?;
    match status {
        None => return Err(Failure::Timeout),
        Some(status) if !status.success() => return Err(Failure::Command),
        Some(_) => {}
    }
    require(total <= request.limit, request.oversized)?;
    for file in archive.iter().chain(&evidence) {
        file.sync_all()?;
    }
    Ok(kept)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::cell::RefCell;

    /// One request as a scripted host saw it.
    #[derive(Clone, Debug)]
    pub(crate) struct Asked {
        pub argv: Vec<String>,
        pub environment: Vec<(String, String)>,
        pub input: Option<Vec<u8>>,
        pub source: Option<std::path::PathBuf>,
        pub archive: Option<std::path::PathBuf>,
        pub ceiling: Option<u64>,
        pub seconds: f64,
        pub limit: usize,
    }

    /// Commands answered from a script, recording what was asked.
    pub(crate) struct Scripted<F: Fn(&Request<'_>) -> Result<Vec<u8>>> {
        pub answer: F,
        pub asked: RefCell<Vec<Asked>>,
        /// The free space every filesystem reports; the real one when unset.
        pub free: std::cell::Cell<Option<u64>>,
    }

    impl<F: Fn(&Request<'_>) -> Result<Vec<u8>>> Scripted<F> {
        pub(crate) fn new(answer: F) -> Self {
            let asked = RefCell::new(Vec::new());
            let free = std::cell::Cell::new(None);
            Self {
                answer,
                asked,
                free,
            }
        }
    }

    impl<F: Fn(&Request<'_>) -> Result<Vec<u8>>> Commands for Scripted<F> {
        fn run(&self, request: &Request<'_>) -> Result<Vec<u8>> {
            let pairs = request.environment.iter();
            self.asked.borrow_mut().push(Asked {
                argv: request.argv.clone(),
                environment: pairs
                    .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
                    .collect(),
                input: request.input.map(<[u8]>::to_vec),
                source: request.source.map(std::path::Path::to_path_buf),
                archive: request.archive.map(std::path::Path::to_path_buf),
                ceiling: request.ceiling,
                seconds: request.seconds,
                limit: request.limit,
            });
            (self.answer)(request)
        }

        fn free(&self, path: &std::path::Path) -> Result<u64> {
            match self.free.get() {
                Some(bytes) => Ok(bytes),
                None => Local.free(path),
            }
        }
    }

    fn shell(script: &str, input: Option<&[u8]>, seconds: f64, limit: usize) -> Result<Vec<u8>> {
        let argv = vec!["/bin/sh".into(), "-c".into(), script.into()];
        Local.run(&Request {
            environment: &[("PATH", "/usr/bin:/bin"), ("MARK", "fixed")],
            input,
            ..Request::fixed(argv, seconds, limit, "Oversized test response")
        })
    }

    /// Output, input and only the given environment reach a real child.
    #[test]
    fn a_real_child_sees_only_the_given_environment_and_input() {
        let output = shell(
            "printf '%s:' \"$MARK\" \"${HOME-unset}\"; cat",
            Some(b"in"),
            5.0,
            64,
        );
        assert_eq!(output.unwrap(), b"fixed:unset:in");
        assert_eq!(shell("exit 3", None, 5.0, 64), Err(Failure::Command));
        assert_eq!(shell("echo late >&2; exit 0", None, 5.0, 0).unwrap(), b"");
    }

    /// Too much output is the caller's named refusal, never a truncated value.
    #[test]
    fn more_output_than_accepted_is_the_named_refusal() {
        assert_eq!(shell("printf 12345", None, 5.0, 5).unwrap(), b"12345");
        let refused = shell("printf 123456", None, 5.0, 5);
        assert_eq!(refused, Err(Failure::Refused("Oversized test response")));
        let flood = shell("head -c 3000000 /dev/zero", None, 5.0, 16);
        assert_eq!(flood, Err(Failure::Refused("Oversized test response")));
    }

    /// A child that outlives its deadline is killed with its whole group and
    /// reaped: a background member cannot hold the output open.
    #[test]
    fn a_late_child_and_its_group_are_killed_at_the_deadline() {
        let started = Instant::now();
        let late = shell("sleep 30 & sleep 30", None, 0.2, 64);
        assert_eq!(late, Err(Failure::Timeout));
        assert!(started.elapsed() < Duration::from_secs(5));
        let started = Instant::now();
        let detached = shell("sleep 30 & echo done", None, 5.0, 64);
        assert_eq!(detached.unwrap(), b"done\n");
        assert!(started.elapsed() < Duration::from_secs(5));
        let argv = vec!["/nonexistent/simplestchat-tool".into()];
        let missing = Local.run(&Request::fixed(argv, 1.0, 1, "Oversized test response"));
        assert_eq!(missing, Err(Failure::Command));
    }

    /// A bounded archive receives its output through the runner: exactly the
    /// ceiling passes, one byte more is the named refusal, and a flood is
    /// stopped with at most the ceiling retained. Kept standard error is
    /// bounded the same way, and each file is created new and private.
    #[test]
    fn retained_output_and_errors_are_private_and_bounded_by_the_runner() {
        use std::os::unix::fs::MetadataExt;
        let scratch = crate::host::state::tests::Scratch::new();
        let kept = |script: &str, name: &str, ceiling: u64| {
            let archive = scratch.path().join(format!("{name}.stdout"));
            let errors = scratch.path().join(format!("{name}.stderr"));
            let argv = vec!["/bin/sh".into(), "-c".into(), script.into()];
            let result = Local.run(&Request {
                archive: Some(&archive),
                ceiling: Some(ceiling),
                errors: Some(&errors),
                ..Request::fixed(argv, 5.0, 0, "Oversized test response")
            });
            let size = |path: &std::path::Path| std::fs::metadata(path).unwrap().len();
            for path in [&archive, &errors] {
                assert_eq!(std::fs::metadata(path).unwrap().mode() & 0o7777, 0o600);
            }
            (result, size(&archive), size(&errors))
        };
        assert_eq!(
            kept("printf 12345; printf oops >&2", "exact", 5),
            (Ok(Vec::new()), 5, 4)
        );
        let over = Err(Failure::Refused("command_stdout_limit_exceeded"));
        assert_eq!(kept("printf 123456", "over", 5), (over.clone(), 5, 0));
        let started = Instant::now();
        let (flood, retained, _) = kept("exec yes", "flood", 1024);
        assert_eq!((flood, retained), (over, 1024));
        assert!(started.elapsed() < Duration::from_secs(4));
        let noisy = "head -c 3000000 /dev/zero >&2; printf done";
        let (refused, _, errors) = kept(noisy, "noisy", 16);
        assert_eq!(
            refused,
            Err(Failure::Refused("command_stderr_limit_exceeded"))
        );
        assert_eq!(errors, 2 * 1024 * 1024);
        // A name that exists is never reused, whatever it holds.
        let argv = vec!["/bin/sh".into(), "-c".into(), "true".into()];
        let again = Local.run(&Request {
            archive: Some(&scratch.path().join("exact.stdout")),
            ceiling: Some(5),
            ..Request::fixed(argv, 5.0, 0, "Oversized test response")
        });
        assert_eq!(again, Err(Failure::Os));
        assert!(Local.free(scratch.path()).unwrap() > 0);
        assert_eq!(Local.free(&scratch.path().join("absent")), Err(Failure::Os));
    }
}
