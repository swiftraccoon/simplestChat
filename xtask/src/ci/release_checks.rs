//! Fixed browser discovery and the unchanged isolated release coordinator.

mod execute;
pub(super) mod files;
mod plan;
#[cfg(test)]
mod tests;

use crate::{
    ci::{browser_suite::process::Signals, process::Native},
    tooling,
};
use std::{path::Path, process::ExitCode};

const USAGE: &str = "usage: cargo run --locked -q -p xtask -- ci release-checks";

pub(super) fn run(root: &Path, arguments: &[String]) -> anyhow::Result<ExitCode> {
    if arguments.len() == 1 && matches!(arguments[0].as_str(), "--help" | "-h") {
        println!("{USAGE}");
        return Ok(ExitCode::SUCCESS);
    }
    if !arguments.is_empty() {
        eprintln!("{USAGE}");
        return Ok(ExitCode::from(2));
    }
    let system = rustix::system::uname();
    let result = (|| -> tooling::Result<()> {
        let context = plan::Context::new(
            root,
            tooling::inherited_env(),
            system.sysname().to_str().unwrap_or(""),
        )?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| files::failure("Release-check runtime unavailable."))?;
        runtime.block_on(async {
            let mut native = Native::new(&context.root, &context.environment, Signals::new()?);
            execute::execute(&context, &mut native).await
        })
    })();
    match result {
        Ok(()) => Ok(ExitCode::SUCCESS),
        Err(error) => {
            eprintln!("Disposable release checks failed.");
            Ok(ExitCode::from(error.code))
        }
    }
}
