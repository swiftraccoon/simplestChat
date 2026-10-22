//! Explicit generator evidence under a disposable Linux loopback impairment.
//! Imports acquire no process, interface, runtime or network resource.

mod controller;
mod netem;
mod output;
mod plan;
mod state;

#[cfg(test)]
mod oracle_tests;
#[cfg(test)]
mod tests;

use crate::bench::{clock::RealClock, process::NativeHost, sampling::NativePorts};
use std::{ffi::OsString, process::ExitCode};

const USAGE: &str =
    "Usage: load_test bench impaired --output DIR [--server-bin BIN] [--generator BIN]";

pub(super) fn run(arguments: &[OsString]) -> ExitCode {
    if arguments == [OsString::from("--help")] || arguments == [OsString::from("-h")] {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let result = (|| -> anyhow::Result<(u8, serde_json::Value)> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        runtime.block_on(async {
            use tokio::signal::unix::{SignalKind, signal};
            let mut interrupt = signal(SignalKind::interrupt())?;
            let mut terminate = signal(SignalKind::terminate())?;
            let host = NativeHost::default();
            let clock = RealClock::default();
            let root = std::env::current_dir()?;
            let plan = plan::Plan::parse(
                &root,
                arguments,
                std::env::vars_os().collect(),
                std::env::consts::OS,
            )
            .map_err(|_| controller::Failure::new(2))?;
            let services = controller::Services {
                host: &host,
                clock: &clock,
                ports: &NativePorts,
                readiness: &controller::NativeReadiness::new()?,
                authority: &state::NativeAuthority,
            };
            let operation = controller::execute(&plan, &services);
            tokio::pin!(operation);
            let outcome = loop {
                tokio::select! {
                    biased;
                    _ = interrupt.recv() => host.cancel(130),
                    _ = terminate.recv() => host.cancel(143),
                    result = &mut operation => break result,
                }
            };
            let value = outcome?;
            tokio::select! {
                biased;
                _ = interrupt.recv() => host.cancel(130),
                _ = terminate.recv() => host.cancel(143),
                _ = std::future::ready(()) => {},
            }
            Ok((host.code(), value))
        })
    })();
    match result {
        Ok((0, _)) => ExitCode::SUCCESS,
        Ok((code, _)) => ExitCode::from(code),
        Err(error) => ExitCode::from(controller::exit_code(&error)),
    }
}
