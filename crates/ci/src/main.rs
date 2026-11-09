//! `simplestchat-ci`: hosted CI steps, local CI and cache retention, run as
//! `cargo run --locked -q -p simplestchat-ci -- <command>`.
//!
//! Every command runs from the repository root and exits non-zero on the first
//! failure, so CI and local runs behave identically.

use anyhow::{Context, Result};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

fn repository_root() -> Result<PathBuf> {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .context("simplestchat-ci lives two levels below the repository root")
}

fn main() -> ExitCode {
    let arguments: Vec<OsString> = std::env::args_os().skip(1).collect();
    // The runner's reserved replay child, before any other parsing or signal
    // registration: it only copies stdin to stdout.
    if let Some(code) = simplestchat_toolkit::replay::route(&arguments) {
        return code;
    }
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    match repository_root().and_then(|root| simplestchat_ci::run(&root, &arguments)) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("simplestchat-ci: {error:#}");
            ExitCode::FAILURE
        }
    }
}
