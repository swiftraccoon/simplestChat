//! Actual fixed standalone backup-verification route; no engine or host override.
#![cfg(unix)]
use simplestchat_ops::owned_process::{self, Group};
use std::{
    ffi::OsString,
    fs::{self, File},
    io,
    os::{
        fd::OwnedFd,
        unix::{fs::DirBuilderExt, process::CommandExt},
    },
    path::PathBuf,
    process::{Command, ExitStatus, Stdio},
    sync::atomic::{AtomicUsize, Ordering},
    time::{Duration, Instant},
};
use tokio::io::AsyncReadExt;
static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "restore-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        Self(path)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir(&self.0).unwrap();
    }
}
struct Reply {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}
fn first(slot: &mut Option<io::Error>, error: io::Error) {
    if slot.is_none() {
        *slot = Some(error);
    }
}
async fn call(fixture: &Fixture, args: &[OsString], sink: Option<Stdio>) -> io::Result<Reply> {
    let end = Instant::now() + Duration::from_secs(30);
    let body = end - Duration::from_secs(10);
    let child = Command::new(env!("CARGO_BIN_EXE_simplestchat-ops"))
        .args(args)
        .env_clear()
        .current_dir(&fixture.0)
        .stdin(Stdio::null())
        .stdout(sink.unwrap_or_else(Stdio::piped))
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()?;
    let mut group = Group::new(child);
    let pid = group.pid();
    let mut error = None;
    // Transfer both original pipes before judging any fallible SDK conversion.
    let output = group.child().stdout.take();
    let errors = group.child().stderr.take();
    let mut output = match output
        .map(tokio::process::ChildStdout::from_std)
        .transpose()
    {
        Ok(stream) => stream,
        Err(cause) => {
            first(&mut error, cause);
            None
        }
    };
    let mut errors = match errors
        .map(tokio::process::ChildStderr::from_std)
        .transpose()
    {
        Ok(stream) => stream,
        Err(cause) => {
            first(&mut error, cause);
            None
        }
    };
    let mut out_done = output.is_none();
    let mut err_done = errors.is_none();
    let mut out = Vec::new();
    let mut err = Vec::new();
    let mut a = vec![0; 4096];
    let mut b = vec![0; 4096];
    while error.is_none() && Instant::now() < body {
        let terminal = owned_process::exited_now(pid);
        match terminal {
            Ok(true) if out_done && err_done => break,
            Err(cause) => {
                first(&mut error, io::Error::other(format!("leader: {cause:?}")));
                break;
            }
            _ => {}
        }
        let event = tokio::select! {
            result=async {
                output.as_mut().unwrap().read(&mut a).await
            }, if !out_done => Some((true,result)),
            result=async {
                errors.as_mut().unwrap().read(&mut b).await
            }, if !err_done => Some((false,result)),
            _=tokio::time::sleep(Duration::from_millis(5))=>None,
        };
        if let Some((stdout, result)) = event {
            match result {
                Ok(0) => {
                    if stdout {
                        out_done = true
                    } else {
                        err_done = true
                    }
                }
                Ok(count) => {
                    let (data, bytes) = if stdout {
                        (&mut out, &a[..count])
                    } else {
                        (&mut err, &b[..count])
                    };
                    if data.len() + count > 65536 {
                        first(&mut error, io::Error::other("bounded CLI output"));
                    } else {
                        data.extend_from_slice(bytes);
                    }
                }
                Err(cause) if cause.kind() == io::ErrorKind::Interrupted => {}
                Err(cause) => first(&mut error, cause),
            }
        }
    }
    let joined = matches!(
        tokio::time::timeout_at(end.into(), owned_process::exited(pid)).await,
        Ok(Ok(()))
    );
    let retired = owned_process::finish_before(pid, end).await;
    let status = group.reap().await;
    if let Some(stream) = output.take() {
        match stream.into_owned_fd() {
            Ok(file) => {
                if let Err(cause) = nix::unistd::close(file) {
                    first(&mut error, io::Error::from(cause));
                }
            }
            Err(cause) => first(&mut error, cause),
        }
    }
    if let Some(stream) = errors.take() {
        match stream.into_owned_fd() {
            Ok(file) => {
                if let Err(cause) = nix::unistd::close(file) {
                    first(&mut error, io::Error::from(cause));
                }
            }
            Err(cause) => first(&mut error, cause),
        }
    }
    if !joined || !retired || !out_done || !err_done || Instant::now() >= end {
        first(
            &mut error,
            io::Error::other("all CLI peers must settle inside original end"),
        );
    }
    let status = status.map_err(|cause| io::Error::other(format!("reap: {cause:?}")));
    if let Some(error) = error {
        return Err(error);
    }
    Ok(Reply {
        status: status?,
        stdout: out,
        stderr: err,
    })
}
fn arguments(tail: &[&str]) -> Vec<OsString> {
    ["release", "backup", "verify"]
        .iter()
        .chain(tail)
        .map(OsString::from)
        .collect()
}
fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}
#[test]
fn help_and_scoped_notices_run_in_empty_environment_before_host_effects() {
    let fixture = Fixture::new();
    let rt = runtime();
    for option in ["--help", "--licenses"] {
        let reply = rt.block_on(call(&fixture, &arguments(&[option]), None));
        let unchanged = fs::read_dir(&fixture.0).unwrap().next().is_none();
        let reply = reply.unwrap();
        assert!(unchanged && reply.status.success());
        assert!(reply.stderr.is_empty());
        if option == "--help" {
            assert!(
                reply
                    .stdout
                    .starts_with(b"Usage: simplestchat-ops release backup verify ")
            );
        } else {
            assert!(reply.stdout.windows(b"MIT".len()).any(|x| x == b"MIT"));
        }
    }
}
#[test]
fn actual_parser_distinguishes_required_native_budget_syntax_from_invalid_selection() {
    let fixture = Fixture::new();
    let rt = runtime();
    for (tail, status) in [
        (vec!["--release-attempt", "release.fixture000"], 2),
        (
            vec![
                "--release-attempt",
                "../private",
                "--timeout-seconds",
                "1200",
                "--finalization-seconds",
                "120",
            ],
            1,
        ),
        (
            vec![
                "--release-attempt",
                "release.fixture000",
                "--timeout-seconds",
                "+1200",
                "--finalization-seconds",
                "120",
            ],
            2,
        ),
        (
            vec![
                "--release-attempt",
                "release.fixture000",
                "--timeout-seconds",
                "1200",
                "--finalization-seconds",
                "0",
            ],
            2,
        ),
        (vec!["--help", "--release-attempt", "release.fixture000"], 2),
    ] {
        let reply = rt.block_on(call(&fixture, &arguments(&tail), None));
        let unchanged = fs::read_dir(&fixture.0).unwrap().next().is_none();
        let reply = reply.unwrap();
        assert!(unchanged);
        assert_eq!(reply.status.code(), Some(status));
        assert!(reply.stdout.is_empty());
    }
}
#[test]
fn checked_stdout_refuses_a_real_readonly_descriptor_for_help_and_notices() {
    let fixture = Fixture::new();
    let rt = runtime();
    for option in ["--help", "--licenses"] {
        let original = File::open("/dev/null").unwrap();
        let duplicate = original.try_clone().map(Stdio::from);
        let reply = match duplicate {
            Ok(sink) => rt.block_on(call(&fixture, &arguments(&[option]), Some(sink))),
            Err(cause) => Err(cause),
        };
        let closed = nix::unistd::close(OwnedFd::from(original));
        let unchanged = fs::read_dir(&fixture.0).unwrap().next().is_none();
        let reply = reply.unwrap();
        assert!(closed.is_ok() && unchanged);
        assert!(!reply.status.success());
    }
}
