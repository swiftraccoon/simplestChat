//! Backups of the deployment's database on the prepared host. Importing this
//! module starts nothing. Every command works on fixed local paths, the
//! local Docker daemon and the deployment's own database, under the shared
//! workload lock, and takes no host, statement or path argument.

mod nightly;
mod offsite;
pub(crate) mod receipts;
mod restore;

use crate::host::command::Interruptible;
use crate::host::failure::{Failure, Result, require};
use crate::host::{Host, Paths, System, emit};
use crate::release::assets::{self, Takeover, Workload};
use nightly::{Action, Request};
pub(crate) use nightly::{digest as file_digest, dump_database, headroom, publish};
use std::ffi::OsString;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

const FAILED: &str = "Nightly backup failed; inspect the private backup logs and journals.\n";
const OFFSITE_FAILED: &str = "Offsite operation failed; inspect private evidence and settings.\n";

/// The request `arguments` spell, or nothing when they are not one: an
/// action, then each retention option at most once with a whole number.
fn parse(arguments: &[OsString]) -> Option<Request> {
    let mut words = arguments.iter().map(|word| word.to_str());
    let action = match words.next()?? {
        "nightly" => Action::Nightly,
        "cleanup" => Action::Cleanup,
        _ => return None,
    };
    let (mut days, mut at_least) = (None, None);
    while let Some(word) = words.next() {
        let word = word?;
        let (name, value) = match word.split_once('=') {
            Some(pair) => pair,
            None => (word, words.next()??),
        };
        let slot = match name {
            "--keep-days" => &mut days,
            "--keep-at-least" => &mut at_least,
            _ => return None,
        };
        let digits = value.strip_prefix('-').unwrap_or(value);
        let whole = (1..=9).contains(&digits.len()) && digits.bytes().all(|b| b.is_ascii_digit());
        if !whole || slot.is_some() {
            return None;
        }
        *slot = value.parse::<i64>().ok();
    }
    Some(Request {
        action,
        days: days.unwrap_or(14),
        at_least: at_least.unwrap_or(3),
    })
}

