//! Unconditional CI advisory audit, using the unchanged authenticated installer.

mod execute;
mod files;
mod plan;
#[cfg(test)]
mod tests;

use crate::{
    ci::{browser_suite::process::Signals, process::Native},
    tooling,
};
use std::{path::Path, process::ExitCode};
const USAGE: &str = "usage: cargo run --locked -q -p xtask -- ci audit-rust";
pub(super) fn run(root: &Path, arguments: &[String]) -> anyhow::Result<ExitCode> {
    if arguments.len() == 1 && matches!(arguments[0].as_str(), "--help" | "-h") {
        println!("{USAGE}");
        return Ok(ExitCode::SUCCESS);
    }
    if !arguments.is_empty() {
        eprintln!("{USAGE}");
        return Ok(ExitCode::from(2));
    }
    let host = rustix::system::uname();
    let result = (|| -> tooling::Result<()> {
        let context = plan::Context::new(
            root,
            tooling::inherited_env(),
            host.sysname().to_str().unwrap_or(""),
        )?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| files::failure("Rust audit runtime unavailable."))?;
        runtime.block_on(async {
            let mut runner = Native::new(&context.root, &context.environment, Signals::new()?);
            execute::execute(&context, &mut runner).await
        })
    })();
    match result {
        Ok(()) => Ok(ExitCode::SUCCESS),
        Err(error) => {
            eprintln!("Rust dependency audit failed.");
            Ok(ExitCode::from(error.code))
        }
    }
}
