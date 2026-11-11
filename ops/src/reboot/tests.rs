use super::*;
use crate::host::command::Request as Asked;
use crate::host::command::tests::Scripted;
use crate::host::replay::{
    build, chmod, concrete, expected, held_lock, listed, normalized, placed,
};
use crate::host::state::tests::Scratch;
use crate::host::tests::{Fixed, paths};
use serde_json::Value;
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;

/// The recorder's names for the attempts of one scenario, in the order they
/// were made.
const ATTEMPTS: [&str; 2] = ["reboot.<name>", "reboot.<second>"];

fn index(value: &Value) -> usize {
    usize::try_from(value.as_u64().unwrap()).unwrap()
}

fn octal(value: &Value) -> u32 {
    let text = value.as_str().unwrap().strip_prefix("0o").unwrap();
    u32::from_str_radix(text, 8).unwrap()
}

/// One scenario's scratch host and the attempt directories made on it so far.
struct Run<'a> {
    base: &'a Path,
    made: RefCell<Vec<String>>,
}

impl Run<'_> {
    /// Note the attempt directory a step made.
    fn observe(&self) {
        let results = placed(self.base, "srv/results");
        let Ok(entries) = std::fs::read_dir(results) else {
            return;
        };
        let names = entries.map(|entry| entry.unwrap().file_name().into_string().unwrap());
        let mut made = self.made.borrow_mut();
        let fresh = names.filter(|name| name.starts_with("reboot.") && !made.contains(name));
        let fresh: Vec<String> = fresh.collect();
        assert!(fresh.len() <= 1, "{fresh:?}");
        made.extend(fresh);
    }

    /// This run's text under the recorder's placeholders: the scratch root,
    /// the attempts in order, and the two digests that cover the root's path.
    fn recorded(&self, text: &str) -> String {
        let mut text = text.to_owned();
        for (made, placeholder) in self.made.borrow().iter().zip(ATTEMPTS) {
            text = text.replace(made, placeholder);
        }
        let mut text = normalized(&text, self.base);
        for key in ["\"resolvedSha256\": \"", "\"identitySha256\": \""] {
            if let Some(at) = text.find(key) {
                let value = at + key.len()..at + key.len() + 64;
                assert!(hexadecimal(&text[value.clone()], 64), "{text}");
                text.replace_range(value, "<digest>");
            }
        }
        text
    }

    /// A recorded text with its placeholders made this run's.
    fn concrete(&self, text: &str) -> String {
        let mut text = text.to_owned();
        for (made, placeholder) in self.made.borrow().iter().zip(ATTEMPTS) {
            text = text.replace(placeholder, made);
        }
        concrete(&text, self.base)
    }

    fn path(&self, recorded: &str) -> PathBuf {
        let mut recorded = recorded.to_owned();
        for (made, placeholder) in self.made.borrow().iter().zip(ATTEMPTS) {
            recorded = recorded.replace(placeholder, made);
        }
        placed(self.base, &recorded)
    }

    /// Make one recorded change to the host between two invocations, and
    /// return the lock when the change is another operation holding it.
    fn edit(&self, edit: &Value) -> Option<std::fs::File> {
        if edit["hold"] == true {
            return Some(held_lock(self.base));
        }
        if edit["rehash"] == true {
            // The journal comes to name the identity record's present digest.
            let identity = self.path("srv/results/reboot.<name>/identity.json");
            let state = self.path("srv/release-state.json");
            let text = std::fs::read_to_string(&state).unwrap();
            let key = "\"identitySha256\": \"";
            let at = text.find(key).unwrap() + key.len();
            let mut text = text;
            text.replace_range(at..at + 64, &digest(&std::fs::read(identity).unwrap()));
            std::fs::write(&state, text).unwrap();
            return None;
        }
        let path = self.path(edit["path"].as_str().unwrap());
        if !edit["chmod"].is_null() {
            chmod(&path, octal(&edit["chmod"]));
        } else if let Some(old) = edit["old"].as_str() {
            let text = std::fs::read_to_string(&path).unwrap();
            assert_eq!(text.matches(old).count(), 1, "{old}");
            std::fs::write(&path, text.replace(old, edit["new"].as_str().unwrap())).unwrap();
        } else {
            let mut parents = std::fs::DirBuilder::new();
            parents.recursive(true).mode(0o700);
            parents.create(path.parent().unwrap()).unwrap();
            std::fs::write(&path, self.concrete(edit["text"].as_str().unwrap())).unwrap();
            chmod(&path, octal(&edit["mode"]));
        }
        None
    }
}

