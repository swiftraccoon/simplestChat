//! Bounded transfers from already stopped, inspected immutable containers.
//!
//! The process owner closes both output sinks before archive validation. Fixed
//! names alone select host outputs; a failed copy or validation retains the
//! private archive and stderr, with no successful collection manifest.

use super::engine::{Engine, MIB, valid_id};
use crate::bench::{artifacts::private_file, process::Sink};
use crate::capacity_run::artifacts::{finish_benchmark_results, finish_handshake_capture};
use anyhow::{Result, ensure};
use serde_json::Value;
use std::path::Path;
use std::time::Duration;

pub(super) async fn collect_generator(
    engine: &Engine,
    id: &str,
    directory: &Path,
) -> Result<Value> {
    transfer(engine, id, directory, false).await
}

pub(super) async fn collect_capture(engine: &Engine, id: &str, directory: &Path) -> Result<Value> {
    transfer(engine, id, directory, true).await
}

async fn transfer(engine: &Engine, id: &str, directory: &Path, capture: bool) -> Result<Value> {
    ensure!(
        valid_id(id),
        "Artifact source must be a verified immutable container"
    );
    let (source, archive, errors, maximum) = if capture {
        (
            "/handshakes.pcap",
            "handshakes.tar",
            "handshakes-cp.stderr",
            65 * MIB,
        )
    } else {
        (
            "/results/.",
            "generator-results.tar",
            "generator-results.stderr",
            144 * MIB,
        )
    };
    let archive = directory.join(archive);
    let output = engine
        .call(
            &["cp".into(), format!("{id}:{source}"), "-".into()],
            Duration::from_secs(60),
            maximum,
            Sink::File(private_file(&archive)?),
            Sink::File(private_file(&directory.join(errors))?),
            true,
        )
        .await?;
    ensure!(
        output.exit.code == Some(0) && output.exit.signal.is_none(),
        "Stopped container artifact transfer failed"
    );
    if capture {
        finish_handshake_capture(&archive, directory)
    } else {
        finish_benchmark_results(&archive, directory)
    }
}

#[cfg(test)]
#[path = "artifacts_tests.rs"]
mod tests;
