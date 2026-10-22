//! Two fixed validators with owned Docker CLI groups and exact engine resources.
//! Caller-trusted Unix engine observations are not authenticated host authority.

mod capture;
mod execute;
mod files;
mod plan;
#[cfg(test)]
mod tests;

use super::{container_engine as engine, process::Native};
use crate::{owned_process::Signals, tooling};
use plan::{Context, Kind};
use std::{path::Path, process::ExitCode, time::Instant};

pub(super) fn run(root: &Path, arguments: &[String]) -> anyhow::Result<ExitCode> {
    const USAGE: &str =
        "usage: cargo run --locked -q -p xtask -- ci validate-config promtool|caddy";
    if arguments.len() == 1 && matches!(arguments[0].as_str(), "--help" | "-h") {
        println!("{USAGE}");
        return Ok(ExitCode::SUCCESS);
    }
    let Some(kind) = arguments
        .first()
        .and_then(|arg| Kind::parse(arg))
        .filter(|_| arguments.len() == 1)
    else {
        eprintln!("{USAGE}");
        return Ok(ExitCode::from(2));
    };
    let environment = tooling::inherited_env();
    let started = Instant::now();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| plan::failure())?;
    let result = runtime.block_on(async {
        let signals = Signals::new()?;
        let context = Context::new(
            root,
            environment,
            kind,
            rustix::system::uname().sysname().to_str().unwrap_or(""),
            started,
        )?;
        let inputs = files::Inputs::read(&context)?;
        let artifacts = match files::Artifacts::new(&context) {
            Ok(value) => value,
            Err(error) => {
                let _ = inputs.finish();
                return Err(error);
            }
        };
        let mut native = Native::new(root, &context.environment, signals);
        let discovery = {
            let mut capture = capture::Capture {
                inner: &mut native,
                artifacts: &artifacts,
                started: context.started,
            };
            engine::discover(
                &context.environment,
                &mut capture,
                engine::Policy::validation(),
            )
            .await
        };
        let endpoint = match discovery {
            Ok(value) => value,
            Err(error) => return execute::early(inputs, artifacts, error),
        };
        let signals = match native.into_signals().await {
            Ok(value) => value,
            Err(error) => return execute::early(inputs, artifacts, error),
        };
        let environment = engine::frozen_environment(&context.environment);
        let mut native = Native::new(root, &environment, signals);
        let mut clock = engine::NativeClock::new(started);
        execute::run(
            &context,
            &endpoint,
            &mut native,
            &mut clock,
            inputs,
            artifacts,
        )
        .await
    });
    match result {
        Ok(()) => Ok(ExitCode::SUCCESS),
        Err(error) => {
            eprintln!("Configuration validation failed (status {}).", error.code);
            Ok(ExitCode::from(error.code))
        }
    }
}
