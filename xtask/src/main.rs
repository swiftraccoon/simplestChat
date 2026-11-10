//! Repository automation: `cargo run -q -p xtask -- <command>`.
//!
//! The developer gates and the local UI. Hosted CI steps, local CI and cache
//! retention run through `simplestchat-ci`, and the release control plane
//! through `simplestchat-release`.
//!
//! Every command runs from the repository root and exits non-zero on the first
//! failure so CI and local runs behave identically; the test harness commands
//! exit with the status the shell helpers they replaced reported.

use anyhow::{Context, Result, bail};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

mod check;
mod http_policy;
mod line_length;
mod performance_thresholds;
mod protocol_ts;
mod python_lines;
mod run_local;

const USAGE: &str = "\
Usage: cargo run -q -p xtask -- <command>

Commands:
  run           Build and run the local UI; --skip-web reuses web/dist.
  check         Run installed-tool quality gates: [--web|--rust|--python|--helpers].
                Default runs all groups, without installs or application services.
  line-length   Fail when Rust production code has more lines over 100 columns
                than the ratchet in xtask/line-length-allowance.txt permits;
                `--update` lowers the allowance to the current count.
  python-lines  Fail unless xtask/python-allowance.txt lists exactly the tracked *.py/*.pyi
                outside vendor/, no file exceeds its row and the total stays within;
                `--update` only removes rows and lowers counts (edit a renamed file's row).
  protocol-ts   Write web/src/protocol.generated.ts from the Rust wire types and limits;
                `--check` fails when the committed file is stale.
  http-policy   Write security/http-policy.json from the server's security headers
                and content security policy; `--check` fails when it is stale.
  media-ip      Print the owned IPv4 address of the default route's interface for a
                non-loopback media announcement (TEST_ANNOUNCE_IP); --github-env appends
                only that fixed variable to the captured runner environment file.
  performance-thresholds COMPARISON.json
                Summarize a same-host benchmark comparison and enforce its
                readiness/CPU/RSS regression budgets.
  test-postgres COMMAND [ARG ...]
                Run COMMAND against an owned disposable PostgreSQL cluster
                (TEST_POSTGRES_PORT, default 15434) exported as DATABASE_URL.
  test-server COMMAND [ARG ...]
                Run COMMAND against an owned loopback server on DATABASE_URL
                (DISPOSABLE_TEST_DATABASE=1); TEST_SERVER_PORT, TEST_MEDIA_PORT,
                TEST_ANNOUNCE_IP, TURN_E2E, TEST_BROWSER_HTTPS, PASSKEY_E2E as
                docs/testing.md describes.
  test-env COMMAND [ARG ...]
                test-postgres around test-server: the disposable cluster and the
                owned server for one command.

Hosted CI steps, local CI and cache retention:
  cargo run --locked -q -p simplestchat-ci -- --help
Signed deployment and verification, Ops artifacts and private VPS capacity runs:
  cargo run --locked -q -p simplestchat-release -- --help
";

fn repository_root() -> Result<PathBuf> {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .map(Path::to_path_buf)
        .context("xtask lives one level below the repository root")
}

fn run(arguments: &[String]) -> Result<ExitCode> {
    let Some((command, rest)) = arguments.split_first() else {
        bail!("{USAGE}");
    };
    let root = repository_root()?;
    match command.as_str() {
        "run" => run_local::run(&root, rest),
        "check" => check::run(&root, rest),
        "line-length" => line_length::run(&root, rest).map(|()| ExitCode::SUCCESS),
        "python-lines" => python_lines::run(&root, rest).map(|()| ExitCode::SUCCESS),
        "protocol-ts" => protocol_ts::run(&root, rest).map(|()| ExitCode::SUCCESS),
        "http-policy" => http_policy::run(&root, rest).map(|()| ExitCode::SUCCESS),
        "media-ip" => simplestchat_toolkit::media_ip::run(&root, rest),
        "performance-thresholds" => performance_thresholds::run(&root, rest),
        "test-postgres" => simplestchat_toolkit::test_env::run_postgres(&root, rest),
        "test-server" => simplestchat_toolkit::test_env::run_server(&root, rest),
        "test-env" => simplestchat_toolkit::test_env::run_env(&root, rest),
        "--help" | "-h" | "help" => {
            print!("{USAGE}");
            Ok(ExitCode::SUCCESS)
        }
        other => bail!("Unknown command `{other}`\n\n{USAGE}"),
    }
}

fn main() -> ExitCode {
    let arguments: Vec<OsString> = std::env::args_os().skip(1).collect();
    // The runner's reserved replay child, before any other parsing or signal
    // registration: it only copies stdin to stdout.
    if let Some(code) = simplestchat_toolkit::replay::route(&arguments) {
        return code;
    }
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    match run(&arguments) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("xtask: {error:#}");
            ExitCode::FAILURE
        }
    }
}
