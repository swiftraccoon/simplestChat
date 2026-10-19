//! Repository automation: `cargo run -q -p xtask -- <command>`.
//!
//! Every command runs from the repository root, depends only on `anyhow` and
//! `syn` (to read Rust sources), and exits non-zero on the first failure so CI
//! and local runs behave identically.

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

mod line_length;
mod protocol_ts;

const USAGE: &str = "\
Usage: cargo run -q -p xtask -- <command>

Commands:
  line-length   Fail when Rust production code has more lines over 100 columns
                than the ratchet in xtask/line-length-allowance.txt permits;
                `--update` lowers the allowance to the current count.
  protocol-ts   Write web/src/protocol.generated.ts from the Rust wire types;
                `--check` fails when the committed file is stale.
";

fn repository_root() -> Result<PathBuf> {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .map(Path::to_path_buf)
        .context("xtask lives one level below the repository root")
}

fn run(arguments: &[String]) -> Result<()> {
    let Some((command, rest)) = arguments.split_first() else {
        bail!("{USAGE}");
    };
    let root = repository_root()?;
    match command.as_str() {
        "line-length" => line_length::run(&root, rest),
        "protocol-ts" => protocol_ts::run(&root, rest),
        "--help" | "-h" | "help" => {
            print!("{USAGE}");
            Ok(())
        }
        other => bail!("Unknown command `{other}`\n\n{USAGE}"),
    }
}

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    match run(&arguments) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("xtask: {error:#}");
            ExitCode::FAILURE
        }
    }
}
