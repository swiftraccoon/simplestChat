//! Publish Python's database-byte cache selectors, never a scan verdict.

mod execute;
mod plan;
#[cfg(test)]
mod tests;

use crate::{
    ci::{browser_suite::process::Signals, process::Native},
    tooling::{self, Failure, Result},
};
use std::{path::Path, process::ExitCode};

const USAGE: &str = "usage: cargo run --locked -q -p xtask -- ci image-cache \
hint|status preparation|status production";

/// Only the framing validator can construct bytes accepted by the output writer.
pub(super) struct Selection(Vec<u8>);
impl Selection {
    pub(super) fn record(&self) -> &[u8] {
        &self.0
    }
}
fn failure(message: &str) -> Failure {
    Failure::new(1, message)
}

pub(super) fn run(root: &Path, arguments: &[String]) -> anyhow::Result<ExitCode> {
    if arguments.len() == 1 && matches!(arguments[0].as_str(), "--help" | "-h") {
        println!("{USAGE}");
        return Ok(ExitCode::SUCCESS);
    }
    let operation = match plan::Operation::parse(arguments) {
        Ok(operation) => operation,
        Err(_) => {
            eprintln!("{USAGE}");
            return Ok(ExitCode::from(2));
        }
    };
    let host = rustix::system::uname();
    let result = (|| -> Result<()> {
        let context = plan::Context::new(
            root,
            tooling::inherited_env(),
            host.sysname().to_str().unwrap_or(""),
        )?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| failure("Image cache runtime unavailable."))?;
        runtime.block_on(async {
            let mut runner = Native::new(&context.root, &context.environment, Signals::new()?);
            execute::execute(&context, operation, &mut runner, &mut std::io::stderr()).await
        })
    })();
    match result {
        Ok(()) => Ok(ExitCode::SUCCESS),
        Err(error) => {
            eprintln!("Image database cache output failed.");
            Ok(ExitCode::from(error.code))
        }
    }
}
