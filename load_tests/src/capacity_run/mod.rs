//! Explicitly requested, isolated capacity measurement with owned cleanup.
//!
//! Merely importing this module starts no runtime, engine or workload. Engine
//! calls use bounded structured argv and retain their process-group identity
//! until cleanup; every container creation is journaled before it is attempted.

pub(crate) mod artifacts;
#[cfg(test)]
mod boundary_tests;
mod clock;
mod commands;
mod conclude;
mod context;
mod controller;
mod engine;
pub(crate) mod files;
#[cfg(test)]
mod fixtures;
mod host;
mod json;
mod judge;
mod options;
mod ownership;
mod parsers;
#[cfg(test)]
mod policy_tests;
#[cfg(test)]
mod process_tests;
mod report;
#[cfg(test)]
mod report_tests;
mod search;
mod step;
mod types;
mod watch;

use crate::owned_process as process;
use crate::owned_process::io as process_io;
use anyhow::Result;
use std::ffi::OsString;
use std::process::ExitCode;

async fn measured(options: &options::Options, cancellation: &process::Cancellation) -> Result<()> {
    use tokio::signal::unix::{SignalKind, signal};
    let mut interrupt = signal(SignalKind::interrupt())?;
    let mut terminate = signal(SignalKind::terminate())?;
    let owned = cancellation.clone();
    let signals = tokio::spawn(async move {
        tokio::select! {
            _ = interrupt.recv() => owned.cancel(130),
            _ = terminate.recv() => owned.cancel(143),
        }
    });
    let result = async {
        let runner = process::Runner {
            directory: std::env::current_dir()?,
            environment: std::env::vars_os().collect(),
            cancellation: cancellation.clone(),
        };
        let engine = engine::NativeEngine::discover(options.engine.as_deref(), runner)?;
        let clock = clock::RealClock::new();
        let output = controller::output(options, &clock);
        controller::run(options, &engine, &clock, cancellation, &output).await
    }
    .await;
    signals.abort();
    let _ = signals.await;
    result
}

pub(crate) fn run(arguments: &[OsString]) -> ExitCode {
    if arguments
        .iter()
        .any(|value| value == "--help" || value == "-h")
    {
        println!("{}", options::USAGE);
        return ExitCode::SUCCESS;
    }
    let options = match options::parse(arguments) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("capacity: {error}\n{}", options::USAGE);
            return ExitCode::from(2);
        }
    };
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("capacity: {error}");
            return ExitCode::FAILURE;
        }
    };
    let cancellation = process::Cancellation::default();
    let result = runtime.block_on(measured(&options, &cancellation));
    if let Err(error) = &result {
        eprintln!("capacity: {error}");
    }
    let code = result
        .as_ref()
        .err()
        .and_then(|error| error.downcast_ref::<process::Interrupted>())
        .map_or(cancellation.code(), |error| error.0);
    if code != 0 {
        ExitCode::from(code)
    } else if result.is_ok() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