/// Run one request. The workload lock is taken through `lock` only once the
/// request is admitted, and is released whatever happens under it. The
/// nightly owner may take over its own unfinished record once that has aged
/// out or the host has rebooted since.
fn execute(
    host: &Host<'_>,
    request: Request,
    privileged: bool,
    lock: impl FnOnce(Takeover<'_>) -> std::result::Result<Workload, assets::Failure>,
) -> Result<Option<String>> {
    nightly::retention(request.days, request.at_least)?;
    require(privileged, "Nightly backup requires root")?;
    let boot = host.boot()?;
    let takeover = Takeover::ExpiredNightly {
        boot: &boot,
        monotonic: host.clock.monotonic().as_secs_f64(),
    };
    let workload = lock(takeover).map_err(|_| Failure::Os)?;
    let outcome = nightly::run(host, &boot, request);
    let released = workload.finish().map_err(|_| Failure::Os);
    let line = outcome?;
    released?;
    Ok(line)
}

/// The offsite request `arguments` spell after `offsite`, or nothing when
/// they are not one: an action, then each selector at most once.
fn parse_offsite(arguments: &[OsString]) -> Option<offsite::Request> {
    let mut words = arguments.iter().map(|word| word.to_str());
    let action = match words.next()?? {
        "upload" => offsite::Action::Upload,
        "restore" => offsite::Action::Restore,
        "recover" => offsite::Action::Recover,
        _ => return None,
    };
    let (mut snapshot, mut stamp) = (None, None);
    while let Some(word) = words.next() {
        let word = word?;
        let (name, value) = match word.split_once('=') {
            Some(pair) => pair,
            None => (word, words.next()??),
        };
        let slot = match name {
            "--snapshot" => &mut snapshot,
            "--stamp" => &mut stamp,
            _ => return None,
        };
        if slot.is_some() {
            return None;
        }
        *slot = Some(value.to_owned());
    }
    Some(offsite::Request {
        action,
        snapshot: snapshot.unwrap_or_default(),
        stamp: stamp.unwrap_or_default(),
    })
}

/// Run one offsite request under the workload lock taken through `lock`.
/// The offsite owner may take over its own unfinished record to resolve it.
fn execute_offsite(
    host: &Host<'_>,
    request: &offsite::Request,
    privileged: bool,
    lock: impl FnOnce(Takeover<'_>) -> std::result::Result<Workload, assets::Failure>,
) -> Result<&'static str> {
    require(privileged, "Offsite backup requires root")?;
    let workload = lock(Takeover::Offsite).map_err(|_| Failure::Os)?;
    let outcome = offsite::run(host, request);
    let released = workload.finish().map_err(|_| Failure::Os);
    let line = outcome?;
    released?;
    Ok(line)
}

/// Run `backup nightly|cleanup` or `backup offsite upload|restore|recover`
/// on the prepared host. A syntax error returns 2 before any effect; every
/// other failure is one fixed line.
pub fn run(arguments: &[OsString]) -> ExitCode {
    let offsite = match arguments.split_first() {
        Some((family, rest)) if family == "offsite" => match parse_offsite(rest) {
            Some(request) => Some(request),
            None => return ExitCode::from(2),
        },
        _ => None,
    };
    let nightly = if offsite.is_none() {
        parse(arguments)
    } else {
        None
    };
    if offsite.is_none() && nightly.is_none() {
        return ExitCode::from(2);
    }
    // Everything a backup creates is private to root.
    rustix::process::umask(rustix::fs::Mode::from_raw_mode(0o077));
    let stop = Arc::new(AtomicBool::new(false));
    let handled = [signal_hook::consts::SIGTERM, signal_hook::consts::SIGINT]
        .iter()
        .all(|signal| signal_hook::flag::register(*signal, Arc::clone(&stop)).is_ok());
    let (refusal, failed) = match offsite {
        Some(_) => ("Offsite backup interrupted", OFFSITE_FAILED),
        None => ("Nightly backup interrupted", FAILED),
    };
    let commands = Interruptible {
        stop: &stop,
        refusal,
    };
    let clock = System::new();
    let host = Host {
        commands: &commands,
        clock: &clock,
        paths: Paths::fixed(),
        owner: 0,
    };
    let privileged = rustix::process::geteuid().is_root();
    let outcome = match (handled, &offsite, nightly) {
        (true, Some(request), _) => {
            let line = execute_offsite(&host, request, privileged, Workload::acquire_for);
            line.map(|line| Some(line.to_owned()))
        }
        (true, None, Some(request)) => execute(&host, request, privileged, Workload::acquire_for),
        _ => Err(Failure::Os),
    };
    let interrupted = stop.load(Ordering::SeqCst);
    let printed = !interrupted
        && outcome.is_ok_and(|line| line.is_none_or(|line| emit(std::io::stdout(), &line)));
    if printed {
        return ExitCode::SUCCESS;
    }
    let _ = emit(std::io::stderr(), failed);
    ExitCode::FAILURE
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::command::Request as Asked;
    use crate::host::command::tests::Scripted;
    use crate::host::replay::{build, concrete, expected, held_lock, listed, normalized};
    use crate::host::state::tests::Scratch;
    use crate::host::tests::{Fixed, paths};
    use serde_json::Value;
    use std::collections::BTreeMap;
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    use std::path::Path;
    use std::time::Duration;

    /// Fifty-six recorded invocations of the Python helper: each one is admitted
    /// or refused as recorded, issues the recorded commands with their
    /// bounds, and leaves the recorded archives, receipts, journal and lock.
    #[test]
    fn every_recorded_invocation_leaves_the_recorded_archives_and_journal() {
        let recorded: Value =
            serde_json::from_str(include_str!("backup/recorded_nightly.json")).unwrap();
        #[expect(clippy::cast_possible_truncation, reason = "a whole number of seconds")]
        let epoch = recorded["epoch"].as_f64().unwrap() as i64;
        let scenarios = recorded["scenarios"].as_array().unwrap();
        assert_eq!(scenarios.len(), 56);
        let uid = rustix::process::geteuid().as_raw();
        for scenario in scenarios {
            let name = scenario["name"].as_str().unwrap();
            let arguments = scenario["arguments"].as_array().unwrap();
            let arguments: Vec<OsString> = arguments
                .iter()
                .map(|word| word.as_str().unwrap().into())
                .collect();
            let status = scenario["status"].as_i64().unwrap();
            let Some(request) = parse(&arguments) else {
                assert_eq!(status, 2, "{name}");
                continue;
            };
            assert_ne!(status, 2, "{name}");
            let scratch = Scratch::new();
            let base = scratch.path();
            build(base, scenario["before"].as_object().unwrap(), epoch);
            std::fs::write(
                base.join("boot_id"),
                format!("{}\n", scenario["boot"].as_str().unwrap()),
            )
            .unwrap();
            let world = &scenario["world"];
            let commands = Scripted::new(|asked: &Asked<'_>| {
                let argv = &asked.argv[3..];
                let has = |word: &str| argv.iter().any(|argument| argument == word);
                let step = match argv[0].as_str() {
                    "ps" => "ps",
                    "inspect" => "inspect",
                    _ if has("pg_dump") => "dump",
                    _ if has("pg_restore") => "list",
                    _ if argv
                        .iter()
                        .any(|argument| argument.contains("pg_database_size")) =>
                    {
                        "size"
                    }
                    _ => "ledger",
                };
                // As on a host, both files exist from before the command starts.
                let private = |path: &Path| {
                    let mut options = std::fs::OpenOptions::new();
                    options
                        .write(true)
                        .create_new(true)
                        .mode(0o600)
                        .open(path)
                        .unwrap()
                };
                let mut output = private(asked.archive.unwrap());
                private(asked.errors.unwrap());
                let ledger = format!("1 t {}\n21 t {}\n", "c".repeat(96), "d".repeat(96));
                let said = match step {
                    "dump" => world["dump"]
                        .as_str()
                        .unwrap_or("PGDMP-fixture archive")
                        .to_owned(),
                    "list" => String::new(),
                    "ps" => world["ps"]
                        .as_str()
                        .map_or_else(|| format!("{}\n", "a".repeat(64)), str::to_owned),
                    "size" => world["size"].as_str().unwrap_or("123456789\n").to_owned(),
                    "inspect" => world["inspect"]
                        .as_str()
                        .map_or_else(|| format!("sha256:{}\n", "b".repeat(64)), str::to_owned),
                    _ => world["ledger"].as_str().map_or(ledger, str::to_owned),
                };
                if step == "dump" || world["fail"] != step {
                    output.write_all(said.as_bytes()).unwrap();
                }
                match (world["fail"] == step, world["failure"] == "timeout") {
                    (true, true) => Err(Failure::Timeout),
                    (true, false) => Err(Failure::Command),
                    (false, _) => Ok(Vec::new()),
                }
            });
            commands.free.set(scenario["free"].as_u64());
            let clock = Fixed::at(recorded["now"].as_str().unwrap());
            clock.monotonic.set(Duration::from_secs_f64(
                scenario["monotonic"].as_f64().unwrap(),
            ));
            let host = Host {
                commands: &commands,
                clock: &clock,
                paths: Paths {
                    root: base.join("srv/simplestchat-public"),
                    work: base.join("run/simplestchat-bench"),
                    boot: base.join("boot_id"),
                    ..paths(base)
                },
                owner: uid,
            };
            let holder = (scenario["busy"] == true).then(|| held_lock(base));
            let privileged = scenario["euid"] == 0;
            let outcome = execute(&host, request, privileged, |takeover| {
                Workload::fixture_for(base, uid, takeover)
            });
            drop(holder);
            match &outcome {
                Ok(line) => {
                    assert_eq!(status, 0, "{name}");
                    assert_eq!(
                        line.as_deref().unwrap_or_default(),
                        scenario["stdout"].as_str().unwrap(),
                        "{name}"
                    );
                }
                Err(_) => {
                    assert_eq!(status, 1, "{name}: {outcome:?}");
                    assert_eq!(scenario["stderr"].as_str().unwrap(), FAILED, "{name}");
                }
            }
            // This run's own command output is evidence the recorder's
            // scripted runner never wrote.
            let mut left = listed(base);
            left.retain(|path, _| {
                let file = path.rsplit('/').next().unwrap();
                let numbered =
                    file.len() == 10 && (file.ends_with(".stdout") || file.ends_with(".stderr"));
                !(numbered && path.contains("/20261111T040007Z.logs/"))
            });
            assert_eq!(
                left,
                expected(scenario["after"].as_object().unwrap()),
                "{name}"
            );
            let asked = commands.asked.borrow();
            let calls = scenario["calls"].as_array().unwrap();
            assert_eq!(asked.len(), calls.len(), "{name}");
            for (number, (ours, theirs)) in asked.iter().zip(calls).enumerate() {
                assert_eq!(
                    ours.argv[..3],
                    crate::host::DOCKER.map(str::to_owned),
                    "{name}"
                );
                let argv: Vec<&str> = theirs["argv"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|word| word.as_str().unwrap())
                    .collect();
                assert_eq!(ours.argv[3..], argv, "{name}");
                assert!(
                    (ours.seconds - theirs["timeout"].as_f64().unwrap()).abs() < f64::EPSILON,
                    "{name}"
                );
                let archive = ours.archive.as_deref().unwrap();
                let file = |path: &Path| path.file_name().unwrap().to_str().unwrap().to_owned();
                match theirs["output"].as_str() {
                    Some(output) => {
                        assert_eq!(file(archive), output, "{name}");
                        let free = scenario["free"].as_u64().unwrap();
                        let budget = (free - 1024 * 1024 * 1024).min(4 * 1024 * 1024 * 1024);
                        assert_eq!(ours.ceiling, Some(budget), "{name}");
                    }
                    None => {
                        assert_eq!(file(archive), format!("{:03}.stdout", number + 1), "{name}");
                        assert_eq!(
                            file(archive.parent().unwrap()),
                            theirs["attempt"].as_str().unwrap(),
                            "{name}"
                        );
                        assert_eq!(ours.ceiling, Some(2 * 1024 * 1024), "{name}");
                    }
                }
                let source = ours.source.as_deref().map(file);
                assert_eq!(source.as_deref(), theirs["input"].as_str(), "{name}");
                assert!(ours.input.is_none(), "{name}");
            }
        }
    }

    /// A long body the recorder spelled as a repeat, or a literal one.
    fn body(spelled: &Value) -> String {
        match spelled.as_str() {
            Some(text) => text.to_owned(),
            None => spelled["repeat"]
                .as_str()
                .unwrap()
                .repeat(usize::try_from(spelled["count"].as_u64().unwrap()).unwrap()),
        }
    }

    /// One hundred and twenty-five recorded invocations of the Python
    /// offsite helper, replayed request by request: each command the Rust
    /// issues must be the recorded one, and receives the recorded answer.
    /// Each invocation is admitted or refused as recorded and leaves the
    /// recorded settings, selection, evidence, downloads and journal.
    #[test]
    fn every_recorded_offsite_invocation_is_replayed_request_by_request() {
        let recorded: Value =
            serde_json::from_str(include_str!("backup/recorded_offsite.json")).unwrap();
        #[expect(clippy::cast_possible_truncation, reason = "a whole number of seconds")]
        let epoch = recorded["epoch"].as_f64().unwrap() as i64;
        let scenarios = recorded["scenarios"].as_array().unwrap();
        assert_eq!(scenarios.len(), 125);
        let uid = rustix::process::geteuid().as_raw();
        let statements = Path::new(env!("CARGO_MANIFEST_DIR")).join("ansible/files");
        let restic: Vec<&str> = recorded["restic"]
            .as_array()
            .unwrap()
            .iter()
            .map(|word| word.as_str().unwrap())
            .collect();
        for scenario in scenarios {
            let name = scenario["name"].as_str().unwrap();
            let arguments = scenario["arguments"].as_array().unwrap();
            let arguments: Vec<OsString> = arguments
                .iter()
                .map(|word| word.as_str().unwrap().into())
                .collect();
            let status = scenario["status"].as_i64().unwrap();
            let Some(request) = parse_offsite(&arguments) else {
                assert_eq!(status, 2, "{name}");
                continue;
            };
            assert_ne!(status, 2, "{name}");
            let scratch = Scratch::new();
            let base = scratch.path();
            let before =
                &recorded["trees"][usize::try_from(scenario["before"].as_u64().unwrap()).unwrap()];
            let before = before.as_object().unwrap();
            // The recorded tree with its placeholders made concrete.
            let mut started = serde_json::Map::new();
            for (path, entry) in before {
                let mut entry = entry.clone();
                for key in ["text", "link"] {
                    if let Some(text) = entry[key].as_str() {
                        entry[key] = Value::String(concrete(text, base));
                    }
                }
                started.insert(concrete(path, base), entry);
            }
            build(base, &started, epoch);
            let calls = scenario["calls"].as_array().unwrap();
            let position = std::cell::Cell::new(0_usize);
            let container = std::cell::RefCell::new("1".repeat(32));
            let commands = Scripted::new(|asked: &Asked<'_>| {
                let call = calls
                    .get(position.get())
                    .unwrap_or_else(|| panic!("{name}: an unrecorded command {:?}", asked.argv));
                position.set(position.get() + 1);
                if let Some(at) = asked.argv.iter().position(|word| word == "create") {
                    let unique = asked.argv[at + 2].strip_prefix("scpub-restore-").unwrap();
                    container.replace(unique.to_owned());
                }
                // As on a host, both files exist from before the command starts.
                let private = |path: &Path| {
                    let mut options = std::fs::OpenOptions::new();
                    options
                        .write(true)
                        .create_new(true)
                        .mode(0o600)
                        .open(path)
                        .unwrap()
                };
                let mut output = private(asked.archive.unwrap());
                private(asked.errors.unwrap());
                let said = match &call["written"] {
                    Value::Null => call["answer"].as_str().unwrap_or_default().to_owned(),
                    spelled => body(spelled),
                };
                let said = said
                    .replace("<id>", &container.borrow())
                    .replace("<base>", base.to_str().unwrap());
                let most = usize::try_from(asked.ceiling.unwrap())
                    .unwrap()
                    .min(said.len());
                output.write_all(&said.as_bytes()[..most]).unwrap();
                match call["raises"].as_str() {
                    Some("timeout") => Err(Failure::Timeout),
                    Some("command") => Err(Failure::Command),
                    Some("limit") => Err(Failure::Refused("command_stdout_limit_exceeded")),
                    _ => Ok(Vec::new()),
                }
            });
            commands.free.set(scenario["free"].as_u64());
            let clock = Fixed::at(recorded["now"].as_str().unwrap());
            clock.tick.set(Duration::from_millis(250));
            let host = Host {
                commands: &commands,
                clock: &clock,
                paths: Paths {
                    root: base.join("srv/simplestchat-public"),
                    work: base.join("run/simplestchat-bench"),
                    offsite: base.join("offsite"),
                    state: base.join("state"),
                    sql: statements.clone(),
                    ..paths(base)
                },
                owner: uid,
            };
            let holder = (scenario["busy"] == true).then(|| held_lock(base));
            let privileged = scenario["euid"] == 0;
            let outcome = execute_offsite(&host, &request, privileged, |takeover| {
                Workload::fixture_for(base, uid, takeover)
            });
            drop(holder);
            match &outcome {
                Ok(line) => {
                    assert_eq!(status, 0, "{name}");
                    assert_eq!(*line, scenario["stdout"].as_str().unwrap(), "{name}");
                }
                Err(_) => {
                    assert_eq!(status, 1, "{name}: {outcome:?}");
                    assert_eq!(
                        scenario["stderr"].as_str().unwrap(),
                        OFFSITE_FAILED,
                        "{name}"
                    );
                }
            }
            // The recorded end tree: the start tree with what changed.
            let mut after = before.clone();
            for removed in scenario["removed"].as_array().unwrap() {
                after.remove(removed.as_str().unwrap());
            }
            for (path, entry) in scenario["changed"].as_object().unwrap() {
                after.insert(path.clone(), entry.clone());
            }
            let mut left = BTreeMap::new();
            for (path, (mode, content)) in listed(base) {
                // Numbered command output is evidence the recorder's scripted
                // runner never wrote.
                let file = path.rsplit('/').next().unwrap();
                let numbered =
                    file.len() == 10 && (file.ends_with(".stdout") || file.ends_with(".stderr"));
                if !numbered {
                    left.insert(normalized(&path, base), (mode, normalized(&content, base)));
                }
            }
            assert_eq!(left, expected(&after), "{name}");
            let asked = commands.asked.borrow();
            assert_eq!(asked.len(), calls.len(), "{name}");
            let relative =
                |path: &Path| normalized(path.strip_prefix(base).unwrap().to_str().unwrap(), base);
            for (ours, theirs) in asked.iter().zip(calls) {
                let line = &recorded["commandLines"]
                    [usize::try_from(theirs["argv"].as_u64().unwrap()).unwrap()];
                let mut line: Vec<&str> = line
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|word| word.as_str().unwrap())
                    .collect();
                if line[0] == "<restic>" {
                    line.splice(0..1, restic.iter().copied());
                }
                let said: Vec<String> = ours
                    .argv
                    .iter()
                    .map(|word| normalized(word, base))
                    .collect();
                let said = if theirs["kind"] == "docker" {
                    assert_eq!(
                        ours.argv[..3],
                        crate::host::DOCKER.map(str::to_owned),
                        "{name}"
                    );
                    &said[3..]
                } else {
                    &said[..]
                };
                assert_eq!(said, line, "{name}");
                assert!(
                    (ours.seconds - theirs["timeout"].as_f64().unwrap()).abs() < f64::EPSILON,
                    "{name}"
                );
                let archive = ours.archive.as_deref().unwrap();
                match theirs["output"].as_str() {
                    Some(output) => {
                        assert_eq!(relative(archive), output, "{name}");
                        let free = scenario["free"].as_u64().unwrap();
                        let limit = theirs["limit"].as_u64().unwrap();
                        assert_eq!(
                            ours.ceiling,
                            Some(limit.min(free - 1024 * 1024 * 1024)),
                            "{name}"
                        );
                    }
                    None => {
                        let attempt = relative(archive.parent().unwrap());
                        assert_eq!(attempt, theirs["attempt"].as_str().unwrap(), "{name}");
                        assert_eq!(ours.ceiling, Some(2 * 1024 * 1024), "{name}");
                    }
                }
                let source = ours.source.as_deref().map(relative);
                assert_eq!(source.as_deref(), theirs["input"].as_str(), "{name}");
                let typed = theirs["data"].as_u64().map(|index| {
                    recorded["inputs"][usize::try_from(index).unwrap()]
                        .as_str()
                        .unwrap()
                        .as_bytes()
                });
                assert_eq!(ours.input.as_deref(), typed, "{name}");
            }
        }
    }
}
