//! The Python helper's answers for every recorded machine, replayed.

use super::*;
use crate::host::command::Request;
use crate::host::failure::Result as Ran;
use serde_json::Value;
use std::cell::RefCell;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

const RECORDED: &str = include_str!("recorded_inspect.json");
/// The one recorded answer this command deliberately does not give.
const STRICTER: &str = "an image selection with a repeated revision";

/// The one command an inspection may run, answered as recorded.
struct Daemon<'a> {
    scenario: &'a Value,
    calls: RefCell<Vec<(Vec<String>, f64)>>,
}

impl Commands for Daemon<'_> {
    fn run(&self, request: &Request<'_>) -> Ran<Vec<u8>> {
        self.calls
            .borrow_mut()
            .push((request.argv.clone(), request.seconds));
        if self.scenario["dockerRaises"] == "timeout" {
            return Err(Failure::Timeout);
        }
        if self.scenario["dockerStatus"] != 0 {
            return Err(Failure::Command);
        }
        Ok(self.scenario["containers"]
            .as_str()
            .unwrap()
            .as_bytes()
            .to_vec())
    }
}

fn chmod(path: &Path, mode: u32) {
    std::fs::set_permissions(path, PermissionsExt::from_mode(mode)).unwrap();
}

fn mode(entry: &Value) -> u32 {
    let text = entry["mode"].as_str().unwrap().strip_prefix("0o").unwrap();
    u32::from_str_radix(text, 8).unwrap()
}

/// A recorded file's bytes: text, one long repeated run, or hexadecimal for
/// bytes that are not text.
fn content(entry: &Value) -> Vec<u8> {
    if let Some(hex) = entry["hex"].as_str() {
        let digits = hex.as_bytes().chunks(2);
        let byte = |pair: &[u8]| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16);
        return digits.map(|pair| byte(pair).unwrap()).collect();
    }
    let text = match &entry["text"] {
        Value::String(text) => text.clone(),
        long => {
            let count = usize::try_from(long["count"].as_u64().unwrap()).unwrap();
            let repeated = long["repeat"].as_str().unwrap().repeat(count);
            let prefix = long["prefix"].as_str().unwrap();
            format!("{prefix}{repeated}{}", long["suffix"].as_str().unwrap())
        }
    };
    text.into_bytes()
}

fn placed(base: &Path, name: &str) -> PathBuf {
    let (top, rest) = name.split_once('/').unwrap_or((name, ""));
    let fixed = match top {
        "etc" => "etc/simplestchat-public",
        "srv" => "srv/simplestchat-public",
        other => panic!("unexpected recorded top {other}"),
    };
    match rest {
        "" => base.join(fixed),
        rest => base.join(fixed).join(rest),
    }
}

/// Lay out one recorded machine under a scratch root.
fn build(base: &Path, scenario: &Value) {
    for directory in ["etc", "srv", "usr/lib", "usr/bin", "usr/local/bin"] {
        std::fs::create_dir_all(base.join(directory)).unwrap();
    }
    let tree = scenario["tree"].as_object().unwrap();
    for (name, entry) in tree {
        let path = placed(base, name);
        if let Some(link) = entry["link"].as_str() {
            std::os::unix::fs::symlink(link, &path).unwrap();
        } else if entry["directory"] == true {
            std::fs::create_dir(&path).unwrap();
        } else {
            std::fs::write(&path, content(entry)).unwrap();
        }
    }
    for (name, entry) in tree.iter().rev() {
        if !entry["link"].is_string() {
            chmod(&placed(base, name), mode(entry));
        }
    }
    for (name, field) in [
        ("etc/machine-id", "machine"),
        ("etc/os-release", "release"),
        ("usr/lib/os-release", "libRelease"),
    ] {
        if let Some(text) = scenario[field].as_str() {
            std::fs::write(base.join(name), text).unwrap();
        }
    }
    if scenario["docker"] == "present" {
        // A client found only outside the packaged location cannot be run.
        let elsewhere = scenario["dockerRaises"] == "missing";
        let directory = if elsewhere {
            "usr/local/bin"
        } else {
            "usr/bin"
        };
        let client = base.join(directory).join("docker");
        std::fs::write(&client, "#!/bin/sh\n").unwrap();
        chmod(&client, 0o755);
    }
}

