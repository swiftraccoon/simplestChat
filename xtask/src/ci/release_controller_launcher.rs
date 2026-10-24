//! A two-phase handoff. Launcher authority ends before commit can be visible.

use super::{
    files::Owned,
    plan::{self, Context},
    protocol::{Action, Intent, Ready},
    worker,
};
use crate::{
    owned_process::{self, Group, Signals},
    tooling::{self, Failure, Result},
};
use std::{
    os::unix::process::CommandExt,
    process::{Child, Stdio},
    time::{Duration, Instant},
};

struct Launch(Option<Child>);
impl Launch {
    fn release(mut self) -> Child {
        // Standard Child::drop does not kill or wait. From this instruction on,
        // no launcher path may signal a PID/group, including publication errors.
        self.0.take().unwrap()
    }
}
impl Drop for Launch {
    fn drop(&mut self) {
        if let Some(child) = self.0.take() {
            drop(Group::new(child));
        }
    }
}
pub(super) async fn start(context: &Context, signals: &mut Signals) -> Result<()> {
    let executable = std::env::current_exe().map_err(|_| plan::failure())?;
    let proof = super::source::Source::open(&executable, 256 * 1024 * 1024).map_err(|error| {
        Failure::new(
            error.code,
            "Release controller executable admission failed.",
        )
    })?;
    let result = start_with(
        context,
        signals,
        |context, _, _, _| {
            let mut command = tooling::command(
                &context.root,
                &context.environment,
                &executable,
                ["ci", "release-controller", "__worker"],
            );
            command
                .process_group(0)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            command
                .spawn()
                .map_err(|_| Failure::new(1, "Release controller worker launch failed."))
        },
        drop,
        |owned, intent, digest| worker::request(owned, intent, digest, Action::Commit),
        || {
            proof.validate().map_err(|error| {
                Failure::new(
                    error.code,
                    "Release controller executable changed during startup.",
                )
            })
        },
        plan::STARTUP,
    )
    .await;
    let closed = proof.finish();
    result.and(closed)
}
pub(super) async fn start_with(
    context: &Context,
    signals: &mut Signals,
    spawn: impl FnOnce(&Context, &Owned, &Intent, &str) -> Result<Child>,
    release: impl FnOnce(Child),
    publish: impl FnOnce(&Owned, &Intent, &str) -> Result<()>,
    mut validate: impl FnMut() -> Result<()>,
    startup: Duration,
) -> Result<()> {
    if let Some(code) = signals.pending().await {
        return Err(Failure::new(code, "Release controller start interrupted."));
    }
    let (owned, intent, digest) = Owned::create(context).map_err(|error| {
        Failure::new(
            error.code,
            "Release controller namespace preparation failed.",
        )
    })?;
    let result: Result<()> = async {
        validate()?;
        let child = spawn(context, &owned, &intent, &digest)?;
        let launch = Launch(Some(child));
        let pid = rustix::process::Pid::from_raw(
            i32::try_from(launch.0.as_ref().unwrap().id()).map_err(|_| plan::failure())?,
        )
        .ok_or_else(plan::failure)?;
        let deadline = Instant::now() + startup;
        loop {
            if Instant::now() >= deadline {
                return Err(Failure::new(124, "Release controller startup timed out."));
            }
            if let Some(code) = signals.pending().await {
                return Err(Failure::new(code, "Release controller start interrupted."));
            }
            if let Some(ready) = owned.spool.json::<Ready>("ready.json")? {
                if ready.intent_sha256 != digest {
                    return Err(plan::failure());
                }
                break;
            }
            if owned_process::exited_now(pid)? {
                return Err(Failure::new(
                    1,
                    "Release controller worker exited before readiness.",
                ));
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        validate()?;
        if Instant::now() >= deadline {
            return Err(Failure::new(124, "Release controller startup timed out."));
        }
        release(launch.release());
        // Irrevocably detached BEFORE the first commit publication syscall.
        let committed = if let Some(code) = signals.pending().await {
            Err(Failure::new(
                code,
                "Release controller handoff interrupted.",
            ))
        } else if Instant::now() >= deadline {
            Err(Failure::new(124, "Release controller startup timed out."))
        } else {
            publish(&owned, &intent, &digest)
        };
        if committed.is_err() {
            let _ = worker::request(&owned, &intent, &digest, Action::Cancel);
        }
        committed?;
        if let Some(code) = signals.pending().await {
            let _ = worker::request(&owned, &intent, &digest, Action::Cancel);
            return Err(Failure::new(
                code,
                "Release controller handoff interrupted.",
            ));
        }
        Ok(())
    }
    .await;
    let closed = owned.finish();
    if result.is_err() || closed.is_err() {
        // A reopened capability request is the sole post-handoff authority.
        if let Ok(owned) = Owned::open(context) {
            if let Ok((intent, digest)) = owned.intent(context) {
                let _ = worker::request(&owned, &intent, &digest, Action::Cancel);
            }
            let _ = owned.finish();
        }
    }
    result.and(closed)
}
