//! The five maintained two-browser workflow suites, without shell command text.
//!
//! Suite selection fixes the script, Chromium/Firefox order and artifact names.
//! Existing workflow opt-ins, service isolation and Playwright guards still own
//! what the selected suite may do. The first failed or interrupted harness stops
//! the plan; this command never records verification or expands the CI matrix.

use crate::tooling::{self, Environment, Failure};
use anyhow::Result;
use process::Signals;
use std::ffi::{OsStr, OsString};
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

#[path = "browser_suite_cleanup.rs"]
pub(super) mod cleanup;
#[path = "browser_suite_process.rs"]
pub(super) mod process;

const USAGE: &str = "usage: cargo run --locked -q -p xtask -- ci browser-suite \
auth-lifecycle|auth-cross-tab|room-control-confirmation|media-continuity|presentation";
// The harness can spend 10s stopping its test child, then 5s each on HTTPS,
// server and TURN cleanup. Allow all of those normal graces before escalation.
const CLEANUP_GRACE: Duration = Duration::from_secs(45);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Suite {
    AuthLifecycle,
    AuthCrossTab,
    RoomControl,
    MediaContinuity,
    Presentation,
}

impl Suite {
    fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "auth-lifecycle" => Self::AuthLifecycle,
            "auth-cross-tab" => Self::AuthCrossTab,
            "room-control-confirmation" => Self::RoomControl,
            "media-continuity" => Self::MediaContinuity,
            "presentation" => Self::Presentation,
            _ => return None,
        })
    }

    fn script(self) -> &'static str {
        match self {
            Self::AuthLifecycle => "web/e2e/auth-lifecycle.cjs",
            Self::AuthCrossTab => "web/e2e/auth-cross-tab.cjs",
            Self::RoomControl => "web/e2e/room-control-confirmation.cjs",
            Self::MediaContinuity => "web/e2e/media-continuity.cjs",
            Self::Presentation => "web/e2e/presentation.cjs",
        }
    }

    fn artifact_prefix(self) -> &'static str {
        match self {
            Self::AuthLifecycle => "auth-lifecycle",
            Self::AuthCrossTab => "account-tabs",
            Self::RoomControl => "room-controls",
            Self::MediaContinuity => "media-continuity",
            Self::Presentation => "presentation",
        }
    }

    fn needs_media_ip(self) -> bool {
        matches!(self, Self::MediaContinuity | Self::Presentation)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Browser {
    Chromium,
    Firefox,
}

impl Browser {
    fn name(self) -> &'static str {
        match self {
            Self::Chromium => "chromium",
            Self::Firefox => "firefox",
        }
    }
}

struct Invocation {
    executable: PathBuf,
    arguments: Vec<OsString>,
    environment: Environment,
}

fn artifact_root(environment: &Environment) -> tooling::Result<PathBuf> {
    let path = environment
        .get(OsStr::new("RUNNER_TEMP"))
        .map(PathBuf::from)
        .filter(|path| path.is_absolute() && path.is_dir())
        .ok_or_else(|| Failure::new(2, "RUNNER_TEMP must name an existing absolute directory."))?;
    Ok(path)
}

/// Child values are separate argv/env entries, including paths with spaces.
fn plan(
    suite: Suite,
    environment: &Environment,
    executable: &Path,
    media_ip: Option<Ipv4Addr>,
) -> tooling::Result<Vec<Invocation>> {
    let artifacts = artifact_root(environment)?;
    if suite.needs_media_ip() && media_ip.is_none() {
        return Err(Failure::new(1, "Cannot select the owned test media IP."));
    }
    Ok([Browser::Chromium, Browser::Firefox]
        .into_iter()
        .map(|browser| {
            let mut environment = environment.clone();
            environment.insert("E2E_BROWSER".into(), browser.name().into());
            environment.insert(
                "E2E_ARTIFACTS".into(),
                artifacts
                    .join(format!("{}-{}", suite.artifact_prefix(), browser.name()))
                    .into_os_string(),
            );
            if suite.needs_media_ip() {
                environment.insert(
                    "TEST_ANNOUNCE_IP".into(),
                    media_ip
                        .expect("required media address established")
                        .to_string()
                        .into(),
                );
            }
            Invocation {
                executable: executable.into(),
                arguments: ["test-server", "node", suite.script()]
                    .into_iter()
                    .map(OsString::from)
                    .collect(),
                environment,
            }
        })
        .collect())
}

async fn execute(
    root: &Path,
    invocations: &[Invocation],
    signals: &mut Signals,
    cleanup_grace: Duration,
) -> tooling::Result<()> {
    for invocation in invocations {
        process::run(root, invocation, signals, cleanup_grace).await?;
    }
    if let Some(code) = signals.pending().await {
        return Err(Failure::new(code, "Browser suite interrupted."));
    }
    Ok(())
}

/// Resolve the shared address once, before either harness can start. Dropping a
/// cancelled detector future releases its existing owned read-only probe.
async fn prepare(
    suite: Suite,
    environment: &Environment,
    executable: &Path,
    signals: &mut Signals,
    detector: impl std::future::Future<Output = Result<Ipv4Addr>>,
) -> tooling::Result<Vec<Invocation>> {
    artifact_root(environment)?;
    let media_ip = if suite.needs_media_ip() {
        Some(tokio::select! {
            biased;
            code = signals.next() => {
                return Err(Failure::new(code, "Browser suite interrupted."));
            },
            result = detector => result.map_err(|error| {
                Failure::new(1, format!("Cannot select test media IP: {error:#}"))
            })?,
        })
    } else {
        None
    };
    plan(suite, environment, executable, media_ip)
}

pub(super) fn run(root: &Path, arguments: &[String]) -> Result<ExitCode> {
    if arguments.len() == 1 && matches!(arguments[0].as_str(), "--help" | "-h") {
        println!("{USAGE}");
        return Ok(ExitCode::SUCCESS);
    }
    let Some(suite) = arguments
        .first()
        .filter(|_| arguments.len() == 1)
        .and_then(|value| Suite::parse(value))
    else {
        eprintln!("{USAGE}");
        return Ok(ExitCode::from(2));
    };
    let environment = tooling::inherited_env();
    let result = async {
        let mut signals = Signals::new()?;
        let executable = std::env::current_exe()
            .map_err(|_| Failure::new(1, "Cannot identify the owned test harness executable."))?;
        let invocations = prepare(
            suite,
            &environment,
            &executable,
            &mut signals,
            crate::media_ip::detect(),
        )
        .await?;
        execute(root, &invocations, &mut signals, CLEANUP_GRACE).await
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    match runtime.block_on(result) {
        Ok(()) => Ok(ExitCode::SUCCESS),
        Err(error) => {
            eprintln!("{}", error.message);
            Ok(ExitCode::from(error.code))
        }
    }
}

#[cfg(test)]
#[path = "browser_suite_tests.rs"]
mod tests;
