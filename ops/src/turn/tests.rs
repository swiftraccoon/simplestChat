use super::*;
use crate::host::command::Request as Asked;
use crate::host::command::tests::Scripted;
use crate::host::failure::Failure;
use crate::host::hexadecimal;
use crate::host::replay::{
    build, chmod, concrete, content, expected, held_lock, listed, normalized, placed, target,
};
use crate::host::state::tests::Scratch;
use crate::host::tests::{Fixed, paths};
use serde_json::Value;
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The recorder's names for the two pairs of key material. A scratch host
/// holds each name as the file's whole content: nothing here parses a
/// certificate, the system's tool does.
const PAIRS: [u8; 2] = *b"AB";
/// Directories whose names are random on a host, by their fixed beginning.
const RANDOM: [&str; 2] = ["turn.", ".certificate-"];

pub(crate) fn index(value: &Value) -> usize {
    usize::try_from(value.as_u64().unwrap()).unwrap()
}

/// The pair a certificate or key file is of, when it is one at all.
pub(crate) fn pair(content: &[u8], kind: &str) -> Option<u8> {
    let named = PAIRS.into_iter();
    let mut named =
        named.filter(|pair| content == format!("<{kind} {}>", char::from(*pair)).as_bytes());
    named.next()
}

/// One scenario's scratch host and the random names met on it so far.
pub(crate) struct Run<'a> {
    pub base: &'a Path,
    /// Each random name with the recorder's name for it, in order of
    /// appearance.
    pub met: RefCell<Vec<(String, String)>>,
    pub random: &'a [&'a str],
}

impl Run<'_> {
    /// The published generation of each pair on this host.
    fn generations() -> [(String, String); 2] {
        PAIRS.map(|pair| {
            let letter = char::from(pair);
            let held = format!("<certificate {letter}>");
            (digest(held.as_bytes()), format!("<generation {letter}>"))
        })
    }

    /// Give every random name in `text` that was not met before the
    /// recorder's next name of its kind.
    fn meet(&self, text: &str) {
        for prefix in self.random {
            let mut rest = text;
            while let Some(at) = rest.find(prefix) {
                rest = &rest[at + prefix.len()..];
                let Some(unique) = rest.get(..8).filter(|unique| hexadecimal(unique, 8)) else {
                    continue;
                };
                let actual = format!("{prefix}{unique}");
                let mut met = self.met.borrow_mut();
                if met.iter().all(|(known, _)| *known != actual) {
                    let earlier = met.iter().filter(|(known, _)| known.starts_with(prefix));
                    let name = match earlier.count() {
                        0 => format!("{prefix}<name>"),
                        earlier => format!("{prefix}<name {}>", earlier + 1),
                    };
                    met.push((actual, name));
                }
            }
        }
    }

    /// This run's text under the recorder's placeholders.
    pub(crate) fn recorded(&self, text: &str) -> String {
        self.meet(text);
        let mut text = text.to_owned();
        for (actual, name) in self.met.borrow().iter() {
            text = text.replace(actual, name);
        }
        for (digest, name) in Self::generations() {
            text = text.replace(&digest, &name);
        }
        normalized(&text, self.base)
    }

    /// A recorded text with its placeholders made this run's.
    pub(crate) fn concrete(&self, text: &str) -> String {
        let mut text = text.to_owned();
        for (actual, name) in self.met.borrow().iter() {
            text = text.replace(name, actual);
        }
        for (digest, name) in Self::generations() {
            text = text.replace(&name, &digest);
        }
        concrete(&text, self.base)
    }

    pub(crate) fn path(&self, recorded: &str) -> PathBuf {
        let mut recorded = recorded.to_owned();
        for (digest, name) in Self::generations() {
            recorded = recorded.replace(&name, &digest);
        }
        placed(self.base, &recorded)
    }

    fn remove(path: &Path) {
        match std::fs::symlink_metadata(path) {
            Ok(found) if found.is_dir() => std::fs::remove_dir_all(path).unwrap(),
            Ok(_) => std::fs::remove_file(path).unwrap(),
            Err(_) => {}
        }
    }

    /// Change the host as the recorder did between two invocations.
    pub(crate) fn alter(&self, before: &Value, epoch: i64) {
        let removed = before["removed"].as_array().unwrap();
        for name in removed.iter().rev() {
            Self::remove(&self.path(name.as_str().unwrap()));
        }
        for (name, entry) in before["changed"].as_object().unwrap() {
            let path = self.path(name);
            let found = std::fs::symlink_metadata(&path).ok();
            let kept = entry["directory"] == true && found.is_some_and(|found| found.is_dir());
            if !kept {
                Self::remove(&path);
            }
            if let Some(link) = entry["link"].as_str() {
                let link = self.concrete(link);
                std::os::unix::fs::symlink(target(self.base, &link), &path).unwrap();
                continue;
            }
            if entry["directory"] == true {
                if !kept {
                    let mut parents = std::fs::DirBuilder::new();
                    parents.recursive(true).mode(0o700).create(&path).unwrap();
                }
            } else {
                std::fs::write(&path, content(entry)).unwrap();
            }
            let mode = entry["mode"].as_str().unwrap().strip_prefix("0o").unwrap();
            chmod(&path, u32::from_str_radix(mode, 8).unwrap());
            if let Some(age) = entry["mtime"].as_i64() {
                let stamp = rustix::fs::Timespec {
                    tv_sec: epoch + age,
                    tv_nsec: 0,
                };
                let times = rustix::fs::Timestamps {
                    last_access: stamp,
                    last_modification: stamp,
                };
                let follow = rustix::fs::AtFlags::empty();
                rustix::fs::utimensat(rustix::fs::CWD, &path, &times, follow).unwrap();
            }
        }
    }

    /// The host's tree as the recorder listed it, without the numbered
    /// output of commands, which its scripted runner never wrote.
    pub(crate) fn listed(&self) -> BTreeMap<String, (u32, String)> {
        let mut left = BTreeMap::new();
        for (path, (mode, content)) in listed(self.base) {
            let file = path.rsplit('/').next().unwrap();
            let numbered =
                file.len() == 10 && (file.ends_with(".stdout") || file.ends_with(".stderr"));
            if !numbered {
                left.insert(self.recorded(&path), (mode, self.recorded(&content)));
            }
        }
        left
    }
}

