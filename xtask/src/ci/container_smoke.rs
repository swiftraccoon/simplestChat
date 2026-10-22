//! Two disposable containers, one frozen local engine and the complete image smoke.
//! The reference shell remains unchanged while inert command oracles establish
//! assertion parity. This command never builds an image or selects a remote host.

mod engine;
mod execute;
mod files;
mod http;
mod options;
mod plan;
#[cfg(test)]
mod tests;

use crate::{ci::process::Native, owned_process::Signals, tooling};
use options::{Context, Mode};
use std::{path::Path, process::ExitCode, time::Instant};

pub(super) fn run_ci(root: &Path, arguments: &[String]) -> anyhow::Result<ExitCode> {
    run(root, arguments, Mode::Ci)
}
pub(super) fn run_local(root: &Path, arguments: &[String]) -> anyhow::Result<ExitCode> {
    run(root, arguments, Mode::Local)
}

fn run(root: &Path, arguments: &[String], mode: Mode) -> anyhow::Result<ExitCode> {
    let usage = if mode == Mode::Ci {
        "usage: cargo run --locked -q -p xtask -- ci container-smoke"
    } else {
        "usage: cargo run --locked -q -p xtask -- container-smoke"
    };
    if arguments.len() == 1 && matches!(arguments[0].as_str(), "--help" | "-h") {
        println!("{usage}");
        return Ok(ExitCode::SUCCESS);
    }
    if !arguments.is_empty() {
        eprintln!("{usage}");
        return Ok(ExitCode::from(2));
    }
    let environment = tooling::inherited_env();
    let started = Instant::now();
    let result = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| engine::failure())
        .and_then(|runtime| {
            runtime.block_on(async {
                let signals = Signals::new()?;
                let context = Context::new(
                    root,
                    environment,
                    mode,
                    rustix::system::uname().sysname().to_str().unwrap_or(""),
                )?;
                let mut discovery = Native::new(&context.root, &context.environment, signals);
                let endpoint = engine::discover(&context, &mut discovery).await?;
                let signals = discovery.into_signals().await?;
                let environment = context.frozen_environment();
                let mut runner = Native::new(&context.root, &environment, signals);
                let mut clock = engine::NativeClock::new(started);
                execute::run(&context, &endpoint, &mut runner, &mut clock).await
            })
        });
    let result = match result {
        Ok(outcome) => {
            println!(
                "Private container smoke artifacts: {}",
                outcome.path.display()
            );
            outcome.result
        }
        Err(error) => Err(error),
    };
    match result {
        Ok(()) => {
            println!(concat!(
                "PASS production image: migrations, static UI and PWA files, health, ",
                "DB-backed API, registration and same-image restart."
            ));
            Ok(ExitCode::SUCCESS)
        }
        Err(error) => {
            eprintln!("Container smoke failed: {}", error.message);
            Ok(ExitCode::from(error.code))
        }
    }
}
