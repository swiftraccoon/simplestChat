//! The fixture's native runner: each command in its own process group with a
//! fixed environment, its streams on private files, stopped with TERM, its
//! grace and a group KILL when its bound, a signal or the work deadline ends
//! it, and reaped before the next command starts.

use super::{
    files::present,
    host::{Execution, Fault, Paths, Request, Result, Runner, Stop, Window, require},
    plan::TOOLS,
};
use simplestchat_toolkit::{
    executable::Executable,
    owned_process::{Group, Signals, exited},
    replay::{Finalize, Finalizer},
};
use std::{
    fs::File,
    os::{
        fd::{AsFd, OwnedFd},
        unix::{
            fs::OpenOptionsExt,
            process::{CommandExt, ExitStatusExt},
        },
    },
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, Instant},
};

/// The environment every command starts from; a request may add to it.
pub(super) const ENVIRONMENT: [(&str, &str); 2] =
    [("PATH", "/usr/sbin:/usr/bin:/sbin:/bin"), ("LC_ALL", "C")];
const MACHINES: [&str; 4] = ["x86_64", "amd64", "aarch64", "arm64"];
const FREE: u64 = 4 * 1024 * 1024 * 1024;

/// The host the fixture runs on, as preflight sees it.
pub(super) struct Platform<'a> {
    pub system: &'a str,
    pub machine: &'a str,
    pub root: bool,
}

/// Refuse a host that is not Linux on AMD64 or ARM64 run as root, any
/// existing service or benchmark path, missing tooling, an occupied
/// fixture port and less than 4 GiB free for fixture data, in that order:
/// nothing later is probed after a refusal.
pub(super) fn preflight(
    platform: &Platform<'_>,
    paths: &Paths,
    tools: &[&str],
    ports: impl FnOnce() -> std::io::Result<()>,
    free: impl FnOnce() -> std::io::Result<u64>,
) -> Result<()> {
    require(
        platform.system == "Linux" && MACHINES.contains(&platform.machine) && platform.root,
        "Use a fresh disposable Linux amd64/arm64 host as root; never the public VPS",
    )?;
    for path in paths.service() {
        require(
            !present(path),
            "Existing service or benchmark path; refusing fixture setup",
        )?;
    }
    for tool in tools {
        let usable = rustix::fs::access(*tool, rustix::fs::Access::EXEC_OK).is_ok();
        require(
            usable,
            "Required Docker, TLS or namespace tooling is missing",
        )?;
    }
    // Bind checks only: an existing service is never probed.
    require(ports().is_ok(), "A fixture port is already in use")?;
    require(
        free().is_ok_and(|free| free >= FREE),
        "At least 4 GiB of free fixture space is required",
    )
}

fn bind() -> std::io::Result<()> {
    drop(std::net::TcpListener::bind(("127.0.0.1", 3000))?);
    drop(std::net::TcpListener::bind(("127.0.0.1", 443))?);
    drop(std::net::UdpSocket::bind(("127.0.0.1", 40000))?);
    Ok(())
}

fn free(path: &Path) -> std::io::Result<u64> {
    let statistics = rustix::fs::statvfs(path)?;
    Ok(statistics.f_bavail.saturating_mul(statistics.f_frsize))
}

/// The native runner over the fixture's one signal registration.
pub(super) struct Native {
    signals: Signals,
    latched: bool,
    checkout: PathBuf,
    policy: Executable,
}
impl Native {
    pub(super) fn new(signals: Signals, checkout: &Path, policy: Executable) -> Self {
        Self {
            signals,
            latched: false,
            checkout: checkout.to_path_buf(),
            policy,
        }
    }
    /// Release the retained response-check executable.
    pub(super) fn finish(self) -> Result<()> {
        self.policy
            .close()
            .map_err(|_| Fault::Check("HTTP policy binary close failed"))
    }

