//! Backups of the deployment's database on the prepared host. Importing this
//! module starts nothing. Every command works on fixed local paths, the
//! local Docker daemon and the deployment's own database, under the shared
//! workload lock, and takes no host, statement or path argument.

mod nightly;
pub(crate) mod receipts;

use crate::host::command::Interruptible;
use crate::host::failure::{Failure, Result, require};
use crate::host::{Host, Paths, System, emit};
use crate::release::assets::{self, Takeover, Workload};
use nightly::{Action, Request};
use std::ffi::OsString;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

const FAILED: &str = "Nightly backup failed; inspect the private backup logs and journals.\n";

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
    let boot = nightly::boot(host)?;
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

/// Run `backup nightly|cleanup` on the prepared host. A syntax error returns
/// 2 before any effect; every other failure is one fixed line.
pub fn run(arguments: &[OsString]) -> ExitCode {
    let Some(request) = parse(arguments) else {
        return ExitCode::from(2);
    };
    // Everything a backup creates is private to root.
    rustix::process::umask(rustix::fs::Mode::from_raw_mode(0o077));
    let stop = Arc::new(AtomicBool::new(false));
    let handled = [signal_hook::consts::SIGTERM, signal_hook::consts::SIGINT]
        .iter()
        .all(|signal| signal_hook::flag::register(*signal, Arc::clone(&stop)).is_ok());
    let commands = Interruptible {
        stop: &stop,
        refusal: "Nightly backup interrupted",
    };
    let clock = System::new();
    let host = Host {
        commands: &commands,
        clock: &clock,
        paths: Paths::fixed(),
        owner: 0,
    };
    let privileged = rustix::process::geteuid().is_root();
    let outcome = if handled {
        execute(&host, request, privileged, Workload::acquire_for)
    } else {
        Err(Failure::Os)
    };
    let interrupted = stop.load(Ordering::SeqCst);
    let printed = !interrupted
        && outcome.is_ok_and(|line| line.is_none_or(|line| emit(std::io::stdout(), &line)));
    if printed {
        return ExitCode::SUCCESS;
    }
    let _ = emit(std::io::stderr(), FAILED);
    ExitCode::FAILURE
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::command::Request as Asked;
    use crate::host::command::tests::Scripted;
    use crate::host::state::tests::Scratch;
    use crate::host::tests::{Fixed, paths};
    use serde_json::Value;
    use std::collections::BTreeMap;
    use std::io::Write;
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    const TOPS: [(&str, &str); 3] = [
        ("srv", "srv/simplestchat-public"),
        ("etc", "etc/simplestchat-public"),
        ("run", "run/simplestchat-bench"),
    ];

    /// Where a recorded path lives under the scratch root: the recorder's
    /// three directories are the fixed ones the workload lock owns.
    fn placed(base: &Path, recorded: &str) -> PathBuf {
        let (top, rest) = recorded.split_once('/').unwrap_or((recorded, ""));
        let fixed = TOPS.iter().find(|(name, _)| *name == top).unwrap().1;
        base.join(fixed).join(rest)
    }

    fn chmod(path: &Path, mode: u32) {
        std::fs::set_permissions(path, PermissionsExt::from_mode(mode)).unwrap();
    }

    fn mode(entry: &Value) -> u32 {
        let text = entry["mode"].as_str().unwrap().strip_prefix("0o").unwrap();
        u32::from_str_radix(text, 8).unwrap()
    }

    /// Build the recorded tree, then give every entry its recorded age.
    fn build(base: &Path, tree: &serde_json::Map<String, Value>, epoch: i64) {
        chmod(base, 0o700);
        for (top, _) in TOPS {
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(base.join(top))
                .unwrap();
        }
        for (name, entry) in tree {
            let path = placed(base, name);
            if let Some(link) = entry["link"].as_str() {
                std::os::unix::fs::symlink(placed(base, link), &path).unwrap();
            } else if entry["directory"] == true {
                std::fs::DirBuilder::new()
                    .mode(0o700)
                    .create(&path)
                    .unwrap();
            } else {
                std::fs::write(&path, entry["text"].as_str().unwrap()).unwrap();
            }
        }
        for (name, entry) in tree.iter().rev() {
            let path = placed(base, name);
            if entry["link"].is_string() {
                continue;
            }
            chmod(&path, mode(entry));
            let stamp = rustix::fs::Timespec {
                tv_sec: epoch + entry["mtime"].as_i64().unwrap(),
                tv_nsec: 0,
            };
            let times = rustix::fs::Timestamps {
                last_access: stamp,
                last_modification: stamp,
            };
            rustix::fs::utimensat(rustix::fs::CWD, &path, &times, rustix::fs::AtFlags::empty())
                .unwrap();
        }
    }

    /// The tree as the recorder listed it: recorded name to kind and content.
    fn listed(base: &Path) -> BTreeMap<String, (u32, String)> {
        fn walk(base: &Path, at: &Path, name: &str, found: &mut BTreeMap<String, (u32, String)>) {
            let metadata = std::fs::symlink_metadata(at).unwrap();
            let content = if metadata.is_symlink() {
                let target = std::fs::read_link(at).unwrap();
                let (top, fixed) = TOPS
                    .iter()
                    .find(|(_, fixed)| target.starts_with(base.join(fixed)))
                    .unwrap();
                let rest = target.strip_prefix(base.join(fixed)).unwrap();
                format!("link:{top}/{}", rest.display())
            } else if metadata.is_dir() {
                "directory".to_owned()
            } else {
                format!("text:{}", std::fs::read_to_string(at).unwrap())
            };
            // A link's own mode means nothing and follows the creator's mask.
            let mode = if metadata.is_symlink() {
                0
            } else {
                metadata.mode() & 0o7777
            };
            found.insert(name.to_owned(), (mode, content));
            if metadata.is_dir() {
                for entry in std::fs::read_dir(at).unwrap() {
                    let entry = entry.unwrap();
                    let child = format!("{name}/{}", entry.file_name().to_str().unwrap());
                    walk(base, &entry.path(), &child, found);
                }
            }
        }
        let mut found = BTreeMap::new();
        for (top, fixed) in TOPS {
            if base.join(fixed).exists() {
                walk(base, &base.join(fixed), top, &mut found);
            }
        }
        found
    }

    fn expected(tree: &serde_json::Map<String, Value>) -> BTreeMap<String, (u32, String)> {
        let entries = tree.iter().map(|(name, entry)| {
            let content = if let Some(link) = entry["link"].as_str() {
                format!("link:{link}")
            } else if entry["directory"] == true {
                "directory".to_owned()
            } else {
                format!("text:{}", entry["text"].as_str().unwrap())
            };
            let mode = if entry["link"].is_string() {
                0
            } else {
                mode(entry)
            };
            (name.clone(), (mode, content))
        });
        entries.collect()
    }

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
            // Another operation holds the lock it created.
            let holder = (scenario["busy"] == true).then(|| {
                let work = base.join("run/simplestchat-bench");
                std::fs::DirBuilder::new()
                    .mode(0o700)
                    .create(&work)
                    .unwrap();
                let mut options = std::fs::OpenOptions::new();
                options.write(true).create_new(true).mode(0o600);
                let lock = options.open(work.join("workload.lock")).unwrap();
                let exclusive = rustix::fs::FlockOperation::NonBlockingLockExclusive;
                rustix::fs::flock(&lock, exclusive).unwrap();
                lock
            });
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
}
