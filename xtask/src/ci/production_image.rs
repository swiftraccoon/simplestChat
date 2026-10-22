//! Explicit hosted/disposable CI image build and immutable content verification.
//! Imports never contact an engine; only the selected build/verify plan executes.

mod execute;
mod files;
mod options;
mod plan;
mod process;

#[cfg(test)]
mod tests;

use crate::tooling;
use std::path::Path;
use std::process::ExitCode;

const USAGE: &str = "usage: cargo run --locked -q -p xtask -- ci production-image build|verify";

pub(super) fn run(root: &Path, arguments: &[String]) -> anyhow::Result<ExitCode> {
    if arguments.len() == 1 && matches!(arguments[0].as_str(), "--help" | "-h") {
        println!("{USAGE}");
        return Ok(ExitCode::SUCCESS);
    }
    if arguments.len() != 1 || !matches!(arguments[0].as_str(), "build" | "verify") {
        eprintln!("{USAGE}");
        return Ok(ExitCode::from(2));
    }
    let host = rustix::system::uname();
    let result = (|| -> tooling::Result<()> {
        let context = options::Context::new(
            root,
            tooling::inherited_env(),
            host.sysname().to_str().unwrap_or(""),
            host.machine().to_str().unwrap_or(""),
            arguments[0] == "verify",
        )?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| files::failure("Production image runtime unavailable."))?;
        runtime.block_on(async {
            let signals = crate::ci::browser_suite::process::Signals::new()?;
            let mut runner = process::Native::new(&context, signals);
            if arguments[0] == "build" {
                execute::build(&context, &mut runner).await
            } else {
                execute::verify(&context, &mut runner).await
            }
        })
    })();
    match result {
        Ok(()) => Ok(ExitCode::SUCCESS),
        Err(error) => {
            eprintln!("Production image rejected: {}", error.message);
            Ok(ExitCode::from(error.code))
        }
    }
}