/// Two groups the caller belongs to besides its own, when it has them, so
/// that a file given to a group shows it.
pub(crate) fn groups() -> (u32, u32) {
    let own = rustix::process::getegid().as_raw();
    let held = rustix::process::getgroups().unwrap();
    let others = held.into_iter().map(rustix::process::Gid::as_raw);
    let mut others = others.filter(|group| *group != own);
    let first = others.next().unwrap_or(own);
    (first, others.next().unwrap_or(first))
}

/// Require every path the recorded helper gave to a group to belong to
/// `group` now, where it still exists.
pub(crate) fn given(run: &Run<'_>, paths: &Value, group: u32, context: &str) {
    use std::os::unix::fs::MetadataExt;
    for path in paths.as_array().unwrap() {
        let path = run.path(path.as_str().unwrap());
        if let Ok(found) = std::fs::symlink_metadata(&path) {
            assert_eq!(found.gid(), group, "{context}: {path:?}");
        }
    }
}

/// The recorded tree after one more set of changes.
pub(crate) fn changed(
    tree: &mut serde_json::Map<String, Value>,
    changes: &Value,
    removals: &Value,
) {
    for removed in removals.as_array().unwrap() {
        tree.remove(removed.as_str().unwrap());
    }
    for (path, entry) in changes.as_object().unwrap() {
        tree.insert(path.clone(), entry.clone());
    }
}

/// A new private file, as a command's output is on a host.
pub(crate) fn private(path: &Path) -> std::fs::File {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true).mode(0o600);
    options.open(path).unwrap()
}

/// What a scripted command answers.
pub(crate) type Answer = std::result::Result<Vec<u8>, Failure>;

