//! Complete unsigned manual generator-image owner. No image is published/deployed.

mod builder;
mod capture;
mod execute;
mod files;
mod hash;
mod image;
mod plan;
#[cfg(test)]
mod tests;
pub(super) mod tools;
mod transfer;

use super::{container_engine as engine, process::Native};
use crate::{owned_process::Signals, tooling};
use std::{path::Path, process::ExitCode, time::Instant};

pub(super) fn run(root: &Path, arguments: &[String]) -> anyhow::Result<ExitCode> {
    const USAGE: &str = "usage: cargo run --locked -q -p xtask -- ci load-generator-image";
    if arguments == ["--help"] {
        println!("{USAGE}");
        return Ok(ExitCode::SUCCESS);
    }
    if !arguments.is_empty() {
        eprintln!("{USAGE}");
        return Ok(ExitCode::from(2));
    }
    let started = Instant::now();
    let environment = tooling::inherited_env();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| plan::failure())?;
    let result = runtime.block_on(async {
        let signals = Signals::new()?;
        let context = plan::Context::new(
            root,
            environment,
            rustix::system::uname().sysname().to_str().unwrap_or(""),
            started,
        )?;
        let files = files::Owned::new(&context)?;
        let tools = match tools::Tools::select(root, &context.environment) {
            Ok(value) => value,
            Err(error) => return execute::early(files, None, error),
        };
        let mut native = Native::new(&context.root, &context.environment, signals);
        let endpoint = {
            let mut capture = capture::Capture {
                inner: &mut native,
                files: &files,
                context: &context,
                tools: &tools,
            };
            engine::discover(
                &context.environment,
                &mut capture,
                engine::Policy::load_image(),
            )
            .await
        };
        let endpoint = match endpoint {
            Ok(value) => value,
            Err(error) => return execute::early(files, Some(tools), error),
        };
        let signals = match native.into_signals().await {
            Ok(value) => value,
            Err(error) => return execute::early(files, Some(tools), error),
        };
        let environment = files.environment(&context.environment);
        let mut native = Native::new(&context.root, &environment, signals);
        execute::run(&context, &endpoint, &mut native, files, tools).await
    });
    match result {
        Ok(()) => Ok(ExitCode::SUCCESS),
        Err(error) => {
            eprintln!("Load image preparation failed (status {}).", error.code);
            Ok(ExitCode::from(error.code))
        }
    }
}
