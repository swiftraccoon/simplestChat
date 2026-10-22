//! Terminate only a process group whose unreaped leader still reserves its ID.

use super::exited;
use super::group::Group;
use anyhow::Result;
#[cfg(target_os = "macos")]
use anyhow::bail;
use nix::errno::Errno;
use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;
use std::time::Duration;

pub(super) async fn stop(group: &mut Group) -> Result<()> {
    stop_with_grace(group, Duration::from_secs(5))
        .await
        .map(|_| ())
}

/// Observed reserved-group signals and reap. Missing observations stay absent;
/// an accepted ESRCH/zombie outcome never asserts a delivered signal.
#[derive(Debug, Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Retirement {
    pub(crate) term_attempted: bool,
    pub(crate) term_sent: bool,
    pub(crate) kill_attempted: bool,
    pub(crate) kill_sent: bool,
    pub(crate) leader_terminal_before_cleanup: Option<bool>,
    pub(crate) leader_grace_expired: Option<bool>,
    pub(crate) wait_code: Option<i32>,
    pub(crate) wait_signal: Option<i32>,
    pub(crate) group_cleanup_passed: bool,
    pub(crate) streams_closed: bool,
}

pub(super) async fn stop_with_grace(
    group: &mut Group,
    grace: Duration,
) -> Result<std::process::ExitStatus> {
    stop_with_report(group, grace, &mut Retirement::default()).await
}

pub(super) async fn stop_with_report(
    group: &mut Group,
    grace: Duration,
    report: &mut Retirement,
) -> Result<std::process::ExitStatus> {
    use std::os::unix::process::ExitStatusExt;
    let pid = group.pid;
    report.leader_terminal_before_cleanup = super::peek(pid).ok().map(|v| v.is_some());
    report.term_attempted = true;
    let terminate = signal_observed(pid, Signal::SIGTERM).await;
    report.term_sent = matches!(terminate, Ok(true));
    let grace_result = tokio::time::timeout(grace, exited(pid)).await;
    report.leader_grace_expired = match &grace_result {
        Ok(Ok(_)) => Some(false),
        Err(_) => Some(true),
        _ => None,
    };
    report.kill_attempted = true;
    let kill = signal_observed(pid, Signal::SIGKILL).await;
    report.kill_sent = matches!(kill, Ok(true));
    let exited = tokio::time::timeout(Duration::from_secs(5), exited(pid)).await;
    // Darwin can reject TERM just before NOWAIT exposes the leader's exit.
    // Reconcile only with a fresh all-zombie proof while the ID is reserved.
    #[cfg(target_os = "macos")]
    let (terminate, kill) = if (permission_error(&terminate) || permission_error(&kill))
        && matches!(&exited, Ok(Ok(_)))
        && zombie_group(pid).await
    {
        let reconcile = |result: Result<bool>| {
            if permission_error(&result) {
                Ok(false)
            } else {
                result
            }
        };
        (reconcile(terminate), reconcile(kill))
    } else {
        (terminate, kill)
    };
    // Retain a real reap even when an earlier signal error remains primary.
    let reaped = if matches!(exited, Ok(Ok(_))) {
        Some(group.reap())
    } else {
        None
    };
    if let Some(Ok(status)) = &reaped {
        report.wait_code = status.code();
        report.wait_signal = status.signal();
    }
    terminate?;
    kill?;
    exited.map_err(|_| anyhow::anyhow!("command_reap_timed_out"))??;
    let status = reaped.expect("exit was proved before reaping")?;
    report.group_cleanup_passed = true;
    Ok(status)
}

#[cfg(target_os = "macos")]
fn permission_error(result: &Result<bool>) -> bool {
    result
        .as_ref()
        .err()
        .is_some_and(|error| error.downcast_ref::<Errno>() == Some(&Errno::EPERM))
}

async fn signal_observed(pid: Pid, signal: Signal) -> Result<bool> {
    match killpg(pid, signal) {
        Ok(()) => Ok(true),
        Err(Errno::ESRCH) => Ok(false),
        #[cfg(target_os = "macos")]
        Err(Errno::EPERM) if zombie_group(pid).await => Ok(false),
        Err(error) => Err(error.into()),
    }
}

#[cfg(target_os = "macos")]
fn leader_exited(pid: Pid) -> bool {
    matches!(super::peek(pid), Ok(Some(_)))
}

/// Darwin excludes zombies from group signaling. Confirm every reported member
/// is a zombie before accepting EPERM; inspect no arguments or environment.
#[cfg(target_os = "macos")]
async fn zombie_group(pid: Pid) -> bool {
    use super::io::{Destination, read_limited};
    use std::process::Stdio;
    use tokio::process::Command;
    if !leader_exited(pid) {
        return false;
    }
    let work = async {
        let mut child = Command::new("/bin/ps")
            .args(["-g", &pid.as_raw().to_string(), "-o", "pid=,pgid=,stat="])
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("LC_ALL", "C")
            .process_group(0)
            .kill_on_drop(true)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");
        let result = tokio::time::timeout(Duration::from_secs(2), async {
            tokio::try_join!(
                read_limited(stdout, Destination::capture(), 65536, "stdout"),
                read_limited(stderr, Destination::capture(), 65536, "stderr"),
                async { Ok::<_, anyhow::Error>(child.wait().await?) }
            )
        })
        .await;
        let Ok(Ok((stdout, stderr, status))) = result else {
            let _ = child.start_kill();
            let _ = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
            bail!("command_group_inspection_failed");
        };
        if !status.success() || !stderr.is_empty() {
            bail!("command_group_inspection_failed");
        }
        Ok::<_, anyhow::Error>(zombie_rows(&stdout, pid) && leader_exited(pid))
    };
    work.await.unwrap_or(false)
}

#[cfg(any(target_os = "macos", test))]
pub(crate) fn zombie_rows(bytes: &[u8], pid: Pid) -> bool {
    let Ok(source) = std::str::from_utf8(bytes) else {
        return false;
    };
    let mut members = std::collections::BTreeSet::new();
    for row in source.lines() {
        let fields = row.split_whitespace().collect::<Vec<_>>();
        let number = |value: &str| {
            !value.is_empty()
                && value.len() <= 10
                && value.as_bytes()[0] != b'0'
                && value.bytes().all(|byte| byte.is_ascii_digit())
        };
        if fields.len() != 3
            || !number(fields[0])
            || !number(fields[1])
            || fields[1].parse::<i32>().ok() != Some(pid.as_raw())
            || !fields[2]
                .strip_prefix('Z')
                .is_some_and(|tail| tail.bytes().all(|byte| b"+<>AELNSsVWX".contains(&byte)))
        {
            return false;
        }
        let Ok(member) = fields[0].parse::<i32>() else {
            return false;
        };
        if !members.insert(member) {
            return false;
        }
    }
    members.contains(&pid.as_raw())
}