/// The answer to a command the Python helper had no counterpart for,
/// because it parsed key material and spoke TLS in its own process; nothing
/// for a command it issued.
pub(crate) fn unrecorded(
    asked: &Asked<'_>,
    handshake: impl FnOnce(&[&str]) -> Option<String>,
) -> Option<Answer> {
    let argv: Vec<&str> = asked.argv.iter().map(String::as_str).collect();
    let key = |kind: &str, path: &str| {
        let held = std::fs::read(path).unwrap();
        let pair = pair(&held, kind).ok_or(Failure::Command)?;
        Ok(format!("public key {}", char::from(pair)).into_bytes())
    };
    match argv.as_slice() {
        ["/usr/bin/openssl", "x509", "-in", path, "-noout", "-pubkey"] => {
            Some(key("certificate", path))
        }
        ["/usr/bin/openssl", "pkey", "-in", path, "-pubout"] => Some(key("key", path)),
        ["/usr/bin/openssl", "s_client", rest @ ..] => {
            assert!((asked.seconds - 3.0).abs() < f64::EPSILON && asked.input == Some(&b""[..]));
            Some(
                handshake(rest)
                    .map(String::into_bytes)
                    .ok_or(Failure::Command),
            )
        }
        [
            "/usr/bin/openssl",
            "x509",
            "-noout",
            "-fingerprint",
            "-sha256",
            source @ ..,
        ] => {
            let subject = match source {
                ["-in", path] => std::fs::read(path).unwrap(),
                [] => asked.input.unwrap().to_vec(),
                other => panic!("{other:?}"),
            };
            let known = pair(&subject, "certificate").ok_or(Failure::Command);
            Some(known.map(|pair| format!("fingerprint {}", char::from(pair)).into_bytes()))
        }
        _ => None,
    }
}

/// What a replayed recording came to.
#[derive(Debug, Default, PartialEq, Eq)]
struct Replayed {
    scenarios: usize,
    invocations: usize,
    /// Restarts of the application the helper asked for.
    restarts: usize,
    /// Restarts the recorder's host refused although its environment
    /// admitted the relay's settings.
    failed: usize,
    /// Restarts refused because a relay setting was already there.
    configured: usize,
}

/// Whether nothing holds the workload lock of the scratch host.
fn unlocked(base: &Path) -> bool {
    let lock = std::fs::File::open(base.join("run/simplestchat-bench/workload.lock")).unwrap();
    let exclusive = rustix::fs::FlockOperation::NonBlockingLockExclusive;
    rustix::fs::flock(&lock, exclusive).is_ok()
}

