//! Explicit native worker regression build in a fresh disposable source copy.

mod execute;
pub(super) mod files;
#[cfg(test)]
mod fixtures;
mod marker;
mod plan;
mod snapshot;
mod source;

use crate::{owned_process::Signals, tooling};
use std::{path::Path, process::ExitCode};
const USAGE: &str = "usage: cargo run --locked -q -p xtask -- ci native-dtls [--record-success]";

pub(super) fn run(root: &Path, arguments: &[String]) -> anyhow::Result<ExitCode> {
    if arguments == ["--help"] {
        println!("{USAGE}");
        return Ok(ExitCode::SUCCESS);
    }
    if !arguments.is_empty() && arguments != ["--record-success"] {
        eprintln!("{USAGE}");
        return Ok(ExitCode::from(2));
    }
    let _mask = Umask(rustix::process::umask(
        rustix::fs::Mode::from_bits_truncate(0o077),
    ));
    let environment = tooling::inherited_env();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let result = runtime.block_on(async {
        let signals = Signals::new().map_err(|_| plan::failure(1))?;
        let context = plan::Context::new(
            root,
            &std::env::current_dir().map_err(|_| plan::failure(2))?,
            environment,
            rustix::system::uname().sysname().to_str().unwrap_or(""),
            !arguments.is_empty(),
        )?;
        execute::run(&context, signals).await
    });
    match result {
        Ok(()) => Ok(ExitCode::SUCCESS),
        Err(error) => {
            eprintln!("{}", error.message);
            Ok(ExitCode::from(error.code))
        }
    }
}

struct Umask(rustix::fs::Mode);
impl Drop for Umask {
    fn drop(&mut self) {
        rustix::process::umask(self.0);
    }
}
