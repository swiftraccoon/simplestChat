//! Explicit fixed hosted operating point with one owner for both CLI groups.
//! Importing this module performs no I/O or runtime/service initialization.

mod config;
mod controller;
mod database;
pub(super) mod files;
mod http;
mod measure;
mod policy;
mod report;
mod sampling;

#[cfg(test)]
mod files_tests;
#[cfg(test)]
mod oracle_tests;
#[cfg(test)]
mod tests;

use crate::bench::{clock::RealClock, process::NativeHost, sampling::NativePorts};
use anyhow::Result;
use std::{ffi::OsString, process::ExitCode, sync::Arc};

pub(super) fn run(arguments: &[OsString]) -> ExitCode {
    if arguments.len() == 1 && matches!(arguments[0].to_str(), Some("--help" | "-h")) {
        println!(
            "load_test bench account-room (fixed hosted-only profile; explicit environment opt-ins)"
        );
        return ExitCode::SUCCESS;
    }
    if !arguments.is_empty() {
        return ExitCode::from(2);
    }
    let result = native();
    match result {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!(
                "Account-room measurement failed; inspect the bounded private report artifacts."
            );
            ExitCode::from(controller::status(&error))
        }
    }
}

fn native() -> Result<u8> {
    let environment = std::env::vars_os().collect();
    let directory = std::env::current_dir()?;
    let host = Arc::new(NativeHost::default());
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        // Register before any selected input/file admission, and retain this
        // owner through all final namespace, report-close and clock fences.
        use tokio::signal::unix::{SignalKind, signal};
        let mut interrupt = signal(SignalKind::interrupt())?;
        let mut terminate = signal(SignalKind::terminate())?;
        let clock = RealClock::default();
        let result = async {
            let config = config::Config::parse(environment, directory, std::env::consts::OS)?;
            let http = http::NativeHttp::new(&config)?;
            let observations = sampling::NativeObservations::new(&config, host.as_ref(), &clock);
            let services = controller::Services {
                host: host.as_ref(),
                clock: &clock,
                http: &http,
                observations: &observations,
                ports: &NativePorts,
            };
            let outcome = controller::execute(&config, &services).await?;
            let code = if outcome.code == 0 && outcome.report["passed"] != true {
                1
            } else {
                outcome.code
            };
            Ok::<_, anyhow::Error>(code)
        };
        tokio::pin!(result);
        let result = loop {
            tokio::select! { biased;
                _ = interrupt.recv() => host.cancel(130),
                _ = terminate.recv() => host.cancel(143),
                result = &mut result => break result,
            }
        };
        tokio::select! { biased;
            _ = interrupt.recv() => host.cancel(130),
            _ = terminate.recv() => host.cancel(143),
            () = std::future::ready(()) => {},
        }
        let mut code = result?;
        if code == 0 && host.code() != 0 {
            code = host.code();
        }
        Ok(code)
    })
}
