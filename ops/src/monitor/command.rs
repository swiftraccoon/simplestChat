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
    /// A new private file that keeps standard error as evidence; otherwise
    /// it is discarded.
    pub errors: Option<&'a std::path::Path>,
    pub seconds: f64,
    pub limit: usize,
    /// The refusal named when the command says more than `limit`.
    pub oversized: &'static str,
}

/// Something that can run a fixed command and return its standard output.
pub(crate) trait Commands {
    /// Standard output of a command that exited with status zero in time.
    fn run(&self, request: &Request<'_>) -> Result<Vec<u8>>;
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
            errors: None,
            seconds,
            limit,
            oversized,
        }
    }
}

impl Commands for Local {
    fn run(&self, request: &Request<'_>) -> Result<Vec<u8>> {
        let (program, arguments) = request.argv.split_first().ok_or(Failure::Command)?;
        let input = match (request.source, request.input) {
            (Some(path), _) => Stdio::from(std::fs::File::open(path)?),
            (None, Some(_)) => Stdio::piped(),
            (None, None) => Stdio::null(),
        };
        let archive = match request.archive {
            Some(path) => {
                let mut options = std::fs::OpenOptions::new();
                Some(
                    options
                        .write(true)
                        .create_new(true)
                        .mode(0o600)
                        .open(path)?,
                )
            }
            None => None,
        };
        let output = match &archive {
            Some(file) => Stdio::from(file.try_clone()?),
            None => Stdio::piped(),
        };
        let mut command = Command::new(program);
        command
            .args(arguments)
            .env_clear()
            .envs(request.environment.iter().copied())
            .stdin(input)
            .stdout(output)
            .stderr(match request.errors {
                Some(path) => {
                    let mut options = std::fs::OpenOptions::new();
                    Stdio::from(
                        options
                            .write(true)
                            .create_new(true)
                            .mode(0o600)
                            .open(path)?,
                    )
                }
                None => Stdio::piped(),
            })
            .process_group(0);
        let mut child = command.spawn().map_err(|_| Failure::Command)?;
        let group = i32::try_from(child.id()).map(Pid::from_raw);
        let input = child.stdin.take();
        let output = child.stdout.take();
        let error = child.stderr.take();
        let limit = request.limit;
        let finished = std::thread::scope(|scope| {
            let stdout = scope.spawn(move || match output {
                Some(output) => bounded(output, limit),
                None => (Vec::new(), 0),
            });
            let stderr = scope.spawn(move || error.map(|error| bounded(error, 0)));
            if let (Some(mut pipe), Some(bytes)) = (input, request.input) {
                scope.spawn(move || {
                    let _ = pipe.write_all(bytes);
                });
            }
            let deadline = Instant::now() + Duration::from_secs_f64(request.seconds);
            let status = loop {
                match child.try_wait() {
                    Ok(Some(status)) => break Some(status),
                    Ok(None) if Instant::now() < deadline => {
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
            let _ = stderr.join();
            (status, stdout.join())
        });
        let (status, stdout) = finished;
        let (kept, total) = stdout.map_err(|_| Failure::Command)?;
        match status {
            None => return Err(Failure::Timeout),
            Some(status) if !status.success() => return Err(Failure::Command),
            Some(_) => {}
        }
        require(total <= request.limit, request.oversized)?;
        if let Some(file) = archive {
            file.sync_all()?;
        }
        Ok(kept)
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use std::cell::RefCell;

    /// One request as a scripted host saw it.
    #[derive(Clone, Debug)]
    pub(in super::super) struct Asked {
        pub argv: Vec<String>,
        pub environment: Vec<(String, String)>,
        pub input: Option<Vec<u8>>,
        pub source: Option<std::path::PathBuf>,
        pub archive: Option<std::path::PathBuf>,
        pub seconds: f64,
        pub limit: usize,
    }

    /// Commands answered from a script, recording what was asked.
    pub(in super::super) struct Scripted<F: Fn(&Request<'_>) -> Result<Vec<u8>>> {
        pub answer: F,
        pub asked: RefCell<Vec<Asked>>,
    }

    impl<F: Fn(&Request<'_>) -> Result<Vec<u8>>> Scripted<F> {
        pub(in super::super) fn new(answer: F) -> Self {
            let asked = RefCell::new(Vec::new());
            Self { answer, asked }
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
                seconds: request.seconds,
                limit: request.limit,
            });
            (self.answer)(request)
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
}
