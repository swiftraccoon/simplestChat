//! Production-shaped container comparison policy; imports perform no operations.

#[path = "podman/artifacts.rs"]
mod artifacts;
#[path = "podman/attribution.rs"]
mod attribution;
#[path = "podman/commands.rs"]
mod commands;
#[path = "podman/controller.rs"]
mod controller;
#[path = "podman/engine.rs"]
mod engine;
#[path = "podman/finalize.rs"]
mod finalize;
#[path = "podman/netem.rs"]
mod netem;
#[path = "podman/options.rs"]
mod options;
#[path = "podman/ownership.rs"]
mod ownership;
#[path = "podman/parsers.rs"]
mod parsers;
#[path = "podman/report.rs"]
mod report;
#[path = "podman/sampler.rs"]
mod sampler;
#[path = "podman/step.rs"]
mod step;

#[cfg(test)]
#[path = "podman/tests.rs"]
mod tests;

use crate::bench::{clock::RealClock, metrics::HttpMetrics, process::NativeHost};
use std::ffi::OsString;
use std::process::ExitCode;
use std::sync::Arc;

pub(super) fn run(arguments: &[OsString]) -> ExitCode {
    if arguments
        .iter()
        .any(|value| value == "--help" || value == "-h")
    {
        println!(concat!(
            "load_test bench podman --server-image IMAGE ",
            "--generator-image IMAGE --output DIR [OPTIONS]"
        ));
        return ExitCode::SUCCESS;
    }
    let result = (|| -> anyhow::Result<_> {
        let directory = std::env::current_dir()?;
        let options = options::parse(arguments, &directory)?;
        let host = Arc::new(NativeHost::default());
        let engine = engine::Engine::new(
            host.clone(),
            Arc::new(RealClock::default()),
            directory,
            std::env::vars_os().collect(),
        )?;
        let metrics = HttpMetrics::new()?;
        let executable = std::env::current_exe()?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?;
        runtime.block_on(async {
            use tokio::signal::unix::{SignalKind, signal};
            let mut interrupt = signal(SignalKind::interrupt())?;
            let mut terminate = signal(SignalKind::terminate())?;
            let owned = host.clone();
            let signals = tokio::spawn(async move {
                tokio::select! {
                    _=interrupt.recv()=>{owned.cancel(130);130u8},
                    _=terminate.recv()=>{owned.cancel(143);143u8},
                }
            });
            let result = controller::execute(&options, &engine, &metrics, &executable).await;
            let code = host.code();
            signals.abort();
            let _ = signals.await;
            Ok::<_, anyhow::Error>((result, code))
        })
    })();
    match result {
        Ok((result, code)) => {
            if let Err(error) = &result {
                eprintln!("benchmark podman: {error:#}");
            }
            if code != 0 {
                ExitCode::from(code)
            } else if result.as_ref().is_ok_and(|report| report["passed"] == true) {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Err(error) => {
            eprintln!("benchmark podman: {error:#}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
#[path = "podman/runtime_tests.rs"]
mod runtime_tests;