/// Whether a file is a command's numbered output, which the recorder's
/// scripted runner never wrote.
fn numbered(path: &str) -> bool {
    let file = path.rsplit('/').next().unwrap();
    file.len() == 10 && (file.ends_with(".stdout") || file.ends_with(".stderr"))
}

/// One hundred and five recorded scenarios of the Python helper, one hundred
/// and forty-three invocations in all, replayed request by request: every command
/// the Rust issues must be the recorded one with its deadline and its place
/// in the evidence, and receives the recorded answer. Each invocation ends
/// as recorded, prints the recorded line and failure kind, and leaves the
/// recorded journal, identity, outcome and evidence directories.
#[test]
fn every_recorded_reboot_invocation_is_replayed_request_by_request() {
    let recorded: Value = serde_json::from_str(include_str!("recorded_reboot.json")).unwrap();
    let scenarios = recorded["scenarios"].as_array().unwrap();
    assert_eq!(scenarios.len(), 105);
    let uid = rustix::process::geteuid().as_raw();
    let mut invocations = 0;
    for scenario in scenarios {
        let name = scenario["name"].as_str().unwrap();
        let scratch = Scratch::new();
        let base = scratch.path();
        let mut tree = recorded["trees"][index(&scenario["start"])]
            .as_object()
            .unwrap()
            .clone();
        build(base, &tree, 0);
        let run = Run {
            base,
            made: RefCell::new(Vec::new()),
        };
        for step in scenario["steps"].as_array().unwrap() {
            invocations += 1;
            let words = step["arguments"].as_array().unwrap().iter();
            let words: Vec<OsString> = words.map(|word| word.as_str().unwrap().into()).collect();
            let status = step["status"].as_i64().unwrap();
            let Some(action) = parse(&words) else {
                assert_eq!(status, 2, "{name}");
                continue;
            };
            assert_ne!(status, 2, "{name}");
            let edits = step["edits"].as_array().unwrap().iter();
            let holder: Vec<std::fs::File> = edits.filter_map(|edit| run.edit(edit)).collect();
            std::fs::write(
                base.join("boot_id"),
                format!("{}\n", step["boot"].as_str().unwrap()),
            )
            .unwrap();
            let calls = step["calls"].as_array().unwrap();
            let position = Cell::new(0_usize);
            let commands = Scripted::new(|asked: &Asked<'_>| {
                let call = calls.get(position.get());
                let call = call.unwrap_or_else(|| panic!("{name}: unrecorded {:?}", asked.argv));
                position.set(position.get() + 1);
                // The boot identity the host has from this command on.
                if let Some(boot) = call["boot"].as_str() {
                    std::fs::write(base.join("boot_id"), format!("{boot}\n")).unwrap();
                }
                // As on a host, both files exist from before the command starts.
                let private = |path: &Path| {
                    let mut options = std::fs::OpenOptions::new();
                    options.write(true).create_new(true).mode(0o600);
                    options.open(path).unwrap()
                };
                let mut output = private(asked.archive.unwrap());
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
            let privileged = step["euid"] == 0;
            let (line, outcome) = execute(&host, action, privileged, &rearm, |takeover| {
                Workload::fixture_for(base, uid, takeover)
            });
            drop(holder);
            run.observe();
            let context = format!("{name}: {words:?}: {outcome:?}");
            assert_eq!(i64::from(outcome.is_err()), status, "{context}");
            let line = run.recorded(&line.unwrap_or_default());
            assert_eq!(line, step["stdout"].as_str().unwrap(), "{context}");
            let complained = outcome.as_ref().err().map(complaint).unwrap_or_default();
            assert_eq!(complained, step["stderr"].as_str().unwrap(), "{context}");
            // The recorded tree: the one before with what this step changed.
            for removed in step["removed"].as_array().unwrap() {
                tree.remove(removed.as_str().unwrap());
            }
            for (path, entry) in step["changed"].as_object().unwrap() {
                tree.insert(path.clone(), entry.clone());
            }
            let mut left = BTreeMap::new();
            for (path, (mode, content)) in listed(base) {
                if !numbered(&path) {
                    left.insert(run.recorded(&path), (mode, run.recorded(&content)));
                }
            }
            assert_eq!(left, expected(&tree), "{context}");
            // One request to stop is forgotten exactly when a failed stop
            // begins its recovery.
            let outcome_path = format!(
                "{}/outcome.json",
                ATTEMPTS[run.made.borrow().len().saturating_sub(1)]
            );
            let recovering = step["changed"]
                .as_object()
                .unwrap()
                .iter()
                .any(|(path, entry)| {
                    let text = entry["text"].as_str().unwrap_or_default();
                    path.ends_with(&outcome_path) && text.contains("\"recoveryAttempted\": true")
                });
            let recovering = recovering && action == Action::Prepare;
            assert_eq!(rearmed.get(), usize::from(recovering), "{context}");
            let asked = commands.asked.borrow();
            assert_eq!(asked.len(), calls.len(), "{context}");
            for (number, (ours, theirs)) in asked.iter().zip(calls).enumerate() {
                let line = recorded["commandLines"][index(&theirs["argv"])]
                    .as_array()
                    .unwrap();
                let line: Vec<&str> = line.iter().map(|word| word.as_str().unwrap()).collect();
                let said: Vec<String> = ours.argv.iter().map(|word| run.recorded(word)).collect();
                assert_eq!(said, line, "{context}");
                let seconds = theirs["timeout"].as_f64().unwrap();
                assert!((ours.seconds - seconds).abs() < f64::EPSILON, "{context}");
                assert_eq!(ours.ceiling, Some(2 * 1024 * 1024), "{context}");
                assert!(ours.input.is_none() && ours.source.is_none(), "{context}");
                let kept = ours.archive.as_deref().unwrap();
                let evidence = kept.parent().unwrap().strip_prefix(base).unwrap();
                let evidence = run.recorded(evidence.to_str().unwrap());
                assert_eq!(evidence, theirs["attempt"].as_str().unwrap(), "{context}");
                let file = kept.file_name().unwrap().to_str().unwrap();
                assert_eq!(file, format!("{:03}.stdout", number + 1), "{context}");
            }
        }
    }
    assert_eq!(invocations, 143);
}

/// Seventeen usage texts with the Python patterns' verdicts: an option
/// counts only after blank space that begins a line and before more blank
/// space, whatever kind of blank that is.
#[test]
fn an_option_is_offered_exactly_where_the_recorded_patterns_found_it() {
    let recorded: Value = serde_json::from_str(include_str!("recorded_reboot.json")).unwrap();
    let vectors = recorded["offered"].as_array().unwrap();
    assert_eq!(vectors.len(), 17);
    for vector in vectors {
        let usage = vector["usage"].as_str().unwrap();
        assert_eq!(offered(usage, "--wait"), vector["wait"], "{usage:?}");
        let timeout = offered(usage, "--wait-timeout");
        assert_eq!(timeout, vector["waitTimeout"], "{usage:?}");
    }
}

/// The digest a recovery compares the resolved project by is the one the
/// Python helper took of the same project: over its keys in sorted order,
/// however the installed tool happened to list them.
#[test]
fn the_resolved_project_is_fingerprinted_as_the_recorded_helper_did() {
    let recorded: Value = serde_json::from_str(include_str!("recorded_reboot.json")).unwrap();
    let vector = recorded["canonical"].as_array().unwrap().last().unwrap();
    let document = vector["document"].as_str().unwrap();
    let project = state::decode(document.as_bytes()).unwrap();
    let listed = project.get("services").unwrap().object().unwrap();
    assert_eq!(listed.len(), 3);
    assert!(document.find("simplestchat\":").unwrap() < document.find("caddy").unwrap());
    assert_eq!(fingerprint(&project), vector["sha256"].as_str().unwrap());
}