/// Replay every scenario of a recording of the Python helper request by
/// request: every command the Rust issues must be the recorded one with its
/// deadline and its place in the evidence, and receives the recorded
/// answer; every TLS attempt must go where the recorded one went and meets
/// the certificate that one met; every restart of the application must be
/// the recorded one, asked for with the workload lock given up and nothing
/// retained yet. Each invocation ends as recorded, prints the recorded
/// lines and failure kind, and leaves the recorded generations, selection,
/// configuration and outcome.
fn replayed(recorded: &Value) -> Replayed {
    let scenarios = recorded["scenarios"].as_array().unwrap();
    let mut tally = Replayed {
        scenarios: scenarios.len(),
        ..Replayed::default()
    };
    let epoch = recorded["epoch"].as_i64().unwrap();
    let uid = rustix::process::geteuid().as_raw();
    let (proxy_group, relay_group) = groups();
    for scenario in scenarios {
        let name = scenario["name"].as_str().unwrap();
        let scratch = Scratch::new();
        let base = scratch.path();
        let mut tree = recorded["trees"][index(&scenario["start"])]
            .as_object()
            .unwrap()
            .clone();
        build(base, &tree, epoch);
        let run = Run {
            base,
            met: RefCell::new(Vec::new()),
            random: &RANDOM,
        };
        for step in scenario["steps"].as_array().unwrap() {
            tally.invocations += 1;
            let words = step["arguments"].as_array().unwrap().iter();
            let words: Vec<OsString> = words.map(|word| word.as_str().unwrap().into()).collect();
            let status = step["status"].as_i64().unwrap();
            run.alter(&step["before"], epoch);
            changed(
                &mut tree,
                &step["before"]["changed"],
                &step["before"]["removed"],
            );
            let Some(request) = parse(&words) else {
                assert_eq!(status, 2, "{name}");
                continue;
            };
            assert_ne!(status, 2, "{name}");
            let context = format!("{name}: {words:?}");
            let calls = step["calls"].as_array().unwrap();
            let attempts = step["tls"].as_array().unwrap();
            let (position, handshakes) = (Cell::new(0_usize), Cell::new(0_usize));
            let evidence: RefCell<Option<PathBuf>> = RefCell::new(None);
            let commands = Scripted::new(|asked: &Asked<'_>| {
                let handshake = |arguments: &[&str]| {
                    let attempt = attempts.get(handshakes.get());
                    let attempt =
                        attempt.unwrap_or_else(|| panic!("{context}: an unrecorded handshake"));
                    handshakes.set(handshakes.get() + 1);
                    let destination = attempt["destination"].as_str().unwrap();
                    let endpoint = match destination.contains(':') {
                        true => format!("[{destination}]:{}", attempt["port"]),
                        false => format!("{destination}:{}", attempt["port"]),
                    };
                    let hostname = "relay.example.test";
                    let expected = [
                        "-connect",
                        &endpoint,
                        "-servername",
                        hostname,
                        "-verify_hostname",
                        hostname,
                        "-verify_return_error",
                        "-no_tls1",
                        "-no_tls1_1",
                    ];
                    assert_eq!(arguments, expected, "{context}");
                    assert!(
                        attempt["sni"].is_null() || attempt["sni"] == hostname,
                        "{context}"
                    );
                    assert_eq!(attempt["timeout"], 3, "{context}");
                    let presented = attempt["presented"].as_str().unwrap();
                    presented.starts_with('<').then(|| presented.to_owned())
                };
                if let Some(answer) = unrecorded(asked, handshake) {
                    assert!(
                        asked.archive.is_none() && asked.errors.is_none(),
                        "{context}"
                    );
                    return answer;
                }
                let call = calls.get(position.get());
                let call = call.unwrap_or_else(|| panic!("{context}: unrecorded {:?}", asked.argv));
                // The recorder changed its host while this command ran.
                if step["during"]["call"] == position.get() {
                    run.alter(&step["during"], epoch);
                }
                position.set(position.get() + 1);
                let line = recorded["commandLines"][index(&call["argv"])]
                    .as_array()
                    .unwrap();
                let line: Vec<&str> = line.iter().map(|word| word.as_str().unwrap()).collect();
                let said: Vec<String> = asked.argv.iter().map(|word| run.recorded(word)).collect();
                assert_eq!(said, line, "{context}");
                let seconds = call["timeout"].as_f64().unwrap();
                assert!((asked.seconds - seconds).abs() < f64::EPSILON, "{context}");
                assert_eq!(asked.ceiling, Some(2 * 1024 * 1024), "{context}");
                assert!(asked.input.is_none() && asked.source.is_none(), "{context}");
                let kept = asked.archive.unwrap();
                *evidence.borrow_mut() = Some(kept.parent().unwrap().to_owned());
                let attempt = kept.parent().unwrap().strip_prefix(base).unwrap();
                let attempt = run.recorded(attempt.to_str().unwrap());
                assert_eq!(attempt, call["attempt"].as_str().unwrap(), "{context}");
                let file = kept.file_name().unwrap().to_str().unwrap();
                assert_eq!(file, format!("{:03}.stdout", position.get()), "{context}");
                // As on a host, both files exist from before the command starts.
                let mut output = private(kept);
                private(asked.errors.unwrap());
                if !call["answer"].is_null() {
                    let answer = recorded["answers"][index(&call["answer"])]
                        .as_str()
                        .unwrap();
                    output.write_all(run.concrete(answer).as_bytes()).unwrap();
                }
                match call["raises"].as_str() {
                    Some("timeout") => Err(Failure::Timeout),
                    Some("command") => Err(Failure::Command),
                    _ => Ok(Vec::new()),
                }
            });
            let clock = Fixed::at(recorded["now"].as_str().unwrap());
            clock.tick.set(Duration::from_millis(250));
            let host = Host {
                commands: &commands,
                clock: &clock,
                paths: Paths {
                    root: base.join("srv/simplestchat-public"),
                    config: base.join("etc/simplestchat-public"),
                    work: base.join("run/simplestchat-bench"),
                    ..paths(base)
                },
                owner: uid,
            };
            let rearmed = Cell::new(0_usize);
            let rearm = || rearmed.set(rearmed.get() + 1);
            let site = Site {
                host: &host,
                proxy: uid,
                proxy_group,
                relay_group,
                rearm: &rearm,
            };
            let holder = (step["busy"] == true).then(|| held_lock(base));
            let privileged = step["euid"] == 0;
            let none = Vec::new();
            let restarts = step["deploys"].as_array().unwrap_or(&none);
            let (restarted, refused) = (Cell::new(0_usize), Cell::new((0_usize, 0_usize)));
            let restart = |replacement: &Replacement| {
                let asked = restarts.get(restarted.get());
                let asked = asked.unwrap_or_else(|| panic!("{context}: an unrecorded restart"));
                restarted.set(restarted.get() + 1);
                assert_eq!(asked["revision"], replacement.revision, "{context}");
                assert_eq!(asked["options"]["quiet_seconds"], 600, "{context}");
                let address = replacement.relay.address.map(|address| address.to_string());
                let address = address.map_or(Value::Null, Value::String);
                assert_eq!(asked["options"]["verify_address"], address, "{context}");
                let settings = replacement.relay.settings().unwrap();
                let told: BTreeMap<&str, &str> = settings
                    .iter()
                    .map(|(name, value)| (*name, value.as_str()))
                    .collect();
                assert_eq!(asked["environment"], serde_json::json!(told), "{context}");
                let selected = std::fs::read(host.paths.config.join("images.json")).unwrap();
                let selected: Value = serde_json::from_slice(&selected).unwrap();
                assert_eq!(asked["selected"], selected, "{context}");
                // The replacement takes the lock for itself, and what the
                // activation retains says how the replacement ended.
                assert!(unlocked(base), "{context}");
                let evidence = evidence.borrow().clone().unwrap();
                assert!(!evidence.join("outcome.json").exists(), "{context}");
                let environment = std::fs::read(host.paths.config.join("app.env")).unwrap();
                let environment = String::from_utf8(environment).unwrap();
                let (failed, configured) = refused.get();
                match replacement.relay.appended(&environment) {
                    Err(refusal) => {
                        refused.set((failed, configured + 1));
                        Err(Fault::Refused(refusal.to_owned()))
                    }
                    Ok(_) if status == 1 => {
                        refused.set((failed + 1, configured));
                        Err(Fault::Refused("Application replacement failed".to_owned()))
                    }
                    Ok(_) => Ok(()),
                }
            };
            let lock = |takeover: Takeover<'_>| Workload::fixture_for(base, uid, takeover);
            let (said, outcome) = execute(&site, &request, privileged, lock, restart);
            drop(holder);
            assert_eq!(restarted.get(), restarts.len(), "{context}");
            tally.restarts += restarted.get();
            tally.failed += refused.get().0;
            tally.configured += refused.get().1;
            let context = format!("{context}: {outcome:?}");
            assert_eq!(i64::from(outcome.is_err()), status, "{context}");
            assert_eq!(
                run.recorded(&said),
                step["stdout"].as_str().unwrap(),
                "{context}"
            );
            let complained = outcome.as_ref().err().map(complaint).unwrap_or_default();
            assert_eq!(complained, step["stderr"].as_str().unwrap(), "{context}");
            assert_eq!(position.get(), calls.len(), "{context}");
            assert_eq!(handshakes.get(), attempts.len(), "{context}");
            changed(&mut tree, &step["changed"], &step["removed"]);
            assert_eq!(run.listed(), expected(&tree), "{context}");
            given(&run, &step["chowns"], relay_group, &context);
            // One request to stop is forgotten exactly when a replacement
            // that was begun is rolled back.
            let begun = step["changed"].as_object().unwrap();
            let begun = begun
                .keys()
                .any(|path| path.ends_with("/before-settings.json"));
            let rolled = begun && outcome.is_err();
            assert_eq!(rearmed.get(), usize::from(rolled), "{context}");
        }
    }
    tally
}

/// One hundred and seventeen recorded scenarios of preparing, refreshing
/// and giving the relay its metrics, none of which restarts the
/// application.
#[test]
fn every_recorded_relay_invocation_is_replayed_request_by_request() {
    let recorded: Value = serde_json::from_str(include_str!("recorded_turn.json")).unwrap();
    let expected = Replayed {
        scenarios: 117,
        invocations: 150,
        ..Replayed::default()
    };
    assert_eq!(replayed(&recorded), expected);
}

/// Fifty-eight recorded activations. An application that already has the
/// relay's settings is only checked, at the address override when one is
/// given; any other is restarted as its selected release with exactly the
/// three settings, unless the selection or its retained manifest is not
/// what it must be; a restart is refused for an environment that already
/// has any relay line. The secret is read again after the relay was
/// verified: five scenarios change it while the relay is told to reload.
#[test]
fn every_recorded_activation_is_replayed_request_by_request() {
    let recorded: Value = serde_json::from_str(include_str!("recorded_activate.json")).unwrap();
    let expected = Replayed {
        scenarios: 58,
        invocations: 115,
        restarts: 15,
        failed: 1,
        configured: 7,
    };
    assert_eq!(replayed(&recorded), expected);
}