#[test]
fn every_recorded_machine_gets_the_recorded_answer() {
    let recorded: Value = serde_json::from_str(RECORDED).unwrap();
    let scenarios = recorded["scenarios"].as_array().unwrap();
    assert_eq!(scenarios.len(), 96);
    let me = rustix::process::getuid().as_raw();
    let (mut passed, mut refused) = (0, 0);
    for (index, scenario) in scenarios.iter().enumerate() {
        let name = scenario["name"].as_str().unwrap();
        let base = std::env::temp_dir().canonicalize().unwrap();
        let base = base.join(format!("migrate-inspect-{}-{index}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir(&base).unwrap();
        build(&base, scenario);
        let owners = scenario["owners"].as_object().unwrap();
        let foreign: Vec<(PathBuf, u32)> = owners
            .iter()
            .map(|(name, owner)| {
                (
                    placed(&base, name),
                    u32::try_from(owner.as_u64().unwrap()).unwrap(),
                )
            })
            .collect();
        // The scratch owner stands for root; a marked entry is someone else's.
        let owner = |path: &Path, metadata: &Metadata| {
            let marked = foreign.iter().find(|(marked, _)| marked == path);
            let scratch = if metadata.uid() == me {
                0
            } else {
                metadata.uid()
            };
            marked.map_or(scratch, |(_, owner)| *owner)
        };
        let daemon = Daemon {
            scenario,
            calls: RefCell::new(Vec::new()),
        };
        let (config, root) = (
            base.join("etc/simplestchat-public"),
            base.join("srv/simplestchat-public"),
        );
        let machine = Machine {
            privileged: scenario["euid"] == 0,
            system: scenario["system"].as_str().unwrap(),
            processor: scenario["cpu"].as_str().unwrap(),
            anchor: &base,
            config: &config,
            root: &root,
            commands: &daemon,
            owner: &owner,
        };
        let arguments: Vec<&str> = scenario["arguments"]
            .as_array()
            .unwrap()
            .iter()
            .map(|word| word.as_str().unwrap())
            .collect();
        let expected = scenario["stdout"].as_str().unwrap();
        let [role, origin] = arguments[..] else {
            // Another shape of command line is a syntax error here; the
            // helper answered it with a code of its own.
            assert!(expected.contains("invalid_arguments"), "{name}");
            let words: Vec<OsString> = arguments.iter().map(OsString::from).collect();
            assert_eq!(run(&words), ExitCode::from(2), "{name}");
            refused += 1;
            std::fs::remove_dir_all(&base).unwrap();
            continue;
        };
        let (line, verdict) = answer(&machine, role, origin);
        if name == STRICTER {
            // The helper kept the last of two revisions; ambiguous JSON is
            // not a selection here.
            assert!(expected.contains("\"passed\": true"), "{name}");
            assert_eq!(
                line,
                "{\"passed\": false, \"failure\": \"inspection_failed\"}\n"
            );
            assert!(!verdict);
            chmod(&base, 0o700);
            std::fs::remove_dir_all(&base).unwrap();
            continue;
        }
        assert_eq!(line, expected, "{name}");
        assert_eq!(verdict, scenario["status"] == 0, "{name}");
        // The only command is the recorded listing, with its deadline.
        let calls: Vec<Value> = daemon
            .calls
            .borrow()
            .iter()
            .map(|(argv, seconds)| serde_json::json!({"argv": argv, "timeout": *seconds as u64}))
            .collect();
        let recorded = scenario["calls"].as_array().unwrap();
        if scenario["dockerRaises"] == "missing" {
            assert!(calls.is_empty() && recorded.len() == 1, "{name}");
        } else {
            assert_eq!(&calls, recorded, "{name}");
        }
        passed += usize::from(verdict);
        // Nothing on the machine changed: the inspection only reads.
        chmod(&base, 0o700);
        std::fs::remove_dir_all(&base).unwrap();
    }
    // Twenty-six recorded machines passed; one of them is refused here.
    assert_eq!((passed, refused), (25, 3));
}

/// The recorded failure codes are exactly the ones this command can print.
#[test]
fn the_recording_covers_every_code_the_command_prints() {
    let source = include_str!("inspect.rs");
    let recorded: Value = serde_json::from_str(RECORDED).unwrap();
    let mut printed = BTreeSet::new();
    for scenario in recorded["scenarios"].as_array().unwrap() {
        let answer: Value = serde_json::from_str(scenario["stdout"].as_str().unwrap()).unwrap();
        if let Some(code) = answer["failure"].as_str() {
            printed.insert(code.to_owned());
        }
    }
    printed.remove("invalid_arguments");
    let mut named = BTreeSet::new();
    for word in source.split('"').skip(1).step_by(2) {
        let code = word
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte == b'_');
        if code && word.contains('_') && !word.contains("__") {
            named.insert(word.to_owned());
        }
    }
    assert_eq!(named, printed);
}

/// A name that cannot be judged is a failure of the inspection, never a
/// machine without configuration.
#[test]
fn a_path_that_cannot_be_examined_is_not_an_absent_one() {
    if rustix::process::geteuid().is_root() {
        // Root examines everything; there is nothing to refuse.
        return;
    }
    let base = std::env::temp_dir().canonicalize().unwrap();
    let base = base.join(format!("migrate-inspect-closed-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("closed/held")).unwrap();
    assert_eq!(exists(&base.join("closed/held")), Ok(true));
    assert_eq!(exists(&base.join("closed/absent")), Ok(false));
    assert_eq!(exists(&base.join("closed/held/absent/below")), Ok(false));
    chmod(&base.join("closed"), 0o000);
    assert_eq!(exists(&base.join("closed/held")), Err(PRIVATE));
    assert_eq!(present(&base.join("closed/held")), Err(PRIVATE));
    chmod(&base.join("closed"), 0o700);
    std::fs::remove_dir_all(&base).unwrap();
}
