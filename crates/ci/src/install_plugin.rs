//! Install only an authenticated Docker CLI plugin release in explicit Linux CI.
//!
//! Each [`Plugin`] names one pinned release; every plugin shares the same
//! download, verification, privileged installation and version checks.

mod execute;
mod files;
mod plan;
#[cfg(test)]
mod tests;

use crate::{browser_suite::process::Signals, process::Native};
use simplestchat_toolkit::tooling;
use std::{path::Path, process::ExitCode};

/// The Docker CLI plugins CI installs from a pinned release.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Plugin {
    /// Compose, for production configuration rendering and the release fixture.
    Compose,
    /// Buildx, for every image build from the repository's ignore file.
    Buildx,
}
impl Plugin {
    /// Every plugin a subcommand can select.
    const ALL: [Self; 2] = [Self::Compose, Self::Buildx];

    /// The plugin an `install-<name>` subcommand selects, if it names one.
    pub(super) fn from_subcommand(subcommand: &str) -> Option<Self> {
        let name = subcommand.strip_prefix("install-")?;
        Self::ALL
            .into_iter()
            .find(|plugin| plugin.release().name == name)
    }
}

/// Run `install-<plugin>`: help succeeds, any other argument is usage status
/// 2, and installation failures print one fixed line and return their status.
/// Blocks the calling thread until the owned commands and cleanup settle.
pub(super) fn run(root: &Path, plugin: Plugin, arguments: &[String]) -> anyhow::Result<ExitCode> {
    let release = plugin.release();
    let usage = format!(
        "usage: cargo run --locked -q -p simplestchat-ci -- install-{}",
        release.name
    );
    if arguments.len() == 1 && matches!(arguments[0].as_str(), "--help" | "-h") {
        println!("{usage}");
        return Ok(ExitCode::SUCCESS);
    }
    if !arguments.is_empty() {
        eprintln!("{usage}");
        return Ok(ExitCode::from(2));
    }
    let host = rustix::system::uname();
    let result = (|| -> tooling::Result<()> {
        let context = plan::Context::new(
            root,
            tooling::inherited_env(),
            host.sysname().to_str().unwrap_or(""),
            host.machine().to_str().unwrap_or(""),
            plugin,
        )?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| files::failure("Plugin runtime unavailable."))?;
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
            eprintln!("Pinned {} installation failed.", release.title);
            Ok(ExitCode::from(error.code))
        }
    }
}
