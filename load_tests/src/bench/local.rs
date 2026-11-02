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
use anyhow::Context as _;
use std::ffi::OsString;
use std::path::Path;
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
        if let Err(error) = lend_bundle(&options.candidate_root, &options.baseline_root) {
            eprintln!("benchmark local: {error:#}");
            return Err(error);
        }
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

/// The server refuses to start without `web/dist/index.html`. A baseline root
/// without one (the comparison's fresh worktree never builds it) receives, and
/// keeps, a copy of the candidate's bundle; the workload drives WebSockets and
/// media and never requests a page. Nothing is copied when the baseline has a
/// bundle or the candidate has none. The baseline must already be a checkout
/// with a `web` directory, and the copy creates `web/dist` and every directory
/// below it, so it never creates a root or merges into a partial bundle.
fn lend_bundle(candidate_root: &Path, baseline_root: &Path) -> anyhow::Result<()> {
    let page = Path::new("web/dist/index.html");
    if baseline_root.join(page).is_file() || !candidate_root.join(page).is_file() {
        return Ok(());
    }
    anyhow::ensure!(
        baseline_root.join("web").is_dir(),
        "baseline root {} has no web directory to receive the candidate's bundle",
        baseline_root.display()
    );
    let target = baseline_root.join("web/dist");
    match std::fs::create_dir(&target) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => anyhow::bail!(
            "{} exists without index.html; remove it or build the baseline's bundle",
            target.display()
        ),
        Err(error) => {
            return Err(error).with_context(|| format!("creating {}", target.display()));
        }
    }
    copy_tree(&candidate_root.join("web/dist"), &target)
        .context("copying the candidate's web/dist into the baseline root")
}

/// Copies the directories and regular files below `source` into the existing
/// `target`, creating each subdirectory with `create_dir` so nothing is merged;
/// links and other entries are skipped.
fn copy_tree(source: &Path, target: &Path) -> anyhow::Result<()> {
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let destination = target.join(entry.file_name());
        if kind.is_dir() {
            std::fs::create_dir(&destination)?;
            copy_tree(&entry.path(), &destination)?;
        } else if kind.is_file() {
            std::fs::copy(entry.path(), &destination)?;
        }
    }
    Ok(())
}
