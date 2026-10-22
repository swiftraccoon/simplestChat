//! Fixed session-continuity workload with one bounded in-process DB interruption.

#[path = "session_soak_engine.rs"]
mod engine;
#[path = "session_soak_plan.rs"]
mod plan;
#[path = "session_soak_policy.rs"]
mod policy;
#[cfg(test)]
#[path = "session_soak_tests.rs"]
mod tests;

use crate::{owned_process::Signals, test_env, tooling};
use std::{path::Path, process::ExitCode};
const USAGE: &str = "usage: cargo run --locked -q -p xtask -- ci session-soak";

pub(super) fn run(root: &Path, arguments: &[String]) -> anyhow::Result<ExitCode> {
    if arguments == ["--help"] {
        println!("{USAGE}");
        return Ok(ExitCode::SUCCESS);
    }
    if !arguments.is_empty() {
        eprintln!("{USAGE}");
        return Ok(ExitCode::from(2));
    }
    let environment = tooling::inherited_env();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let result: anyhow::Result<i32> = runtime.block_on(async {
        let mut signals = Signals::new()?;
        let context = plan::Context::new(
            root,
            environment,
            rustix::system::uname().sysname().to_str().unwrap_or(""),
        )?;
        let native = engine::Native {
            context: &context,
            settled: true,
        };
        let mut supervisor = policy::Supervisor::new(
            &context,
            native,
            policy::RealClock(std::time::Instant::now()),
        );
        supervisor.admit(&mut signals).await?;
        let result = test_env::server_with_environment(
            root,
            &context.workload(),
            &context.environment,
            &mut signals,
            &mut supervisor,
        )
        .await;
        let mut status = match result {
            Ok(status) => status,
            Err(error) => error
                .downcast_ref::<test_env::Stop>()
                .map_or(1, |v| i32::from(v.code)),
        };
        // Planning may fail before the harness acquires a workload/companion.
        status = test_env::Companion::finish(&mut supervisor, status, &mut signals).await;
        if status == 0 && policy::Clock::now(&supervisor.clock) >= plan::STEP {
            status = 124;
        }
        if status == 0
            && let Some(code) = signals.pending().await
        {
            status = i32::from(code);
        }
        supervisor.report.harness_status = status;
        supervisor.report.passed = status == 0;
        println!("{}", serde_json::to_string(&supervisor.report)?);
        Ok(status)
    });
    let code = match result {
        Ok(code) => u8::try_from(code).unwrap_or(1),
        Err(error) => error
            .downcast_ref::<tooling::Failure>()
            .map_or(1, |v| v.code),
    };
    if code != 0 {
        eprintln!("Session soak supervision failed.");
    }
    Ok(ExitCode::from(code))
}
