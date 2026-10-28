//! One prepared-host maintenance transaction, including both actual lock holds.

use super::{
    control::{Cause, Owner, Result},
    deploy,
};
use std::{
    ffi::OsString,
    path::PathBuf,
    process::ExitCode,
    time::{Duration, Instant},
};

pub(super) struct Options {
    pub revision: String,
    pub whole: Duration,
    pub reserve: Duration,
    pub candidate_env: Option<PathBuf>,
    pub browser_assets_upgrade: bool,
}
impl Options {
    fn parse(arguments: &[OsString]) -> std::result::Result<Self, ()> {
        let mut revision = None;
        let mut whole = None;
        let mut reserve = None;
        let mut candidate = None;
        let mut upgrade = None;
        let mut quiet = None;
        let mut at = 0;
        while at < arguments.len() {
            let word = arguments[at].to_str().ok_or(())?;
            if matches!(
                word,
                "--browser-assets-upgrade" | "--no-browser-assets-upgrade"
            ) {
                if upgrade
                    .replace(word == "--browser-assets-upgrade")
                    .is_some()
                {
                    return Err(());
                }
                at += 1;
                continue;
            }
            let (name, value) = if let Some((name, value)) = word.split_once('=') {
                (name, OsString::from(value))
            } else {
                at += 1;
                (word, arguments.get(at).ok_or(())?.clone())
            };
            let number = || {
                let text = value.to_str().ok_or(())?;
                if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
                    return Err(());
                }
                text.parse::<u64>().map_err(|_| ())
            };
            match name {
                "--revision" if revision.is_none() => revision = Some(value),
                "--timeout-seconds" if whole.is_none() => whole = Some(number()?),
                "--finalization-seconds" if reserve.is_none() => reserve = Some(number()?),
                "--candidate-env" if candidate.is_none() => candidate = Some(PathBuf::from(value)),
                // The frozen maintain route accepts this common option but does
                // not wait for quiet rooms. Preserve that deliberate behavior.
                "--quiet-seconds" if quiet.is_none() => quiet = Some(number()?),
                _ => return Err(()),
            }
            at += 1;
        }
        let revision = revision.ok_or(())?.into_string().map_err(|_| ())?;
        let whole = Duration::from_secs(whole.ok_or(())?);
        let reserve = Duration::from_secs(reserve.ok_or(())?);
        if revision.len() != 40
            || !revision
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || whole.is_zero()
            || whole > Duration::from_secs(1800)
            || reserve.is_zero()
            || reserve >= whole
            || quiet.unwrap_or(0) > 600
            || candidate
                .as_ref()
                .is_some_and(|p| !super::files::canonical(p))
        {
            return Err(());
        }
        Ok(Self {
            revision,
            whole,
            reserve,
            candidate_env: candidate,
            browser_assets_upgrade: upgrade.unwrap_or(false),
        })
    }
}

pub(super) trait Launcher {
    fn run(
        &mut self,
        owner: &mut Owner,
        ceiling: Instant,
        observed: &mut Option<serde_json::Value>,
    ) -> Result<()>;
}
struct NativeLauncher;
impl Launcher for NativeLauncher {
    fn run(
        &mut self,
        owner: &mut Owner,
        ceiling: Instant,
        observed: &mut Option<serde_json::Value>,
    ) -> Result<()> {
        #[cfg(target_os = "linux")]
        {
            let mut completed: Option<super::maintain_prerequisite::NormalLaunch> = None;
            let result =
                super::maintain_prerequisite::launch_normal(owner, ceiling, &mut completed);
            let captured = match completed.map(serde_json::to_value).transpose() {
                Ok(value) => {
                    *observed = value;
                    Ok(())
                }
                Err(_) => {
                    owner.record(Cause::Io);
                    Err(Cause::Io)
                }
            };
            result?;
            captured?;
            owner.check(super::control::Window::Body)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (owner, ceiling, observed);
            Err(Cause::Policy)
        }
    }
}

const USAGE: &str = concat!(
    "Usage: simplestchat-ops release maintain --revision SHA40 ",
    "[--candidate-env ABS] [--browser-assets-upgrade|--no-browser-assets-upgrade] ",
    "--timeout-seconds N --finalization-seconds M\n\n",
    "Root on the fixed prepared Linux host; explicit 0<M<N<=1800.\n",
    "Back up before stopping the app and proxy; select durably, release the lock,\n",
    "run the normal launcher, reacquire a fresh lock and verify the final state.\n",
    "Launcher failure retains the selected release. Authenticated installation\n",
    "and the operational caller must be established separately.\n",
);

