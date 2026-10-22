//! Terminate only a process group whose unreaped leader still reserves its ID.

use super::process::exited;
use super::process_group::Group;
use anyhow::Result;
#[cfg(target_os = "macos")]
use anyhow::bail;
use nix::errno::Errno;
use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;
use std::time::Duration;

pub(super) async fn stop(group: &mut Group) -> Result<()> {
    let pid = group.pid;
    let terminate = signal(pid, Signal::SIGTERM).await;
    let _ = tokio::time::timeout(Duration::from_secs(5), exited(pid)).await;
    let kill = signal(pid, Signal::SIGKILL).await;
    let exited = tokio::time::timeout(Duration::from_secs(5), exited(pid)).await;
    // The standard child has no background reaper. Reap only after NOWAIT
    // proves exit; until then the leader continues to reserve this exact PGID.
    let reaped = if matches!(exited, Ok(Ok(_))) {
        Some(group.reap())
    } else {
        None
    };
    terminate?;
    kill?;
    exited.map_err(|_| anyhow::anyhow!("command_reap_timed_out"))??;
    reaped.expect("exit was proved before reaping")?;
    Ok(())
}

async fn signal(pid: Pid, signal: Signal) -> Result<()> {
    match killpg(pid, signal) {
        Ok(()) | Err(Errno::ESRCH) => Ok(()),
        #[cfg(target_os = "macos")]
        Err(Errno::EPERM) if zombie_group(pid).await => Ok(()),
        Err(error) => Err(error.into()),
    }
}

#[cfg(target_os = "macos")]
fn leader_exited(pid: Pid) -> bool {
    matches!(super::process::peek(pid), Ok(Some(_)))
}

/// Darwin excludes zombies from group signaling. Confirm every reported member
/// is a zombie before accepting EPERM; inspect no arguments or environment.
#[cfg(target_os = "macos")]
async fn zombie_group(pid: Pid) -> bool {
    use super::process_io::{Destination, read_limited};
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
pub(super) fn zombie_rows(bytes: &[u8], pid: Pid) -> bool {
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
