//! Repository automation: `cargo run -q -p xtask -- <command>`.
//!
//! Every command runs from the repository root and exits non-zero on the first
//! failure so CI and local runs behave identically; the test harness commands
//! exit with the status the shell helpers they replaced reported.

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

mod http_policy;
mod line_length;
mod media_ip;
mod protocol_ts;
mod test_env;

const USAGE: &str = "\
Usage: cargo run -q -p xtask -- <command>

Commands:
  line-length   Fail when Rust production code has more lines over 100 columns
                than the ratchet in xtask/line-length-allowance.txt permits;
                `--update` lowers the allowance to the current count.
  protocol-ts   Write web/src/protocol.generated.ts from the Rust wire types and limits;
                `--check` fails when the committed file is stale.  http-policy   Write security/http-policy.json from the server's security headers
                and content security policy; `--check` fails when it is stale.
  media-ip      Print the owned IPv4 address of the default route's interface for a
                non-loopback media announcement (TEST_ANNOUNCE_IP).
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
        "line-length" => line_length::run(&root, rest).map(|()| ExitCode::SUCCESS),
        "protocol-ts" => protocol_ts::run(&root, rest).map(|()| ExitCode::SUCCESS),
        "http-policy" => http_policy::run(&root, rest).map(|()| ExitCode::SUCCESS),
        "media-ip" => media_ip::run(&root, rest).map(|()| ExitCode::SUCCESS),
        "test-postgres" => test_env::run_postgres(&root, rest),
        "test-server" => test_env::run_server(&root, rest),
        "test-env" => test_env::run_env(&root, rest),
        "--help" | "-h" | "help" => {
            print!("{USAGE}");
            Ok(ExitCode::SUCCESS)
        }
        other => bail!("Unknown command `{other}`\n\n{USAGE}"),
    }
}

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    match run(&arguments) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("xtask: {error:#}");
            ExitCode::FAILURE
        }
    }
}
