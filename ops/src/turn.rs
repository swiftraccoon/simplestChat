//! The public relay's certificate and metrics. The relay is a separate
//! project that never owns the database or the proxy. A certificate is
//! published as an immutable generation of validated bytes and the relay is
//! told to reload it, which keeps relayed calls; enabling metrics replaces
//! only the relay, once, with a bounded rollback.

mod publish;
mod relay;
mod settings;
pub(crate) mod store;
#[cfg(test)]
pub(crate) mod tests;

use crate::host::command::Interruptible;
use crate::host::evidence::Evidence;
use crate::host::fault::{
    DIRECTORY, Fault, Numbered, Result, VALUE, decode, digest, object, protected, require, set,
    string, timestamp,
};
use crate::host::state::{self, Access};
use crate::host::{Host, Paths, System, emit};
use crate::json::{InOrder, Json};
use crate::release::assets::{self, Takeover, Workload};
pub(crate) use publish::Disposable;
use relay::METRICS_OPTIONS;
use std::ffi::OsString;
use std::net::IpAddr;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use store::Site;

const UNFINISHED: &str = "Inspect and recover the unfinished release before starting another";
/// The pinned relay prints its help on standard error and exits 255. Only
/// that status and zero count as success for this one probe.
const METRICS_HELP_PROBE: &str = concat!(
    "/usr/bin/turnserver --help 2>&1\n",
    "status=$?\n",
    "case \"$status\" in\n",
    "    0|255) exit 0 ;;\n",
    "    *) exit \"$status\" ;;\n",
    "esac\n",
);
const METRICS_HELP_OPTIONS: [&str; 3] =
    ["--prometheus", "--prometheus-address", "--prometheus-port"];

/// What the command was asked to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    Prepare,
    Refresh,
    Metrics,
}

impl Action {
    fn name(self) -> &'static str {
        match self {
            Self::Prepare => "prepare-certificate",
            Self::Refresh => "refresh-certificate",
            Self::Metrics => "enable-metrics",
        }
    }
}

/// One action, and the address to verify the relay at instead of its name.
#[derive(Debug, PartialEq, Eq)]
struct Request {
    action: Action,
    address: Option<String>,
}

/// Publish the newest certificate, tell the relay to reload it and require
/// the relay to present it.
fn refresh(
    site: &Site<'_>,
    runner: &mut Numbered<'_>,
    hostname: &str,
    address: Option<IpAddr>,
) -> Result<bool> {
    let identity = relay::identity(site, runner)?;
    let changed = publish::publish(site, runner, hostname)?;
    // The relay is signalled even when the copy already matched: an earlier
    // reload may have failed after publication, and repeating the signal
    // does no harm.
    runner.docker(&["kill", "--signal", "SIGUSR2", &identity], 30.0)?;
    relay::verify_tls(site, hostname, address)?;
    Ok(changed)
}

fn contains(text: &[u8], word: &str) -> bool {
    text.windows(word.len())
        .any(|window| window == word.as_bytes())
}

