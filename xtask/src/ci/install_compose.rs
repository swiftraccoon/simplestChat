//! Install only the authenticated Compose policy binary in explicit Linux CI.

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
const USAGE: &str = "usage: cargo run --locked -q -p xtask -- ci install-compose";

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
            host.machine().to_str().unwrap_or(""),
        )?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| files::failure("Compose runtime unavailable."))?;
        runtime.block_on(async {
            let signals = Signals::new()?;
            let mut runner = Native::new(&context.root, &context.environment, signals);
            execute::install(&context, &mut runner).await
        })
    })();
    match result {
        Ok(()) => Ok(ExitCode::SUCCESS),
        Err(error) => {
            // The fixed per-stage private report contains no paths or child output.
            eprintln!("Pinned Compose installation failed.");
            Ok(ExitCode::from(error.code))
        }
    }
}
