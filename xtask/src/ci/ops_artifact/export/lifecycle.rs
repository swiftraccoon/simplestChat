//! The original synchronous registration survives runtime and umask teardown.
//! Observer Arcs grant no registration or completion authority. Positive return
//! requires every observer gone, checked unregister, and the original final fence.

use super::{Result, failure, keep, plan};
use simplestchat_ops::owned_process::CompletionSignals;
use std::{path::Path, sync::Arc, time::Instant};

fn complete(owner: Arc<CompletionSignals>, started: Instant, result: Result<()>) -> Result<()> {
    let mut primary = result.err();
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
    if now < started {
        keep(&mut primary, failure(1));
    } else if now >= started + plan::ENTRY {
        keep(&mut primary, failure(124));
    }
    primary.map_or(Ok(()), Err)
}

/// The final callback is a private lifecycle seam; production supplies no work.
/// Isolated tests inject late signals only after this exact runtime/mask boundary.
fn runtime_scope(
    started: Instant,
    owner: Arc<CompletionSignals>,
    operation: impl FnOnce(&tokio::runtime::Runtime, Arc<CompletionSignals>) -> Result<()>,
    after_restore: impl FnOnce() -> Result<()>,
) -> Result<()> {
    let private = nix::sys::stat::Mode::from_bits_truncate(0o077);
    let previous = nix::sys::stat::umask(private);
    let mut primary = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => {
            let result = operation(&runtime, owner.clone());
            drop(runtime);
            result.err()
        }
        Err(_) => Some(failure(1)),
    };
    if nix::sys::stat::umask(previous) != private {
        keep(&mut primary, failure(1));
    }
    if let Err(error) = after_restore() {
        keep(&mut primary, error);
    }
    complete(owner, started, primary.map_or(Ok(()), Err))
}

pub(super) fn run(started: Instant, path: &Path, selected: &str) -> Result<()> {
    let owner = Arc::new(CompletionSignals::new().map_err(|_| failure(1))?);
    runtime_scope(
        started,
        owner,
        |runtime, flags| runtime.block_on(super::execute::native(path, selected, flags, started)),
        || Ok(()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ci::process::{Executor, Native},
        owned_process::Signals,
        tooling::Environment,
    };
    use std::process::Command;

    const CHILD: &str = "SIMPLESTCHAT_EXPORT_LIFECYCLE_CHILD";
    #[test]
    fn isolated_completion_owner_covers_late_int_term_borrowers_first_cause_and_deadline() {
        if let Ok(case) = std::env::var(CHILD) {
            let parent_mask =
                nix::sys::stat::umask(nix::sys::stat::Mode::from_bits_truncate(0o027));
            let started = if case == "deadline" {
                Instant::now() - plan::ENTRY
            } else {
                Instant::now()
            };
            let owner = Arc::new(CompletionSignals::new().unwrap());
            let mut borrower = None;
            let result = runtime_scope(
                started,
                owner,
                |runtime, flags| {
                    let mut signals = Signals::from_completion(flags);
                    if case == "borrower" {
                        borrower = Some(signals);
                        return Ok(());
                    }
                    if matches!(case.as_str(), "observe-int" | "observe-term") {
                        let term = case == "observe-term";
                        let code = runtime.block_on(async {
                            assert_eq!(signals.pending().await, None);
                            let raised = tokio::spawn(async move {
                                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                                nix::sys::signal::raise(if term {
                                    nix::sys::signal::Signal::SIGTERM
                                } else {
                                    nix::sys::signal::Signal::SIGINT
                                })
                                .unwrap();
                            });
                            let code = signals.next().await;
                            raised.await.unwrap();
                            assert_eq!(signals.pending().await, Some(code));
                            code
                        });
                        drop(signals);
                        return Err(failure(code));
                    }
                    let environment = Environment::new();
                    let root = std::env::current_dir().unwrap();
                    let mut executor = Native::new(&root, &environment, signals);
                    runtime.block_on(async {
                        executor.ready().await?;
                        if !executor.cleanup_passed() {
                            return Err(failure(1));
                        }
                        Ok(())
                    })?;
                    drop(executor);
                    if case == "first" {
                        Err(failure(23))
                    } else {
                        Ok(())
                    }
                },
                || {
                    // This callback runs after runtime drop and the actual umask restore.
                    let mask = nix::sys::stat::umask(nix::sys::stat::Mode::empty());
                    let _ = nix::sys::stat::umask(mask);
                    assert_eq!(mask, nix::sys::stat::Mode::from_bits_truncate(0o027));
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
            drop(borrower);
            let restored = nix::sys::stat::umask(parent_mask);
            assert_eq!(restored, nix::sys::stat::Mode::from_bits_truncate(0o027));
            match case.as_str() {
                "success" => assert!(result.is_ok()),
                _ => assert_eq!(
                    result.unwrap_err().code,
                    match case.as_str() {
                        "int" | "observe-int" => 130,
                        "term" | "observe-term" => 143,
                        "first" => 23,
                        "deadline" => 124,
                        "borrower" => 1,
                        _ => panic!("case"),
                    }
                ),
            }
            return;
        }
        let name = concat!(
            module_path!(),
            "::isolated_completion_owner_covers_late_int_term_borrowers_first_cause_and_deadline"
        );
        let name = name
            .strip_prefix(concat!(env!("CARGO_CRATE_NAME"), "::"))
            .unwrap_or(name);
        for case in [
            "success",
            "int",
            "term",
            "borrower",
            "first",
            "deadline",
            "observe-int",
            "observe-term",
        ] {
            let output = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", name])
                .env(CHILD, case)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{case}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(
                String::from_utf8_lossy(&output.stdout).contains("1 passed; 0 failed"),
                "the exact child lifecycle test must execute: {case}"
            );
        }
    }
}