/// Maintain one prepared host under a single original deadline and signal owner.
/// A nonzero exit invalidates any complete or partial earlier report.
/// Host effects require execution through the installed canonical Ops binary;
/// the borrowed native launcher checks its inode after durable selection.
///
/// # Panics
///
/// Run outside an entered Tokio runtime; runtime creation/drop may otherwise panic.
///
/// ```
/// use simplestchat_ops::release::maintain;
/// use std::{ffi::OsString, process::ExitCode};
/// assert_eq!(maintain::run(&[OsString::from("--help")]), ExitCode::SUCCESS);
/// ```
pub fn run(arguments: &[OsString]) -> ExitCode {
    let started = Instant::now();
    if arguments.len() == 1 && arguments[0] == "--licenses" {
        return if super::acquire::licenses::stdout().is_ok() {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        };
    }
    if arguments.len() == 1 && matches!(arguments[0].to_str(), Some("--help" | "-h")) {
        return if super::prepare::stdout(USAGE.as_bytes()).is_ok() {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        };
    }
    let options = match Options::parse(arguments) {
        Ok(value) => value,
        Err(()) => {
            use std::io::Write;
            let _ = std::io::stderr().write_all(USAGE.as_bytes());
            return ExitCode::from(2);
        }
    };
    super::stage::lifecycle(started, options.whole, || {
        let mut owner = match Owner::new(started, options.whole, options.reserve) {
            Ok(value) => value,
            Err(_) => return (false, None),
        };
        if rustix::process::geteuid().as_raw() != 0 || rustix::system::uname().sysname() != c"Linux"
        {
            owner.record(Cause::Policy);
            return (false, Some(owner));
        }
        let runtime = match tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
        {
            Ok(value) => value,
            Err(_) => {
                owner.record(Cause::Io);
                return (false, Some(owner));
            }
        };
        let outcome = runtime.block_on(deploy::execute_maintenance(
            &options,
            &deploy::Access::fixed(),
            &mut owner,
            &mut deploy::process::Native,
            &mut NativeLauncher,
        ));
        drop(runtime);
        let written = super::prepare::stdout(&outcome.bytes);
        if written.is_err() {
            owner.record(Cause::Io);
        }
        (outcome.passed && written.is_ok(), Some(owner))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arguments(extra: &[&str]) -> Vec<OsString> {
        let mut args = vec![
            "--revision".into(),
            "a".repeat(40).into(),
            "--timeout-seconds".into(),
            "1800".into(),
            "--finalization-seconds".into(),
            "60".into(),
        ];
        args.extend(extra.iter().map(OsString::from));
        args
    }

    #[test]
    fn reviewed_inputs_and_legacy_quiet_option_do_not_add_another_clock() {
        let parsed = Options::parse(&arguments(&[
            "--candidate-env",
            "/private/candidate.env",
            "--browser-assets-upgrade",
            "--quiet-seconds",
            "600",
        ]))
        .unwrap();
        assert_eq!(parsed.revision, "a".repeat(40));
        assert_eq!(parsed.whole, Duration::from_secs(1800));
        assert_eq!(parsed.reserve, Duration::from_secs(60));
        assert_eq!(
            parsed.candidate_env,
            Some(PathBuf::from("/private/candidate.env"))
        );
        assert!(parsed.browser_assets_upgrade);
        let ordinary = Options::parse(&arguments(&["--no-browser-assets-upgrade"])).unwrap();
        assert!(!ordinary.browser_assets_upgrade);
        assert!(ordinary.candidate_env.is_none());
    }

    #[test]
    fn unsupported_selectors_duplicate_ownership_options_and_invalid_paths_refuse() {
        for extra in [
            vec!["--host", "unreviewed"],
            vec!["--timeout-seconds", "1800"],
            vec!["--browser-assets-upgrade", "--no-browser-assets-upgrade"],
            vec!["--candidate-env", "relative"],
            vec!["--candidate-env", "/private/../candidate"],
            vec!["--quiet-seconds", "601"],
            vec!["--quiet-seconds", "-1"],
            vec!["--quiet-seconds", "18446744073709551616"],
        ] {
            assert!(Options::parse(&arguments(&extra)).is_err(), "{extra:?}");
        }
        for (whole, reserve) in [(0, 1), (1801, 60), (1800, 0), (60, 60), (60, 61)] {
            let mut args = arguments(&[]);
            args[3] = whole.to_string().into();
            args[5] = reserve.to_string().into();
            assert!(Options::parse(&args).is_err());
        }
    }
}
