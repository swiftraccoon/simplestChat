//! Darwin's zombie-only process-group signaling exception, proved narrowly.

use crate::owned_process::reserved;
use rustix::process::{Pid, Signal};
use std::time::Instant;

/// Finish the exact still-reserved group. Uncertain permission/wait proof is false.
/// Call before consuming the leader; success does not itself reap or close pipes.
pub async fn finish(pid: Pid) -> bool {
    finish_inner(pid, None).await
}

/// Finish a reserved group inside the caller's original absolute cleanup end.
/// This adds no whole-operation clock; late settlement remains negative.
pub async fn finish_before(pid: Pid, end: Instant) -> bool {
    finish_inner(pid, Some(end)).await
}

async fn finish_inner(pid: Pid, end: Option<Instant>) -> bool {
    let late = end.is_some_and(|end| Instant::now() >= end);
    if !reserved(pid) {
        return false;
    }
    match rustix::process::kill_process_group(pid, Signal::KILL) {
        Ok(()) | Err(rustix::io::Errno::SRCH) => !late,
        #[cfg(target_os = "macos")]
        Err(rustix::io::Errno::PERM) => zombie_group(pid, end).await,
        Err(_) => false,
    }
}

#[cfg(target_os = "macos")]
fn leader_exited(pid: Pid) -> bool {
    use rustix::process::{WaitId, WaitIdOptions};
    matches!(
        rustix::process::waitid(
            WaitId::Pid(pid),
            WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT
        ),
        Ok(Some(_))
    )
}

/// Inspect no command lines or environments. Keep the leader unreaped before
/// and after the bounded inventory so a recycled group can never pass proof.
#[cfg(target_os = "macos")]
async fn zombie_group(pid: Pid, end: Option<Instant>) -> bool {
    use std::process::Stdio;
    use std::time::Duration;
    use tokio::io::AsyncReadExt;
    use tokio::process::Command;

    async fn read(reader: &mut (impl tokio::io::AsyncRead + Unpin)) -> std::io::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        reader.take(65537).read_to_end(&mut bytes).await?;
        if bytes.len() > 65536 {
            return Err(std::io::Error::other("bounded group inventory"));
        }
        Ok(bytes)
    }

    if !leader_exited(pid) || end.is_some_and(|end| Instant::now() >= end) {
        return false;
    }
    let Ok(mut child) = Command::new("/bin/ps")
        .args([
            "-g",
            &pid.as_raw_nonzero().get().to_string(),
            "-o",
            "pid=,pgid=,stat=",
        ])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
    else {
        return false;
    };
    let mut output = child.stdout.take().expect("piped stdout");
    let mut errors = child.stderr.take().expect("piped stderr");
    let body = end.map_or(Duration::from_secs(2), |end| {
        end.saturating_duration_since(Instant::now())
            .min(Duration::from_secs(2))
    });
    // Keep both owned readers outside the cancellable body. A read failure or
    // timeout must not drop one pipe instead of attempting its explicit close.
    let result = tokio::time::timeout(body, async {
        tokio::join!(read(&mut output), read(&mut errors), child.wait())
    })
    .await;
    let accepted = match result {
        Ok((Ok(output), Ok(errors), Ok(status))) => Some((output, errors, status)),
        _ => {
            let _ = child.start_kill();
            if end.is_some() {
                // A deadline cannot detach this admitted known no-fork peer.
                // Kernel wait/reap latency is outside a hard wall bound.
                let _ = child.wait().await;
            } else {
                let _ = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
            }
            None
        }
    };
    let output_closed = output
        .into_owned_fd()
        .and_then(|fd| nix::unistd::close(fd).map_err(std::io::Error::from))
        .is_ok();
    let errors_closed = errors
        .into_owned_fd()
        .and_then(|fd| nix::unistd::close(fd).map_err(std::io::Error::from))
        .is_ok();
    let Some((output, errors, status)) = accepted else {
        return false;
    };
    output_closed
        && errors_closed
        && proof(&output, &errors, status.success(), pid)
        && leader_exited(pid)
        && end.is_none_or(|end| Instant::now() < end)
}

#[cfg(any(target_os = "macos", test))]
pub(crate) fn proof(output: &[u8], errors: &[u8], success: bool, pid: Pid) -> bool {
    if !success || !errors.is_empty() || output.len() > 65536 {
        return false;
    }
    let Ok(source) = std::str::from_utf8(output) else {
        return false;
    };
    let mut members = std::collections::BTreeSet::new();
    for row in source.lines() {
        let fields: Vec<_> = row.split_whitespace().collect();
        let number = |value: &str| {
            !value.is_empty()
                && value.len() <= 10
                && value.as_bytes()[0] != b'0'
                && value.bytes().all(|byte| byte.is_ascii_digit())
        };
        if fields.len() != 3
            || !number(fields[0])
            || !number(fields[1])
            || fields[1].parse::<i32>().ok() != Some(pid.as_raw_nonzero().get())
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
    members.contains(&pid.as_raw_nonzero().get())
}
