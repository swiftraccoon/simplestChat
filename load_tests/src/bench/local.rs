//! Sequential A/B measurements of explicitly owned loopback processes. The
//! service boundary lets fixtures prove lifecycle behavior without a workload.

mod controller;
mod finalize;
mod receipt;
mod step;

#[cfg(test)]
mod tests;

pub use controller::{Manifest, execute, manifest};
pub use step::{Context, Services, run_one};

use super::{
    clock::RealClock,
    media_sampler::HttpMedia,
    metrics::HttpMetrics,
    process::NativeHost,
    reports::NativeReports,
    sampling::{NativePorts, NativeResources},
};
use std::ffi::OsString;
use std::process::ExitCode;
use std::sync::Arc;

pub(super) fn run(arguments: &[OsString]) -> ExitCode {
    if arguments.len() == 1 && (arguments[0] == "--help" || arguments[0] == "-h") {
        println!(
            "load_test bench local --baseline-root DIR --baseline-bin BIN \
--candidate-root DIR --candidate-bin BIN --generator BIN --output DIR [OPTIONS]"
        );
        return ExitCode::SUCCESS;
    }
    let result = (|| -> anyhow::Result<(bool, u8)> {
        let directory = std::env::current_dir()?;
        let options = super::options::parse(arguments, &directory)?;
        let environment = std::env::vars_os().collect();
        let run_id = receipt::run_id(&environment)?;
        let host = Arc::new(NativeHost::default());
        let observed = receipt::ObservedHost::new(host.as_ref());
        let selected_host: &dyn super::process::Host = if run_id.is_some() {
            &observed
        } else {
            host.as_ref()
        };
        let clock = RealClock::default();
        let metrics = HttpMetrics::new()?;
        let media = HttpMedia::new()?;
        let resources = NativeResources::new(selected_host, directory.clone(), &environment);
        let generator_sha256 = super::provenance::hash_file(&options.generator)?.0;
        let reports = NativeReports {
            host: selected_host,
            generator: options.generator.clone(),
            generator_sha256,
            directory: directory.clone(),
            environment: super::commands::clean(&environment),
        };
        let services = Services {
            host: selected_host,
            clock: &clock,
            metrics: &metrics,
            resources: &resources,
            ports: &NativePorts,
            media: &media,
            reports: &reports,
            environment: &environment,
        };
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?;
        let executable = std::env::current_exe()?;
        runtime.block_on(async {
            if let Some(run_id) = run_id {
                return receipt::execute(
                    &run_id,
                    &options,
                    &services,
                    &executable,
                    &host,
                    &observed,
                )
                .await;
            }
            use tokio::signal::unix::{SignalKind, signal};
            let mut interrupt = signal(SignalKind::interrupt())?;
            let mut terminate = signal(SignalKind::terminate())?;
            let signal_owner = host.clone();
            let signals = tokio::spawn(async move {
                tokio::select! {
                    _ = interrupt.recv() => signal_owner.cancel(130),
                    _ = terminate.recv() => signal_owner.cancel(143),
                }
            });
            let outcome = execute(&options, &services, &executable).await;
            signals.abort();
            let _ = signals.await;
            let code = host.code();
            if code != 0 {
                return Ok((false, code));
            }
            Ok((outcome?["passed"] == true, 0))
        })
    })();
    match result {
        Ok((_, code)) if code != 0 => ExitCode::from(code),
        Ok((true, _)) => ExitCode::SUCCESS,
        _ => {
            eprintln!("Local benchmark failed; inspect the private output artifacts.");
            ExitCode::FAILURE
        }
    }
}