/// Give the relay its loopback metrics listener by replacing only the relay,
/// once; a replacement that cannot be verified is rolled back. Returns
/// whether anything changed.
fn enable(
    site: &Site<'_>,
    runner: &mut Numbered<'_>,
    hostname: &str,
    address: Option<IpAddr>,
) -> Result<bool> {
    let identity = relay::identity(site, runner)?;
    let config = &site.host.paths.relay;
    let (prepared, record) = (config.join("turnserver.conf"), config.join("settings.json"));
    let original = std::fs::read(&prepared)?;
    let selected = std::fs::read(&record)?;
    if original.ends_with(METRICS_OPTIONS.as_bytes()) {
        relay::verify_metrics(runner)?;
        return Ok(false);
    }
    let lines = original.split(|byte| matches!(byte, b'\n' | b'\r'));
    let present = lines
        .into_iter()
        .any(|line| line.starts_with(b"prometheus"));
    require(!present, "Unexpected existing relay metrics options")?;
    let probe = ["exec", &identity, "/bin/sh", "-c", METRICS_HELP_PROBE];
    let help = runner.docker(&probe, 30.0)?;
    let supported = METRICS_HELP_OPTIONS
        .iter()
        .all(|option| contains(&help, option));
    require(supported, "Relay binary lacks bounded metrics support")?;
    // Neither a certificate reload nor a hang-up starts the new listener: one
    // relay replacement is needed. Secrets, certificates and ports stay.
    let attempt = runner.0.attempt.clone();
    state::atomic(&attempt.join("before-turnserver.conf"), &original)?;
    state::atomic(&attempt.join("before-settings.json"), &selected)?;
    let end = original
        .iter()
        .rposition(|byte| *byte != b'\n')
        .map_or(0, |at| at + 1);
    let updated = [&original[..end], b"\n", METRICS_OPTIONS.as_bytes()].concat();
    let mut metadata = decode(&selected)?;
    metadata.object().ok_or(VALUE)?;
    set(
        &mut metadata,
        "configurationSha256",
        Json::String(digest(&updated)),
    );
    let access = Access {
        owner: site.host.owner,
        group: site.relay_group,
        mode: 0o440,
    };
    let replace = |runner: &mut Numbered<'_>| -> Result<()> {
        let recreated = [
            "up",
            "--detach",
            "--no-deps",
            "--no-build",
            "--pull",
            "never",
            "--force-recreate",
            "turn",
        ];
        relay::compose(site, runner, &recreated)?;
        relay::identity(site, runner)?;
        relay::verify_tls(site, hostname, address)
    };
    let applied = (|| {
        state::atomic_for(&prepared, &updated, &access)?;
        state::atomic_json(&record, &InOrder(&metadata))?;
        replace(runner)?;
        relay::verify_metrics(runner)
    })();
    if let Err(failure) = applied {
        (site.rearm)();
        state::atomic_for(&prepared, &original, &access)?;
        state::atomic(&record, &selected)?;
        replace(runner)?;
        return Err(failure);
    }
    Ok(true)
}

/// What a transaction leaves the caller: what to print, and how it ended.
type Settled = (String, Result<()>);

/// One transaction under the held workload lock.
fn transact(site: &Site<'_>, action: Action, address: Option<IpAddr>) -> Settled {
    let host = site.host;
    let opened = (|| {
        let hostname = settings::settings(site)?.domain;
        let results = host.paths.root.join("results");
        protected(host.owner, &results, DIRECTORY)?;
        Ok((hostname, state::private_directory(&results, "turn.")?))
    })();
    let (hostname, attempt) = match opened {
        Ok(opened) => opened,
        Err(fault) => return (String::new(), Err(fault)),
    };
    let mut report = object(vec![
        ("action", string(action.name())),
        ("startedAt", timestamp(host)),
        ("passed", Json::Bool(false)),
    ]);
    let mut runner = Numbered(Evidence::new(host, attempt.clone()));
    let (field, changed) = match action {
        Action::Metrics => (
            "metricsChanged",
            enable(site, &mut runner, &hostname, address),
        ),
        Action::Prepare => (
            "certificateChanged",
            publish::publish(site, &mut runner, &hostname),
        ),
        Action::Refresh => (
            "certificateChanged",
            refresh(site, &mut runner, &hostname, address),
        ),
    };
    let mut said = String::new();
    if let Ok(changed) = &changed {
        set(&mut report, field, Json::Bool(*changed));
        set(&mut report, "passed", Json::Bool(true));
        if *changed && action == Action::Metrics {
            said.push_str("Relay metrics enabled\n");
        }
    }
    set(&mut report, "finishedAt", timestamp(host));
    let written = state::atomic_json(&attempt.join("outcome.json"), &InOrder(&report));
    if let Err(failure) = written {
        return (said, Err(Fault::from(failure)));
    }
    said.push_str(&format!("Relay evidence: {}\n", attempt.display()));
    (said, changed.map(|_| ()))
}

