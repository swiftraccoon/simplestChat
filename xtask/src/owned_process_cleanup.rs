//! Darwin's zombie-only process-group signaling exception, proved narrowly.

use crate::owned_process::reserved;
use rustix::process::{Pid, Signal};

pub(crate) async fn finish(pid: Pid) -> bool {
    if !reserved(pid) {
        return false;
    }
    match rustix::process::kill_process_group(pid, Signal::KILL) {
        Ok(()) | Err(rustix::io::Errno::SRCH) => true,
        #[cfg(target_os = "macos")]
        Err(rustix::io::Errno::PERM) => zombie_group(pid).await,
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
async fn zombie_group(pid: Pid) -> bool {
    use std::process::Stdio;
    use std::time::Duration;
    use tokio::io::AsyncReadExt;
    use tokio::process::Command;

    async fn read(reader: impl tokio::io::AsyncRead + Unpin) -> std::io::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        reader.take(65537).read_to_end(&mut bytes).await?;
        if bytes.len() > 65536 {
            return Err(std::io::Error::other("bounded group inventory"));
        }
        Ok(bytes)
    }

    if !leader_exited(pid) {
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
    let output = child.stdout.take().expect("piped stdout");
    let errors = child.stderr.take().expect("piped stderr");
    let result = tokio::time::timeout(Duration::from_secs(2), async {
        tokio::try_join!(read(output), read(errors), child.wait())
    })
    .await;
    let Ok(Ok((output, errors, status))) = result else {
        let _ = child.start_kill();
        let _ = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
        return false;
    };
    proof(&output, &errors, status.success(), pid) && leader_exited(pid)
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
