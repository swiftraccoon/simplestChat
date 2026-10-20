//! Owned child processes for the test harness: clean environments, process
//! groups, bounded termination and the status a shell would report.
//!
//! The workspace forbids unsafe code, so signals go through `kill(1)` rather
//! than a `kill(2)` binding; the few signals a run sends do not justify one.

use std::os::unix::process::ExitStatusExt;
use std::process::{ExitStatus, Stdio};
use std::time::Duration;
use tokio::process::{Child, Command};
use tokio::time::sleep;

/// The status a shell reports for a finished child: its exit code, or 128 plus
/// the signal that ended it.
pub(crate) fn shell_status(status: ExitStatus) -> i32 {
    status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(0))
}

/// Deliver `signal` to a process id, or to a process group when `target` is
/// negative; `true` when `kill(1)` accepted it.
pub(crate) async fn signal(target: i32, signal: &str) -> bool {
    Command::new("kill")
        .arg(format!("-{signal}"))
        .arg("--")
        .arg(target.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .is_ok_and(|status| status.success())
}

/// Whether a process or (negative id) process group still has a live member.
pub(crate) async fn alive(target: i32) -> bool {
    signal(target, "0").await
}

/// What termination of an owned child attempted and observed, recorded apart
/// from the status so a forced shutdown is never mistaken for a clean one.
#[derive(Debug, Default)]
pub(crate) struct Termination {
    pub term_attempted: bool,
    pub term_sent: bool,
    pub kill_attempted: bool,
    pub kill_sent: bool,
    pub wait_status: Option<i32>,
}

/// Ask a child to stop with TERM, wait up to `grace`, then KILL it; reap it and
/// keep the raw status.
pub(crate) async fn terminate(child: &mut Child, grace: Duration) -> Termination {
    let mut record = Termination::default();
    let Some(pid) = child.id() else {
        record.wait_status = child.try_wait().ok().flatten().map(shell_status);
        return record;
    };
    let pid = i32::try_from(pid).unwrap_or(i32::MAX);
    record.term_attempted = true;
    record.term_sent = signal(pid, "TERM").await;
    if !wait_for_exit(child, grace).await {
        record.kill_attempted = true;
        record.kill_sent = child.start_kill().is_ok();
    }
    record.wait_status = child.wait().await.ok().map(shell_status);
    record
}

/// Poll a child for up to `limit`; `true` once it has exited.
pub(crate) async fn wait_for_exit(child: &mut Child, limit: Duration) -> bool {
    let started = tokio::time::Instant::now();
    loop {
        if child.try_wait().ok().flatten().is_some() {
            return true;
        }
        if started.elapsed() >= limit {
            return child.try_wait().ok().flatten().is_some();
        }
        sleep(Duration::from_millis(100)).await;
    }
}

/// Stop a whole process group: TERM, wait up to `grace`, then KILL.
pub(crate) async fn terminate_group(group: i32, grace: Duration) {
    let _ = signal(-group, "TERM").await;
    let started = tokio::time::Instant::now();
    while alive(-group).await {
        if started.elapsed() >= grace {
            let _ = signal(-group, "KILL").await;
            break;
        }
        sleep(Duration::from_millis(100)).await;
    }
}

/// Thirty-two random bytes as lowercase hex, from the operating system.
pub(crate) fn random_hex() -> std::io::Result<String> {
    use std::io::Read;
    let mut bytes = [0u8; 32];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}
