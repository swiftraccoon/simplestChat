//! Reserved CLI groups belong to the worker, never to a later collector/PID.

use super::{
    files::Owned,
    log::Log,
    plan::{self, Context, Stage},
    protocol::{Action, Intent, Request},
};
use crate::{
    owned_process::{self, Group, Signals},
    tooling::{self, Result},
};
use rustix::process::Signal;
use std::{
    os::unix::process::CommandExt,
    process::Stdio,
    time::{Duration, Instant},
};

pub(super) struct Control<'a> {
    pub signals: &'a mut Signals,
    pub code: Option<u8>,
}
impl<'a> Control<'a> {
    pub fn new(signals: &'a mut Signals) -> Self {
        Self {
            signals,
            code: None,
        }
    }
    pub async fn check(
        &mut self,
        owned: &Owned,
        intent: &Intent,
        digest: &str,
    ) -> Result<Option<u8>> {
        if self.code.is_none() {
            self.code = self.signals.pending().await;
        }
        if self.code.is_none()
            && let Some(request) = owned.spool.json_limit::<Request>("cancel.json", 4096)?
        {
            if request.intent_sha256 != digest
                || request.capability != intent.capability
                || request.action != Action::Cancel
            {
                return Err(plan::failure());
            }
            self.code = Some(143);
        }
        Ok(self.code)
    }
}
pub(super) struct Outcome {
    pub status: u8,
    pub settled: bool,
}

pub(super) struct Invocation<'a> {
    pub context: &'a Context,
    pub owned: &'a Owned,
    pub intent: &'a Intent,
    pub digest: &'a str,
}
impl Invocation<'_> {
    pub async fn run(
        &self,
        stage: Stage,
        log: &Log,
        control: &mut Control<'_>,
        deadline: Instant,
    ) -> Result<Outcome> {
        let context = self.context;
        let owned = self.owned;
        let intent = self.intent;
        let digest = self.digest;
        context.validate()?;
        super::worker::prefix(context, intent)?;
        owned.parent.verify()?;
        owned.spool.verify()?;
        if owned.intent(context)?.1 != digest {
            return Err(plan::failure());
        }
        if Instant::now() >= deadline || log.failed() {
            return Ok(Outcome {
                status: if log.failed() { 1 } else { 124 },
                settled: true,
            });
        }
        if let Some(code) = control.check(owned, intent, digest).await? {
            return Ok(Outcome {
                status: code,
                settled: true,
            });
        }
        let mut command = context.command(stage);
        command
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::from(log.writer()?))
            .stderr(Stdio::from(log.writer()?));
        let child = command.spawn().map_err(|error| {
            crate::tooling::Failure::new(
                match error.kind() {
                    std::io::ErrorKind::NotFound => 127,
                    std::io::ErrorKind::PermissionDenied => 126,
                    _ => 1,
                },
                "Cannot start release controller command.",
            )
        })?;
        let mut group = Group::new(child);
        let pid = group.pid();
        drop(command);
        let observed: Result<Option<u8>> = async {
            loop {
                if log.failed() {
                    return Ok(Some(1));
                }
                if Instant::now() >= deadline {
                    return Ok(Some(124));
                }
                if let Some(code) = control.check(owned, intent, digest).await? {
                    return Ok(Some(code));
                }
                tokio::select! {
                    biased;
                    code = control.signals.next() => {
                        control.code = Some(code);
                        return Ok(Some(code));
                    },
                    ended = owned_process::exited(pid) => { ended?; return Ok(None); },
                    _ = tokio::time::sleep(Duration::from_millis(100)) => {},
                }
            }
        }
        .await;
        if !matches!(observed, Ok(None)) {
            let signal = if control.code == Some(130) {
                Signal::INT
            } else {
                Signal::TERM
            };
            let _ = rustix::process::kill_process_group(pid, signal);
            let _ = tokio::time::timeout(plan::GRACE, owned_process::exited(pid)).await;
        }
        let settled = owned_process::finish(pid).await;
        if !matches!(
            tokio::time::timeout(Duration::from_secs(5), owned_process::exited(pid)).await,
            Ok(Ok(()))
        ) {
            return Err(plan::failure());
        }
        let status = tooling::shell_status(group.reap().await?);
        let interrupted = observed?;
        Ok(Outcome {
            status: interrupted.unwrap_or(status),
            settled,
        })
    }
}