    fn spawn(&self, request: &mut Request, streams: [File; 2]) -> Option<Group> {
        let [stdout, stderr] = streams;
        let (program, arguments) = request.argv.split_first()?;
        let mut command = std::process::Command::new(program);
        command
            .args(arguments)
            .env_clear()
            .envs(ENVIRONMENT)
            .envs(request.environment.iter().map(|(key, value)| (key, value)))
            .current_dir(request.directory.as_deref().unwrap_or(&self.checkout))
            .process_group(0)
            .stdout(stdout)
            .stderr(stderr)
            .stdin(Stdio::null());
        if let Some(input) = &request.input {
            let file = std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
                .open(input)
                .ok()?;
            command.stdin(file);
        }
        // The child inherits this one descriptor under its own number; the
        // fixture starts nothing else until the command has been reaped.
        if let Some(inherited) = &request.inherit {
            rustix::io::fcntl_setfd(inherited.as_fd(), rustix::io::FdFlags::empty()).ok()?;
        }
        let child = command.spawn();
        if let Some(inherited) = request.inherit.take() {
            drop(inherited);
        }
        child.ok().map(Group::new)
    }
}

impl Runner for Native {
    async fn execute(
        &mut self,
        request: &mut Request,
        streams: [File; 2],
        timeout: Duration,
        window: Window,
    ) -> Execution {
        let Some(mut group) = self.spawn(request, streams) else {
            return Execution {
                status: None,
                stopped: false,
                stop: Some(Stop::Unlaunched),
            };
        };
        let pid = group.pid();
        let end = Instant::now() + timeout;
        // `Err(())`: the leader could not be observed, so nothing about its
        // end is known; its group is finished below all the same.
        let stop: std::result::Result<Option<Stop>, ()> = loop {
            let work = match window {
                Window::Work(work) => work,
                Window::Cleanup(_) => end,
            };
            tokio::select! {
                biased;
                result = exited(pid) => break result.map(|()| None).map_err(|_| ()),
                _ = tokio::time::sleep_until(end.into()) => break Ok(Some(Stop::Timeout)),
                _ = tokio::time::sleep_until(work.into()), if matches!(window, Window::Work(_)) => {
                    break Ok(Some(Stop::Interrupted))
                }
                _ = self.signals.next() => {
                    self.latched = true;
                    if matches!(window, Window::Work(_)) {
                        break Ok(Some(Stop::Interrupted));
                    }
                }
            }
        };
        let observed = stop.is_ok();
        let stop = stop.unwrap_or(None);
        if stop.is_some() || !observed {
            let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::TERM);
            let _ = tokio::time::timeout(request.grace, exited(pid)).await;
        }
        let (cleaned, reaped) = Finalize.finish(&mut group).await;
        let status = reaped
            .ok()
            .filter(|_| stop.is_none() && observed)
            .and_then(|status| {
                status
                    .code()
                    .or_else(|| status.signal().map(|signal| -signal))
            });
        Execution {
            status,
            stopped: stop.is_some() || !cleaned || status.is_none(),
            stop,
        }
    }

    fn now(&self) -> Instant {
        Instant::now()
    }

    async fn pause(&mut self, duration: Duration, window: Window) -> Result<()> {
        let end = Instant::now() + duration;
        loop {
            tokio::select! {
                biased;
                _ = tokio::time::sleep_until(end.into()) => return Ok(()),
                _ = self.signals.next() => {
                    self.latched = true;
                    if let Window::Work(_) = window {
                        return Err(Fault::Interrupted);
                    }
                }
            }
        }
    }

    async fn signalled(&mut self) -> bool {
        let pending = self.signals.pending().await.is_some();
        self.latched |= pending;
        pending
    }

    fn latched(&self) -> bool {
        self.latched
    }

    fn chown(&mut self, path: &Path, owner: u32) -> Result<()> {
        std::os::unix::fs::chown(path, Some(owner), Some(owner))
            .map_err(|_| Fault::Check("Cannot set fixture file ownership"))
    }

    fn namespace(&mut self, pid: i64) -> Result<OwnedFd> {
        File::open(format!("/proc/{pid}/ns/net"))
            .map(OwnedFd::from)
            .map_err(|_| Fault::Check("Migration network namespace unavailable"))
    }

    fn retained(&mut self, path: &Path) -> Result<()> {
        let selected = self.policy.selected_path();
        require(
            selected.is_ok_and(|selected| selected == path),
            "HTTP policy binary changed",
        )
    }

    fn preflight(&mut self, paths: &Paths) -> Result<()> {
        let system = rustix::system::uname();
        let platform = Platform {
            system: system.sysname().to_str().unwrap_or(""),
            machine: system.machine().to_str().unwrap_or(""),
            root: rustix::process::geteuid().is_root(),
        };
        let data = paths.root.parent().unwrap_or(Path::new("/"));
        preflight(&platform, paths, &TOOLS, bind, || free(data))
    }
}