/// Run one action: admit the request and the caller, take the workload lock
/// through `lock`, and release it whatever happens under it.
fn execute(
    site: &Site<'_>,
    request: &Request,
    privileged: bool,
    lock: impl FnOnce(Takeover<'_>) -> std::result::Result<Workload, assets::Failure>,
) -> Settled {
    let admitted = (|| {
        let address = request.address.as_deref().map(str::parse::<IpAddr>);
        let address = address.transpose().map_err(|_| VALUE)?;
        let verifies = request.action != Action::Prepare;
        require(
            address.is_none() || verifies,
            "An address override requires a relay TLS verification action",
        )?;
        require(privileged, "Run as root on the prepared public host")?;
        Ok(address)
    })();
    let address = match admitted {
        Ok(address) => address,
        Err(fault) => return (String::new(), Err(fault)),
    };
    let workload = match lock(Takeover::Nothing) {
        Ok(workload) => workload,
        Err(failure) if failure.busy() => {
            return (String::new(), Err(Fault::Kind("BlockingIOError")));
        }
        Err(_) => return (String::new(), Err(Fault::Refused(UNFINISHED.to_owned()))),
    };
    let (said, outcome) = transact(site, request.action, address);
    let released = workload.finish().map_err(|_| Fault::Kind("OSError"));
    (said, outcome.and(released))
}

/// One action and at most one address override, or nothing for any other
/// command line.
fn parse(arguments: &[OsString]) -> Option<Request> {
    let (mut action, mut address) = (None, None);
    let mut words = arguments.iter().map(|word| word.to_str());
    while let Some(word) = words.next() {
        let word = word?;
        let value = match word.strip_prefix("--verify-address") {
            Some("") => Some(words.next()??),
            Some(given) => Some(given.strip_prefix('=')?),
            None => None,
        };
        match value {
            Some(value) if address.is_none() => address = Some(value.to_owned()),
            None if action.is_none() && !word.starts_with('-') => {
                let known = [Action::Prepare, Action::Refresh, Action::Metrics];
                action = Some(known.into_iter().find(|known| known.name() == word)?);
            }
            _ => return None,
        }
    }
    Some(Request {
        action: action?,
        address,
    })
}

/// The one line a failed action leaves the caller: the kind of fault and
/// where to look, never private detail.
fn complaint(fault: &Fault) -> String {
    let kind = fault.kind();
    format!("Relay operation failed ({kind}); inspect private evidence.\n")
}

/// Run `turn prepare-certificate|refresh-certificate|enable-metrics` on the
/// prepared host. A syntax error returns 2 before any effect; what a
/// transaction has to say is printed also when it failed.
pub fn run(arguments: &[OsString]) -> ExitCode {
    let Some(request) = parse(arguments) else {
        return ExitCode::from(2);
    };
    rustix::process::umask(rustix::fs::Mode::from_raw_mode(0o077));
    let stop = Arc::new(AtomicBool::new(false));
    let handled = [signal_hook::consts::SIGTERM, signal_hook::consts::SIGINT]
        .iter()
        .all(|signal| signal_hook::flag::register(*signal, Arc::clone(&stop)).is_ok());
    let commands = Interruptible {
        stop: &stop,
        refusal: "Relay operation interrupted",
    };
    let clock = System::new();
    let host = Host {
        commands: &commands,
        clock: &clock,
        paths: Paths::fixed(),
        owner: 0,
    };
    let rearm = || stop.store(false, Ordering::SeqCst);
    let site = Site {
        host: &host,
        proxy: 10001,
        proxy_group: 10001,
        relay_group: 10002,
        rearm: &rearm,
    };
    let privileged = rustix::process::geteuid().is_root();
    let (said, outcome) = if handled {
        execute(&site, &request, privileged, Workload::acquire_for)
    } else {
        (String::new(), Err(Fault::Kind("OSError")))
    };
    let printed = said.is_empty() || emit(std::io::stdout(), &said);
    let outcome = if printed {
        outcome
    } else {
        Err(Fault::Kind("OSError"))
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(fault) => {
            let _ = emit(std::io::stderr(), &complaint(&fault));
            ExitCode::FAILURE
        }
    }
}
