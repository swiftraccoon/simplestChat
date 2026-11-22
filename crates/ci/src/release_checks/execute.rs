//! Chromium discovery, then the root fixture and the scanner download as two
//! owned process groups at once. Each keeps its own outcome: one ordinary
//! failure neither stops nor hides the other, and the step passes only when
//! both passed.

use super::{
    files,
    plan::{self, Context},
};
use crate::{
    bounded_log::Log,
    command::{Command, SeparateLogs},
    process::Executor,
};
use serde::Serialize;
use simplestchat_toolkit::{
    executable::{self, Executable},
    tooling::{Failure, Result},
};
use std::{path::Path, sync::Arc, time::Instant};

/// One overlapped command's public status line; its output stays private.
#[derive(Debug, PartialEq, Serialize)]
pub(super) struct Check {
    pub name: &'static str,
    /// The child's exit status, when it ran to an exit of its own.
    pub status: Option<u8>,
    /// The runner's fixed reason when it did not, or could not start it.
    pub failure: Option<String>,
    pub seconds: f64,
}
impl Check {
    fn of(name: &'static str, started: Instant, result: &Result<Vec<u8>>) -> Self {
        let (status, failure) = match result {
            Ok(_) => (Some(0), None),
            Err(error) if error.is_child_exit() => (Some(error.code), None),
            Err(error) => (None, Some(error.message.clone())),
        };
        Self {
            name,
            status,
            failure,
            seconds: (started.elapsed().as_secs_f64() * 1000.0).round() / 1000.0,
        }
    }
    fn passed(&self) -> bool {
        self.status == Some(0) && self.failure.is_none()
    }
}

#[derive(Serialize)]
struct Summary<'a> {
    passed: bool,
    checks: &'a [Check; 2],
}

/// Attach new private stdout and stderr logs named after `name`.
fn logs(command: &mut Command, output: &Path, name: &str) -> Result<()> {
    let open = |suffix: &str| {
        files::create_private(&output.join(format!("{name}.{suffix}")))
            .map_err(|_| files::failure("Release-check log unavailable."))
            .and_then(|file| Log::start(file, plan::MAX_LOG))
    };
    command.separate_logs = Some(Arc::new(SeparateLogs {
        stdout: open("stdout")?,
        stderr: open("stderr")?,
    }));
    Ok(())
}

/// Run both overlapped commands to their own ends, then record them.
async fn overlap(
    context: &Context,
    commands: [Command; 2],
    runners: (&mut impl Executor, &mut impl Executor),
) -> Result<[Check; 2]> {
    let [fixture, scanner] = commands;
    let started = Instant::now();
    let (first, second) = tokio::join!(runners.0.run(fixture), runners.1.run(scanner));
    let checks = [
        Check::of("release-fixture", started, &first),
        Check::of("scanner-tools", started, &second),
    ];
    let passed = checks.iter().all(Check::passed);
    let summary = serde_json::to_vec_pretty(&Summary {
        passed,
        checks: &checks,
    })
    .map_err(|_| files::failure("Release-check summary unavailable."))?;
    files::write_private(&context.output().join("summary.json"), &summary)
        .map_err(|_| files::failure("Release-check summary unavailable."))?;
    for check in &checks {
        println!(
            "{}: status={}, failure={}, seconds={}",
            check.name,
            check
                .status
                .map_or("None".into(), |status| status.to_string()),
            check.failure.as_deref().unwrap_or("None"),
            check.seconds
        );
    }
    Ok(checks)
}

pub(super) async fn execute(
    context: &Context,
    runner: &mut impl Executor,
    scanner: &mut impl Executor,
) -> Result<()> {
    let deadline = Instant::now() + plan::BUDGET;
    runner.ready().await?;
    let http_policy = Executable::open(&context.http_policy)?;
    let node = match files::node(&context.root, &context.environment) {
        Ok(node) => Arc::new(node),
        Err(error) => {
            let _ = http_policy.close();
            return Err(error);
        }
    };
    let mut chromium = None;
    let mut sudo = None;
    let primary: Result<()> = async {
        http_policy.validate()?;
        let bytes = runner
            .run(plan::discover(Arc::clone(&node), deadline)?)
            .await?;
        node.validate()?;
        http_policy.validate()?;
        runner.ready().await?;
        chromium = Some(files::chromium(&bytes)?);
        sudo = Some(Arc::new(executable::select(
            &context.root,
            &context.environment,
            std::ffi::OsStr::new("sudo"),
        )?));
        let tools = [
            node.validate()?,
            chromium.as_ref().unwrap().validate()?,
            http_policy.validate()?,
        ];
        let mut fixture = plan::fixture(context, tools, sudo.as_ref().unwrap().clone(), deadline)?;
        let mut download = plan::scanner(context, deadline)?;
        files::private_directory(&context.output())
            .map_err(|_| files::failure("Release-check output unavailable."))?;
        logs(&mut fixture, &context.output(), "release-fixture")?;
        logs(&mut download, &context.output(), "scanner-tools")?;
        let checks = overlap(context, [fixture, download], (runner, scanner)).await?;
        node.validate()?;
        http_policy.validate()?;
        chromium.as_ref().unwrap().validate()?;
        runner.ready().await?;
        if !checks.iter().all(Check::passed) {
            let code = checks
                .iter()
                .find_map(|check| check.status.filter(|status| *status != 0))
                .unwrap_or(1);
            return Err(Failure::new(code, "A disposable release check failed."));
        }
        plan::fence(deadline)
    }
    .await;
    let closed_node = Arc::try_unwrap(node)
        .map_err(|_| files::failure("Release-check Node still borrowed."))
        .and_then(Executable::close);
    let closed_chromium = chromium.map_or(Ok(()), Executable::close);
    let closed_http = http_policy.close();
    let closed_sudo = sudo.map_or(Ok(()), |proof| {
        let checked = proof.validate().map(|_| ());
        let closed = Arc::try_unwrap(proof)
            .map_err(|_| files::failure("Release-check sudo still borrowed."))
            .and_then(Executable::close);
        checked.and(closed)
    });
    let final_clock = plan::fence(deadline);
    let cleaned = if runner.cleanup_passed() && scanner.cleanup_passed() {
        Ok(())
    } else {
        Err(files::failure("Release-check CLI cleanup unproven."))
    };
    primary
        .and(closed_node)
        .and(closed_chromium)
        .and(closed_http)
        .and(closed_sudo)
        .and(cleaned)
        .and(final_clock)
}
