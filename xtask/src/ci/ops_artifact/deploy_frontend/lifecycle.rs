//! Original signal registration covers publication, runtime and umask restoration.
//!
//! Evidence is a pre-close observation. Successful process completion additionally
//! requires retired borrowers, checked unregister and the original final fence.
//! Published JSON precedes those final fences; only actual exit zero is success.

use super::{failure, keep, options::Options, plan::Plan};
use crate::tooling::{Environment, Result};
use simplestchat_ops::owned_process::CompletionSignals;
use std::{ffi::OsStr, path::Path, sync::Arc, time::Instant};
use tokio::runtime::Runtime;

pub(super) struct Completed {
    pub summary: serde_json::Value,
    pub end: Instant,
    pub status: u8,
}
fn scope(
    started: Instant,
    wait: u64,
    owner: Arc<CompletionSignals>,
    operation: impl FnOnce(&Runtime, Arc<CompletionSignals>, &Plan) -> Result<Completed>,
    after_restore: impl FnOnce() -> Result<()>,
) -> Result<Completed> {
    let plan = Plan::native(started, wait);
    let private = nix::sys::stat::Mode::from_bits_truncate(0o077);
    let previous = nix::sys::stat::umask(private);
    let mut completed = None;
    let mut primary = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => {
            let result = operation(&runtime, owner.clone(), &plan);
            drop(runtime);
            match result {
                Ok(result) => {
                    let failure = (result.status != 0).then(|| failure(result.status));
                    completed = Some(result);
                    failure
                }
                Err(error) => Some(error),
            }
        }
        Err(_) => Some(failure(1)),
    };
    if nix::sys::stat::umask(previous) != private {
        keep(&mut primary, failure(1));
    }
    if let Err(error) = after_restore() {
        keep(&mut primary, error);
    }
    if let Some(code) = owner.status() {
        keep(&mut primary, failure(code));
    }
    match Arc::try_unwrap(owner) {
        Ok(mut owner) => {
            match owner.finish() {
                Ok(Some(code)) => keep(&mut primary, failure(code)),
                Ok(None) => {}
                Err(_) => keep(&mut primary, failure(1)),
            }
            if let Some(code) = owner.status() {
                keep(&mut primary, failure(code));
            }
        }
        Err(_borrowed) => keep(&mut primary, failure(1)),
    }
    let now = Instant::now();
    let end = completed.as_ref().map_or(plan.end(), |v| v.end);
    if now < started {
        keep(&mut primary, failure(1));
    } else if now >= end {
        keep(&mut primary, failure(124));
    }
    let mut completed = match completed {
        Some(completed) => completed,
        None => return Err(primary.unwrap_or_else(|| failure(1))),
    };
    completed.status = primary.as_ref().map_or(0, |error| error.code);
    completed.summary["passed"] = serde_json::json!(primary.is_none());
    if let Some(error) = primary
        && completed
            .summary
            .get("failureClass")
            .is_none_or(serde_json::Value::is_null)
    {
        completed.summary["failureClass"] = serde_json::json!(error.message);
    }
    Ok(completed)
}

pub(super) fn run(root: &Path, options: Options) -> Result<Completed> {
    let started = Instant::now();
    let owner = Arc::new(CompletionSignals::new().map_err(|_| failure(1))?);
    scope(
        started,
        options.wait_seconds,
        owner,
        |runtime, flags, plan| {
            let mut environment: Environment = crate::tooling::inherited_env();
            if !environment.contains_key(OsStr::new("GH_TOKEN"))
                && let Some(token) = environment.get(OsStr::new("GITHUB_TOKEN")).cloned()
            {
                environment.insert("GH_TOKEN".into(), token);
            }
            let current = std::env::current_exe().map_err(|_| failure(1))?;
            runtime.block_on(super::execute::native(
                root.into(),
                Arc::new(options),
                environment,
                current,
                flags,
                plan,
            ))
        },
        || Ok(()),
    )
}

#[cfg(test)]
#[path = "publish_tests.rs"]
mod publish_tests;

#[cfg(test)]
mod tests {
    use super::super::plan;
    use super::*;
    const CHILD: &str = "SIMPLESTCHAT_FRONTEND_COMPLETION_CHILD";
    #[test]
    fn isolated_original_owner_fences_late_signal_borrowers_first_cause_and_restore() {
        if let Ok(case) = std::env::var(CHILD) {
            let prior = nix::sys::stat::umask(nix::sys::stat::Mode::from_bits_truncate(0o027));
            let start = Instant::now();
            let mut borrower = None;
            let owner = Arc::new(CompletionSignals::new().unwrap());
            let result = scope(
                start,
                0,
                owner,
                |_, flags, _| {
                    if case == "borrower" {
                        borrower = Some(flags);
                    }
                    if case == "first" {
                        return Err(failure(23));
                    }
                    Ok(Completed {
                        summary: serde_json::json!({"candidatePassed":true}),
                        status: 0,
                        end: if case == "expired" {
                            start
                        } else {
                            start + plan::FINAL
                        },
                    })
                },
                || {
                    let observed = nix::sys::stat::umask(nix::sys::stat::Mode::empty());
                    let _ = nix::sys::stat::umask(observed);
                    assert_eq!(observed, nix::sys::stat::Mode::from_bits_truncate(0o027));
                    if matches!(case.as_str(), "int" | "term" | "first") {
                        nix::sys::signal::raise(if case == "term" {
                            nix::sys::signal::Signal::SIGTERM
                        } else {
                            nix::sys::signal::Signal::SIGINT
                        })
                        .unwrap();
                    }
                    Ok(())
                },
            );
            let _ = nix::sys::stat::umask(prior);
            drop(borrower);
            let expected = match case.as_str() {
                "int" => Some(130),
                "term" => Some(143),
                "first" => Some(23),
                "borrower" => Some(1),
                "expired" => Some(124),
                "success" => None,
                _ => panic!("case"),
            };
            let status = result
                .map(|completed| completed.status)
                .unwrap_or_else(|error| error.code);
            assert_eq!(Some(status).filter(|code| *code != 0), expected);
            return;
        }
        let executable = std::env::current_exe().unwrap();
        for case in ["success", "int", "term", "first", "borrower", "expired"] {
            let output = std::process::Command::new(&executable)
                .args([
                    "--exact",
                    "ci::ops_artifact::deploy_frontend::lifecycle::tests::\
                isolated_original_owner_fences_late_signal_borrowers_first_cause_and_restore",
                    "--nocapture",
                ])
                .env(CHILD, case)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{case}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}
