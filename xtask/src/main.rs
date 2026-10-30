//! Repository automation: `cargo run -q -p xtask -- <command>`.
//!
//! Every command runs from the repository root and exits non-zero on the first
//! failure so CI and local runs behave identically; the test harness commands
//! exit with the status the shell helpers they replaced reported.

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

mod check;
mod ci;
mod ci_evidence;
mod ci_local;
mod ci_retention;
mod http_policy;
mod line_length;
mod media_ip;
mod owned_process;
mod performance_thresholds;
mod protocol_ts;
mod run_local;
mod test_env;
mod tooling;

const USAGE: &str = "\
Usage: cargo run -q -p xtask -- <command>

Commands:
  ci-local      Run all required local workflow jobs, or one explicitly partial job,
                against only an owned disposable engine; see `ci-local --help`.
  run           Build and run the local UI; --skip-web reuses web/dist.
  release       Select signed application and Ops artifacts, deploy once and verify HTTPS;
                prepared routine host only; see `release deploy --help`.
  public-smoke  Check an explicitly authorized HTTPS origin; writes ONE PUBLIC TEST MESSAGE.
                Requires --features public-smoke; private summary only; see public-smoke --help.
  ops-artifact  Export source, hydrate registry, build unsigned Ops, or verify retained Ops;
                fixed recipe; maintain-runtime owns the optional signed maintenance KVM proof.
  check         Run installed-tool quality gates: [--web|--rust|--python|--helpers].
                Default runs all groups, without installs or application services.
  container-benchmark Standalone root/Linux private benchmark run or locked recovery;
  vps-capacity Authenticated private VPS measurement, collection or explicit recovery;
                installed service callsites remain unchanged; see --help.
  container-smoke Check an already-built image on the selected local Unix engine;
                creates and removes only its own disposable smoke containers.
  ci            Exact-input CI keys and success receipts: key|save|verify --scope SCOPE.
                key --github-output appends the fixed key output to GITHUB_OUTPUT.
                native-security verifies dependency pins, overlays and server linkage.
                automation runs installed controller quality, syntax and unit checks.
                codeql-cache keys and validates analyzed databases with fresh receipts.
                required rejects any missing or unsuccessful canonical workflow job.
                browser-suite runs a maintained suite in Chromium then Firefox.
                copy-evidence preserves a fixed private local-CI artifact projection.
                production-image builds and verifies the immutable CI production image.
                install-compose installs the authenticated Compose release in explicit CI.
                apt-mirror prefers official archives in the fixed root/hosted Ubuntu profile.
                audit-rust installs verified audit tooling and applies the advisory policy.
                release-checks discovers Chromium and requires \
                --http-policy-binary ABSOLUTE_FEATURE_BINARY.
                release-http-policy checks only the owned loopback fixture (optional feature).
                release-controller overlaps bounded controller preparation with the image build.
                commit-subject checks the branch tip against the local hook's byte grammar.
                container-smoke checks immutable production startup and database assertions.
                validate-config runs fixed Promtool or Caddy validation in an owned container.
                load-generator-image builds and retains the unsigned manual generator image.
                mdns owns the opt-in macOS service snapshot and disposable registration diagnostic.
                database-check runs monitoring, restore or migration in owned PostgreSQL.
                session-soak supervises the fixed workload and runner-owned database outage.
                performance-compare owns the three builds, worktree and fixed six-run comparison.
                native-dtls checks tagged native regressions in a disposable source copy.
                npm-path publishes the pinned npm bin directory through the runner file.
                controller-path publishes the installed automation controller bin directory.
                image-cache publishes validated database-byte cache selectors.
                CodeQL extraction keys never substitute for analyzed databases.
  ci-evidence   Record or require complete executed local-CI matrix evidence.
  ci-retention  Print an allowlisted main-cache retention plan; --apply requires
                exact successful main CI and refreshed inventory before each deletion.
  line-length   Fail when Rust production code has more lines over 100 columns
                than the ratchet in xtask/line-length-allowance.txt permits;
                `--update` lowers the allowance to the current count.
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
        "release" => ci::run_release_frontend(&root, rest),
        "check" => check::run(&root, rest),
        "ci" => ci::run(&root, rest),
        "container-smoke" => ci::run_container_smoke(&root, rest),
        "container-benchmark" => ci::run_container_benchmark(rest),
        "ops-artifact" => ci::run_ops_artifact(rest),
        "vps-capacity" => ci::run_vps_capacity(rest),
        "public-smoke" => ci::run_public_smoke(rest),
        "ci-evidence" => ci_evidence::run(&root, rest),
        "ci-local" => ci_local::run(&root, rest),
        "ci-retention" => ci_retention::run(&root, rest),
        "line-length" => line_length::run(&root, rest).map(|()| ExitCode::SUCCESS),
        "protocol-ts" => protocol_ts::run(&root, rest).map(|()| ExitCode::SUCCESS),
        "http-policy" => http_policy::run(&root, rest).map(|()| ExitCode::SUCCESS),
        "media-ip" => media_ip::run(&root, rest),
        "performance-thresholds" => performance_thresholds::run(&root, rest),
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
