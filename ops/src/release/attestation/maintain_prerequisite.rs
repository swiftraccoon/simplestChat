//! Fixed native launcher proof and an internally borrowed normal transport.
#[cfg(any(target_os = "linux", test))]
use super::control::Cause;
#[cfg(target_os = "linux")]
use super::control::{Owner, Window};
use std::{ffi::OsString, process::ExitCode};

#[cfg(feature = "maintain-guest")]
mod guest;

#[cfg(target_os = "linux")]
mod identity;
#[cfg(target_os = "linux")]
mod process;
#[cfg(any(target_os = "linux", test))]
mod protocol;
#[cfg(target_os = "linux")]
mod supervisor;

#[cfg(any(target_os = "linux", test))]
const WHOLE: u64 = 1050;
#[cfg(any(target_os = "linux", test))]
const FINAL: u64 = 60;
#[cfg(target_os = "linux")]
const LAUNCHER: u64 = 900;
#[cfg(target_os = "linux")]
const FRAME: u64 = 5;
#[cfg(target_os = "linux")]
const TERM: u64 = 5;
#[cfg(target_os = "linux")]
const REAP: u64 = 5;
#[cfg(target_os = "linux")]
const KNOWN: u64 = LAUNCHER + FRAME * 3 + TERM + REAP;
#[cfg(any(target_os = "linux", test))]
const MAX_FRAME: usize = 16384;
#[cfg(any(target_os = "linux", test))]
const MAX_STREAM: u64 = 2 * 1024 * 1024;
#[cfg(target_os = "linux")]
const OPS: &str = "/usr/local/libexec/simplestchat-public/simplestchat-ops";
#[cfg(target_os = "linux")]
const UNSHARE: &str = "/usr/bin/unshare";
#[cfg(target_os = "linux")]
const LAUNCH: &str = "/usr/local/bin/simplestchat-public-deploy";
#[cfg(target_os = "linux")]
const TIMEOUT: &str = "/usr/bin/timeout";
const USAGE: &str = concat!(
    "Usage: simplestchat-ops release maintain-prerequisite run ",
    "--trial normal|cancel-owned-launcher|timeout-stopped-launcher ",
    "--timeout-seconds 1050 --finalization-seconds 60\n",
);

#[cfg(any(target_os = "linux", test))]
type Result<T> = std::result::Result<T, Cause>;

#[cfg(any(target_os = "linux", test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
enum Trial {
    Normal,
    CancelOwnedLauncher,
    TimeoutStoppedLauncher,
}

#[cfg(any(target_os = "linux", test))]
#[derive(Debug)]
enum Options {
    Run(Trial),
    Init {
        trial: Trial,
        nonce: String,
        clock: protocol::Clock,
    },
    NormalInit {
        nonce: String,
        clock: protocol::Clock,
    },
}
#[cfg(any(target_os = "linux", test))]
impl Options {
    fn parse(args: &[OsString]) -> Result<Self> {
        if args.len() > 11 {
            return Err(Cause::Input);
        }
        let words: Vec<&str> = args
            .iter()
            .map(|v| v.to_str().filter(|s| s.len() <= 64).ok_or(Cause::Input))
            .collect::<Result<_>>()?;
        let trial = |value| match value {
            "normal" => Ok(Trial::Normal),
            "cancel-owned-launcher" => Ok(Trial::CancelOwnedLauncher),
            "timeout-stopped-launcher" => Ok(Trial::TimeoutStoppedLauncher),
            _ => Err(Cause::Input),
        };
        match words.as_slice() {
            [
                "run",
                "--trial",
                t,
                "--timeout-seconds",
                "1050",
                "--finalization-seconds",
                "60",
            ] => Ok(Self::Run(trial(t)?)),
            [
                "init",
                "--trial",
                t,
                "--nonce",
                n,
                "--origin-monotonic-ns",
                origin,
                "--body-deadline-monotonic-ns",
                body,
                "--deadline-monotonic-ns",
                end,
            ] => {
                protocol::nonce(n)?;
                let clock = protocol::Clock::parse(origin, body, end)?;
                Ok(Self::Init {
                    trial: trial(t)?,
                    nonce: (*n).to_owned(),
                    clock,
                })
            }
            [
                "normal-init",
                "--nonce",
                n,
                "--origin-monotonic-ns",
                origin,
                "--body-deadline-monotonic-ns",
                body,
                "--deadline-monotonic-ns",
                end,
            ] => {
                protocol::nonce(n)?;
                Ok(Self::NormalInit {
                    nonce: (*n).to_owned(),
                    clock: protocol::Clock::parse_inherited(origin, body, end)?,
                })
            }
            _ => Err(Cause::Input),
        }
    }
}

#[cfg(target_os = "linux")]
pub(super) use supervisor::{NormalLaunch, launch_normal};

/// Only the explicit prerequisite route can enter the fixed native roles.
/// Earlier JSON is passive; the returned status includes every final fence.
pub fn run(args: &[OsString]) -> ExitCode {
    if args.first().is_some_and(|arg| arg == "deploy-injection") {
        #[cfg(feature = "maintain-guest")]
        return guest::run_deploy_injection(&args[1..]);
        #[cfg(not(feature = "maintain-guest"))]
        return ExitCode::from(2);
    }
    if args.first().is_some_and(|arg| arg == "deploy-guest") {
        #[cfg(feature = "maintain-guest")]
        return guest::run_deploy(&args[1..]);
        #[cfg(not(feature = "maintain-guest"))]
        return ExitCode::from(2);
    }
    if args.first().is_some_and(|arg| arg == "host-upgrade") {
        #[cfg(feature = "maintain-guest")]
        return guest::run_upgrade(&args[1..]);
        #[cfg(not(feature = "maintain-guest"))]
        return ExitCode::from(2);
    }
    if args.first().is_some_and(|arg| arg == "host-guest") {
        #[cfg(feature = "maintain-guest")]
        return guest::run_host(&args[1..]);
        #[cfg(not(feature = "maintain-guest"))]
        return ExitCode::from(2);
    }
    if args.first().is_some_and(|arg| arg == "guest") {
        #[cfg(feature = "maintain-guest")]
        return guest::run(&args[1..]);
        #[cfg(not(feature = "maintain-guest"))]
        return ExitCode::from(2);
    }
    #[cfg(target_os = "linux")]
    {
        let origin = match protocol::now() {
            Ok(v) => v,
            Err(_) => return ExitCode::FAILURE,
        };
        match Options::parse(args) {
            Ok(options) => supervisor::entry(options, origin),
            Err(_) => {
                eprint!("{USAGE}");
                ExitCode::from(2)
            }
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = args;
        eprint!("{USAGE}");
        ExitCode::FAILURE
    }
}

/// Fixed client for the marked disposable deploy fixture's private mount view.
/// The guest admits its ownership marker and peer before forwarding any request.
#[cfg(feature = "maintain-guest")]
pub fn deploy_fixture_docker(args: &[OsString]) -> ExitCode {
    guest::run_deploy_docker(args)
}
#[cfg(target_os = "linux")]
fn fail<T>(owner: &mut Owner, cause: Cause) -> Result<T> {
    owner.record(cause);
    Err(cause)
}
#[cfg(target_os = "linux")]
fn status(cause: Option<Cause>) -> u8 {
    match cause {
        None => 0,
        Some(Cause::Deadline) => 124,
        Some(Cause::Interrupt) => 130,
        Some(Cause::Terminate) => 143,
        Some(_) => 1,
    }
}
#[cfg(test)]
mod tests;
