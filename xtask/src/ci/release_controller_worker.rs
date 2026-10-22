//! The worker owns CLI groups. The detached worker itself is not a child owned
//! by the collector, and no receipt proves root/session/engine cleanup.

use super::{
    files::{self, Directory, Owned},
    log::Log,
    plan::{self, Context, Stage},
    process::{self, Control},
    protocol::{Action, Intent, Ready, Request, Terminal},
};
use crate::tooling::Result;
use std::time::{Duration, Instant};

pub(super) fn request(owned: &Owned, intent: &Intent, digest: &str, action: Action) -> Result<()> {
    let name = if action == Action::Commit {
        "commit.json"
    } else {
        "cancel.json"
    };
    // Idempotence accepts only this exact request. It never replaces a terminal
    // record or changes another invocation's capability.
    if let Some(old) = owned.spool.json_limit::<Request>(name, 4096)? {
        return if old.intent_sha256 == digest
            && old.capability == intent.capability
            && old.action == action
        {
            Ok(())
        } else {
            Err(plan::failure())
        };
    }
    owned.spool.publish(
        name,
        &Request {
            intent_sha256: digest.into(),
            capability: intent.capability.clone(),
            action,
        },
    )
}
pub(super) fn prefix(context: &Context, intent: &Intent) -> Result<()> {
    let directory = Directory::open(&context.temporary)?.descend(plan::PREFIX)?;
    let same = files::identity(directory.file()).map(|value| value == intent.prefix);
    let closed = directory.finish();
    if !same? {
        return Err(plan::failure());
    }
    closed
}
pub(super) async fn committed(
    owned: &Owned,
    intent: &Intent,
    digest: &str,
    control: &mut Control<'_>,
    deadline: Instant,
) -> Result<bool> {
    loop {
        if control.check(owned, intent, digest).await?.is_some() {
            return Ok(false);
        }
        if Instant::now() >= deadline {
            control.code = Some(124);
            return Ok(false);
        }
        if let Some(commit) = owned.spool.json_limit::<Request>("commit.json", 4096)? {
            if commit.intent_sha256 != digest
                || commit.capability != intent.capability
                || commit.action != Action::Commit
            {
                return Err(plan::failure());
            }
            if Instant::now() >= deadline {
                control.code = Some(124);
                return Ok(false);
            }
            return Ok(true);
        }
        if Instant::now() >= deadline {
            control.code = Some(124);
            return Ok(false);
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
pub(super) async fn execute(
    context: &Context,
    signals: &mut crate::owned_process::Signals,
) -> Result<u8> {
    let owned = Owned::open(context)?;
    let result = work(context, &owned, signals).await;
    let closed = owned.finish();
    let code = result?;
    closed?;
    Ok(code)
}
async fn work(
    context: &Context,
    owned: &Owned,
    signals: &mut crate::owned_process::Signals,
) -> Result<u8> {
    let (intent, digest) = owned.intent(context)?;
    context.validate()?;
    prefix(context, &intent)?;
    let mut control = Control::new(signals);
    let log = Log::start(&owned.parent, &intent, plan::MAX_LOG)?;
    owned.spool.publish(
        "ready.json",
        &Ready {
            intent_sha256: digest.clone(),
        },
    )?;
    let admitted = committed(
        owned,
        &intent,
        &digest,
        &mut control,
        Instant::now() + plan::STARTUP,
    )
    .await;
    let deadline = Instant::now() + plan::WORKER - plan::RESERVE;
    let mut status = if matches!(admitted, Ok(true)) {
        0
    } else {
        control.code.unwrap_or(1)
    };
    let mut settled = true;
    let mut stage: Option<Stage> = None;
    if status == 0 {
        for current in plan::STAGES {
            stage = Some(current);
            match (process::Invocation {
                context,
                owned,
                intent: &intent,
                digest: &digest,
            })
            .run(current, &log, &mut control, deadline)
            .await
            {
                Ok(outcome) => {
                    status = outcome.status;
                    settled &= outcome.settled;
                }
                Err(error) => {
                    status = error.code;
                    settled = false;
                }
            }
            if status != 0 || !settled {
                break;
            }
        }
    }
    // Retire groups before EOF. A holder outside the CLI group gets only the
    // bounded reader drain interval, never an unbounded inherited-pipe wait.
    let log = log.finish().await?;
    owned.parent.check(plan::LOG, &log.metadata)?;
    if !log.complete && status == 0 {
        status = 1;
    }
    control.check(owned, &intent, &digest).await?;
    let status_bytes = format!("{status}\n").into_bytes();
    owned.parent.atomic(plan::STATUS, &status_bytes)?;
    let observed = owned
        .parent
        .read(plan::STATUS, 16)?
        .ok_or_else(plan::failure)?;
    if observed != status_bytes {
        return Err(plan::failure());
    }
    // Only already observed cancellation wins here. A simultaneous unobserved
    // request may race publication; the collector independently fences success.
    control.check(owned, &intent, &digest).await?;
    context.validate()?;
    prefix(context, &intent)?;
    context.close_requirements()?;
    control.check(owned, &intent, &digest).await?;
    let interruption = control.code;
    let prepared = status == 0 && interruption.is_none() && settled && log.complete;
    let terminal = Terminal {
        intent_sha256: digest,
        installer_status: status,
        interruption,
        stage,
        cli_groups_settled: settled,
        log_closed: true,
        log_sha256: log.sha256,
        log_bytes: log.bytes,
        status_sha256: plan::digest(&status_bytes),
        status_bytes: status_bytes.len() as u64,
        prepared,
    };
    owned.spool.publish("terminal.json", &terminal)?;
    Ok(outcome(status, interruption, prepared))
}

pub(super) fn outcome(status: u8, interruption: Option<u8>, prepared: bool) -> u8 {
    if status != 0 {
        status
    } else {
        interruption.unwrap_or(if prepared { 0 } else { 1 })
    }
}
