//! `simplestchat-release`: the release control plane, run as
//! `cargo run --locked -q -p simplestchat-release -- <command>` or as the
//! source-built controller `ops/ansible/RELEASES.md` documents.
//!
//! Its command families are `deploy` (the signed routine deployment),
//! `verify` (signed application verification, forwarded before any UTF-8
//! parsing), `ops-artifact` (the Ops artifact's production, retention and
//! runtime proofs), `vps-capacity` (the private VPS capacity operation) and
//! `bootstrap` (SSH enrollment and preparation of a private benchmark host).
//! Every command exits non-zero on the first failure. They find their files
//! in different places:
//! - `deploy`, `bootstrap` and `ops-artifact maintain-runtime`,
//!   `maintain-host-runtime` and `deploy-runtime` use [`root`], the checkout
//!   two levels above this crate's manifest;
//! - `ops-artifact verify-retained`, `acquire`, `export` and `runtime`, and
//!   `vps-capacity`, use the caller's current directory;
//! - `ops-artifact hosted` uses the hosted runner's `GITHUB_WORKSPACE`, and
//!   `ops-artifact runtime-request` and `runtime-diagnostics` its
//!   `RUNNER_TEMP`;
//! - `verify`, and `ops-artifact build`, `source`, `registry` and
//!   `signing-inputs`, take every path from their arguments or pinned
//!   request.
//!
//! The hosted CI steps and the bounded command runner these commands drive
//! live in `simplestchat-ci`; the developer gates stay in xtask.

use anyhow::{Context, Result, bail};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

mod bootstrap;
mod ops_artifact;

const USAGE: &str = "\
Usage: cargo run --locked -q -p simplestchat-release -- <command> [ARG ...]

Commands:
  deploy        Select signed application and Ops artifacts, deploy once and verify HTTPS;
                prepared routine host only; see `deploy --help`.
  verify        Check explicit signed application inputs without a host; see `verify --help`.
  ops-artifact  Export source, hydrate registry, build unsigned Ops, or verify retained Ops;
                fixed recipe; maintain-runtime owns the optional signed maintenance KVM proof.
  vps-capacity  Authenticated private VPS measurement, collection or explicit recovery;
                installed service callsites remain unchanged; see --help.
  bootstrap     Enroll SSH access to one private Debian benchmark host and optionally
                prepare it through site.yml; see `bootstrap --help`.
";

/// The repository checkout, two levels above this crate's manifest.
fn root() -> Result<PathBuf> {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .context("simplestchat-release lives two levels below the repository root")
}

fn run_vps_capacity(arguments: &[String]) -> Result<ExitCode> {
    if arguments.len() == 1 && matches!(arguments[0].as_str(), "--help" | "-h") {
        println!("{}", ops_artifact::capacity_vps::USAGE);
        return Ok(ExitCode::SUCCESS);
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    Ok(
        match runtime.block_on(ops_artifact::capacity_vps::native(arguments)) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("{}", error.message);
                ExitCode::from(error.code)
            }
        },
    )
}

fn run(arguments: &[String]) -> Result<ExitCode> {
    let Some((command, rest)) = arguments.split_first() else {
        bail!("{USAGE}");
    };
    match command.as_str() {
        "deploy" => Ok(ops_artifact::run_release_frontend(&root()?, rest)),
        "ops-artifact" => ops_artifact::run(rest),
        "vps-capacity" => run_vps_capacity(rest),
        "bootstrap" => Ok(bootstrap::run(rest, &root()?)),
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
    if arguments.first().is_some_and(|command| command == "verify") {
        // The Ops entry owns its runtime and original completion signals.
        // Forward native path arguments before the UTF-8 router.
        return simplestchat_ops::release::verify::run(&arguments[1..]);
    }
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    match run(&arguments) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("simplestchat-release: {error:#}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    /// The checkout `root` names is the workspace whose members include this
    /// crate, so its relative paths reach the files deploy and maintain-runtime
    /// read.
    #[test]
    fn root_is_the_workspace_checkout_that_lists_this_crate() {
        let manifest = std::fs::read_to_string(super::root().unwrap().join("Cargo.toml")).unwrap();
        let manifest: toml::Value = toml::from_str(&manifest).unwrap();
        let members = manifest["workspace"]["members"].as_array().unwrap();
        assert!(
            members
                .iter()
                .any(|member| member.as_str() == Some("crates/release")),
            "{members:?}"
        );
    }
}
