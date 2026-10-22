//! Overlapped controller preparation with an explicit detached handoff. The
//! worker owns its CLI groups only; no broker/root/session/engine cleanup claim.

#[path = "release_controller_files.rs"]
pub(super) mod files;
#[path = "release_controller_launcher.rs"]
mod launcher;
#[path = "release_controller_log.rs"]
mod log;
#[path = "release_controller_plan.rs"]
mod plan;
#[path = "release_controller_process.rs"]
mod process;
#[path = "release_controller_protocol.rs"]
mod protocol;
#[path = "release_controller_replay.rs"]
pub(super) mod replay;
#[path = "release_controller_source.rs"]
mod source;
#[cfg(test)]
#[path = "release_controller_tests.rs"]
mod tests;
#[path = "release_controller_wait.rs"]
mod wait;
#[path = "release_controller_worker.rs"]
mod worker;

use crate::tooling;
use std::{path::Path, process::ExitCode};
const USAGE: &str = "usage: cargo run --locked -q -p xtask -- ci release-controller start|wait";

pub(super) fn run(root: &Path, arguments: &[String]) -> anyhow::Result<ExitCode> {
    if arguments.len() == 1 && matches!(arguments[0].as_str(), "--help" | "-h") {
        println!("{USAGE}");
        return Ok(ExitCode::SUCCESS);
    }
    if arguments.len() != 1
        || !matches!(
            arguments[0].as_str(),
            "start" | "wait" | "__worker" | "__replay"
        )
    {
        eprintln!("{USAGE}");
        return Ok(ExitCode::from(2));
    }
    // This fixed byte copier may block on runner stdout. Keep default signals
    // in the isolated child; its parent owns interruption and group retirement.
    let result = if arguments[0] == "__replay" {
        replay::child()
    } else {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| plan::failure())
            .and_then(|runtime| runtime.block_on(execute(root, &arguments[0])))
    };
    match result {
        Ok(()) => Ok(ExitCode::SUCCESS),
        Err(error) => {
            eprintln!("Release controller preparation failed.");
            Ok(ExitCode::from(error.code))
        }
    }
}

async fn execute(root: &Path, operation: &str) -> tooling::Result<()> {
    // Register before source/namespace admission; the collector already has a
    // live worker from an earlier step. One signal owner survives final closes.
    let mut signals = crate::owned_process::Signals::new()?;
    let deadline = std::time::Instant::now() + plan::WAIT;
    let admitted = plan::Context::new(
        root,
        tooling::inherited_env(),
        rustix::system::uname().sysname().to_str().unwrap_or(""),
    );
    let mut context = match admitted {
        Ok(context) => context,
        Err(error) => {
            return Err(if let Some(code) = signals.pending().await {
                tooling::Failure::new(code, "Release controller admission interrupted.")
            } else {
                error
            });
        }
    };
    let result = match operation {
        "start" => launcher::start(&context, &mut signals).await,
        "wait" => wait::wait(&context, &mut signals, deadline).await,
        _ => worker::execute(&context, &mut signals)
            .await
            .and_then(|code| {
                if code == 0 {
                    Ok(())
                } else {
                    Err(tooling::Failure::new(
                        code,
                        "Release controller worker failed.",
                    ))
                }
            }),
    };
    let closed = context.finish();
    let result = result.and(closed);
    let interrupted = signals.pending().await;
    if ((operation == "start" && result.is_err()) || interrupted.is_some())
        && let Ok(owned) = files::Owned::open(&context)
    {
        if let Ok((intent, digest)) = owned.intent(&context) {
            let _ = worker::request(&owned, &intent, &digest, protocol::Action::Cancel);
        }
        let _ = owned.finish();
    }
    completion(
        result,
        interrupted,
        operation == "wait" && std::time::Instant::now() >= deadline,
    )
}

fn completion(
    result: tooling::Result<()>,
    interrupted: Option<u8>,
    expired: bool,
) -> tooling::Result<()> {
    // A late signal is observed/requested above, but cannot erase an existing
    // nonzero primary result. Only an otherwise successful collection changes.
    result?;
    if let Some(code) = interrupted {
        return Err(tooling::Failure::new(
            code,
            "Release controller interrupted.",
        ));
    }
    if expired {
        return Err(tooling::Failure::new(
            124,
            "Release controller collection timed out.",
        ));
    }
    Ok(())
}
