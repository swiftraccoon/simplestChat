//! An unreaped standard child reserves the group identity until its final kill.

use anyhow::Result;
use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;
use std::process::Child;
use std::time::{Duration, Instant};

pub(super) struct Group {
    child: Option<Child>,
    pub(super) pid: Pid,
}
impl Group {
    pub(super) fn new(child: Child, pid: Pid) -> Self {
        Self {
            child: Some(child),
            pid,
        }
    }
    pub(super) fn child(&mut self) -> &mut Child {
        self.child.as_mut().expect("owned child")
    }
    /// Called only after NOWAIT proved a terminal state. Disarm before wait so
    /// even an unexpected wait error cannot cause a later signal to a reused ID.
    pub(super) fn reap(&mut self) -> Result<()> {
        self.child.take().expect("owned unreaped child").wait()?;
        Ok(())
    }
}
impl Drop for Group {
    fn drop(&mut self) {
        let Some(mut child) = self.child.take() else {
            return;
        };
        // Drop can occur without a running executor. Send the final signal
        // while this standard child still reserves the exact process group.
        // After try_wait may reap it, never signal the group again.
        let _ = killpg(self.pid, Signal::SIGKILL);
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            match child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) => std::thread::sleep(Duration::from_millis(5)),
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => return,
            }
        }
    }
}
