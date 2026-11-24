//! Replays of the recorded lock parsers and policies.
//!
//! `recorded_locks.json` holds what the reference in Python
//! answered for requirement lines and versions (as its packaging library
//! judges them), the lines of requirement files beside what pip's own
//! reader makes of the same text, npm locks, graphs of includes, the
//! wheel-only policy, locks nobody audits and the licence allowlist. Where
//! the reference failed with an exception of its interpreter or its
//! library, `named` gives the code here; `STRICTER` names what it admitted
//! and is refused here.

use super::{
    Dependency, PYTHON_LOCKS, allowed_licenses, dependencies, npm_dependencies,
    require_audited_locks, require_wheel_only, requirement, requirement_lines,
};
pub(super) use crate::scratch::content;
use crate::{Fault, Result, policy::space, scratch::Scratch};
use serde_json::{Value, json};
use std::{collections::BTreeSet, path::Path};

const RECORDED: &str = include_str!("recorded_locks.json");

/// The hash-locked requirement files of the reference, with which every
/// recorded graph, tree and review is replayed.
pub(super) const RECORDED_PYTHON_LOCKS: [&str; 5] = [
    "build/python-requirements.txt",
    "ops/ansible/requirements.txt",
    "security/requirements.txt",
    "vendor/mediasoup-sys-0.19.0/python-invoke-requirements.txt",
    "vendor/mediasoup-sys-0.19.0/python-tools-requirements.txt",
];
/// The locks of the reference that this gate does not read: the Python lint
/// tools' pins, removed with the repository's last Python.
const REMOVED_SINCE_RECORDING: [&str; 1] = ["build/python-requirements.txt"];

/// The gate reads the reference's locks but the removed one, in the same
/// order, and each is a file of the checkout; the removed one is not.
#[test]
fn the_read_locks_are_the_recorded_ones_but_the_removed() {
    let kept: Vec<&str> = RECORDED_PYTHON_LOCKS
        .into_iter()
        .filter(|path| !REMOVED_SINCE_RECORDING.contains(path))
        .collect();
    assert_eq!(kept, PYTHON_LOCKS);
    let checkout = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for path in PYTHON_LOCKS {
        assert!(checkout.join(path).is_file(), "{path}");
    }
    for path in REMOVED_SINCE_RECORDING {
        assert!(!checkout.join(path).exists(), "{path}");
    }
}

/// A requirement is a plain pin; a lock's version is the number three; a
/// lock entry holds no fraction, which has no one spelling; a lock is UTF-8
/// from its first byte.
const STRICTER: [(&str, &str); 6] = [
    (
        "a specifier in parentheses",
        "dependency_python_exact_version",
    ),
    ("empty extras", "dependency_python_exact_version"),
    (
        "a lock version that is a fraction",
        "dependency_npm_lock_version",
    ),
    ("a fraction among the fields", "dependency_npm_lock"),
    ("a lock after a byte order mark", "dependency_npm_lock"),
    ("a lock in UTF-16", "dependency_npm_lock"),
];

fn recorded() -> Value {
    serde_json::from_str(RECORDED).expect("the recording is JSON")
}

pub(super) fn text(value: &Value) -> &str {
    value.as_str().expect("a recorded string")
}

pub(super) fn list(value: &Value) -> &[Value] {
    value.as_array().expect("a recorded list")
}

fn named<'a>(code: &'a str, group: &str) -> &'a str {
    match (code, group) {
        ("InvalidRequirement", _) => "dependency_python_exact_version",
        ("UnicodeDecodeError", "lines") => "dependency_requirements_text",
        ("FileNotFoundError", _) => "tool_entry_missing",
        (_, "npm") if !code.starts_with("dependency_") => "dependency_npm_lock",
        (_, "allowed") if !code.starts_with(|first: char| first.is_ascii_lowercase()) => {
            "dependency_license_policy"
        }
        (code, _) => code,
    }
}

/// Counts of the cases answered alike, refused alike and refused more
/// strictly.
#[derive(Debug, Default, PartialEq, Eq)]
struct Tally(usize, usize, usize);

/// Hold one answer to the recording. The answer fails with a fault or, where
/// a tree is read through a command, with the failure of an invocation.
fn settle<E: ToString>(
    group: &str,
    name: &str,
    reference: &Value,
    judged: std::result::Result<Value, E>,
    tally: &mut Tally,
) {
    let refused = judged.as_ref().err().map(ToString::to_string);
    let refused = refused.as_deref();
    if let Some((_, code)) = STRICTER.iter().find(|(case, _)| *case == name) {
        assert!(reference.get("code").is_none(), "{group}: {name}");
        assert_eq!(refused, Some(*code), "{group}: {name}");
        tally.2 += 1;
    } else if let Some(code) = reference.get("code") {
        assert_eq!(refused, Some(named(text(code), group)), "{group}: {name}");
        tally.1 += 1;
    } else {
        assert_eq!(judged.as_ref().ok(), Some(reference), "{group}: {name}");
        tally.0 += 1;
    }
}

fn shown(dependency: &Dependency) -> Value {
    json!({
        "ecosystem": dependency.ecosystem,
        "manifest": dependency.manifest,
        "name": dependency.name,
        "version": dependency.version,
        "identity": dependency.identity,
        "hashes": dependency.hashes,
        "license": dependency.license,
        "source": dependency.source,
    })
}

/// Declarations in the recording's order: by ecosystem, lock, name and
/// identity.
fn listed(found: &BTreeSet<Dependency>) -> Value {
    let mut ordered: Vec<&Dependency> = found.iter().collect();
    ordered.sort_by_key(|item| (item.ecosystem, &item.manifest, &item.name, &item.identity));
    let shown: Vec<Value> = ordered.into_iter().map(shown).collect();
    json!({"dependencies": shown})
}

fn pinned(line: &str) -> Result<Value> {
    let pin = requirement::pin(line)?;
    Ok(json!({
        "ecosystem": "pypi",
        "manifest": "requirements.txt",
        "name": pin.name,
        "version": pin.version,
        "identity": pin.identity,
        "hashes": pin.hashes,
        "license": "",
        "source": "",
    }))
}

#[test]
fn every_recorded_requirement_is_read_alike() {
    let recording = recorded();
    let mut tally = Tally::default();
    for case in list(&recording["requirements"]) {
        let name = text(&case["name"]);
        settle(
            "requirements",
            name,
            &case["result"],
            pinned(text(&case["line"])),
            &mut tally,
        );
    }
    assert_eq!(tally, Tally(66, 47, 2));
}

#[test]
fn every_recorded_version_is_judged_alike() {
    let recording = recorded();
    let hash = text(&recording["hashes"][0]);
    let mut tally = Tally::default();
    for case in list(&recording["versions"]) {
        let version = text(&case["version"]);
        let line = format!("x=={version} --hash=sha256:{hash}");
        settle(
            "versions",
            version,
            &case["result"],
            pinned(&line),
            &mut tally,
        );
    }
    assert_eq!(tally, Tally(61, 41, 0));
}

#[test]
fn every_recorded_requirement_file_is_split_alike() {
    let recording = recorded();
    let mut tally = Tally::default();
    for case in list(&recording["lines"]) {
        let lines = requirement_lines(&content(&case["content"]));
        let judged = lines.map(|lines| json!({"lines": lines}));
        settle(
            "lines",
            text(&case["name"]),
            &case["result"],
            judged,
            &mut tally,
        );
    }
    assert_eq!(tally, Tally(29, 11, 0));
}

#[test]
fn every_recorded_npm_lock_is_read_alike() {
    let recording = recorded();
    let mut tally = Tally::default();
    for case in list(&recording["npm"]) {
        let found = npm_dependencies("web/package-lock.json", &content(&case["content"]));
        let judged = found.map(|found| listed(&found));
        settle(
            "npm",
            text(&case["name"]),
            &case["result"],
            judged,
            &mut tally,
        );
    }
    assert_eq!(tally, Tally(14, 20, 4));
}

#[test]
fn every_recorded_graph_of_locks_is_read_alike() {
    let recording = recorded();
    let mut tally = Tally::default();
    for case in list(&recording["graphs"]) {
        let files = case["files"].as_object().expect("files");
        let load = |path: &str| Ok(files.get(path).map(|file| text(file).as_bytes().to_vec()));
        let judged = dependencies(&load, &RECORDED_PYTHON_LOCKS).map(|found| listed(&found));
        settle(
            "graphs",
            text(&case["name"]),
            &case["result"],
            judged,
            &mut tally,
        );
    }
    assert_eq!(tally, Tally(13, 20, 0));
}

/// The recorded trees but those of the scanners' lock, whose installer the
/// reference read from the tree and which is this gate's own here.
#[test]
fn every_recorded_tree_is_held_to_wheels_alike() {
    let recording = recorded();
    let scratch = Scratch::new("lw");
    let mut tally = Tally::default();
    for (index, case) in list(&recording["wheels"]).iter().enumerate() {
        let root = scratch.0.join(index.to_string());
        std::fs::create_dir(&root).expect("a tree");
        for (path, written) in case["files"].as_object().expect("files") {
            let target = root.join(path);
            std::fs::create_dir_all(target.parent().expect("a parent")).expect("directories");
            std::fs::write(target, text(written)).expect("a file");
        }
        let judged = require_wheel_only(&root, &RECORDED_PYTHON_LOCKS);
        let judged = judged.map(|()| json!({"held": true}));
        settle(
            "wheels",
            text(&case["name"]),
            &case["result"],
            judged,
            &mut tally,
        );
    }
    assert_eq!(tally, Tally(4, 8, 0));
}

/// The scanners' lock needs no option of its own, whatever else the tree
/// holds: this gate's environment build installs it with the option, and
/// every other lock is still held to carry it.
#[test]
fn the_scanners_lock_is_installed_by_the_gates_own_command() {
    let scratch = Scratch::new("ls");
    let root = &scratch.0;
    let lock = root.join("security/requirements.txt");
    std::fs::create_dir_all(lock.parent().expect("a parent")).expect("directories");
    std::fs::write(&lock, "pip-audit==2.10.1 \\\n    --hash=sha256:00\n").expect("a lock");
    let installer = root.join("build/install-scanners.sh");
    std::fs::create_dir_all(installer.parent().expect("a parent")).expect("directories");
    std::fs::write(
        &installer,
        "pip install --requirement security/requirements.txt\n",
    )
    .expect("an installer");
    assert_eq!(require_wheel_only(root, &PYTHON_LOCKS), Ok(()));
    let other = root.join("ops/ansible/requirements.txt");
    std::fs::create_dir_all(other.parent().expect("a parent")).expect("directories");
    std::fs::write(&other, "ansible-core==2.21.4 \\\n    --hash=sha256:00\n").expect("a lock");
    assert_eq!(
        require_wheel_only(root, &PYTHON_LOCKS),
        Err(Fault("dependency_python_wheel_only_policy"))
    );
}

#[test]
fn every_recorded_unaudited_lock_is_judged_alike() {
    let recording = recorded();
    let mut tally = Tally::default();
    for case in list(&recording["audited"]) {
        let names: BTreeSet<String> = list(&case["names"])
            .iter()
            .map(|name| text(name).to_owned())
            .collect();
        let tree = |side: &'static str| {
            move |path: &str| {
                Ok(case[side]
                    .get(path)
                    .map(|file| text(file).as_bytes().to_vec()))
            }
        };
        let vendored = case["includeVendor"] == true;
        let (current, previous) = (tree("current"), tree("previous"));
        let locks = &RECORDED_PYTHON_LOCKS;
        let judged = require_audited_locks(&names, &current, &previous, vendored, locks);
        let judged = judged.map(|()| json!({"held": true}));
        settle(
            "audited",
            text(&case["name"]),
            &case["result"],
            judged,
            &mut tally,
        );
    }
    assert_eq!(tally, Tally(15, 20, 0));
}

#[test]
fn every_recorded_allowlist_is_read_alike() {
    let recording = recorded();
    let scratch = Scratch::new("la");
    let mut tally = Tally::default();
    for (index, case) in list(&recording["allowed"]).iter().enumerate() {
        let root = scratch.0.join(index.to_string());
        std::fs::create_dir(&root).expect("a tree");
        if !case["text"].is_null() {
            std::fs::write(root.join("deny.toml"), content(&case["text"])).expect("a policy");
        }
        let judged = allowed_licenses(&root).map(|allowed| json!({"allowed": allowed}));
        settle(
            "allowed",
            text(&case["name"]),
            &case["result"],
            judged,
            &mut tally,
        );
    }
    assert_eq!(tally, Tally(3, 11, 0));
}

/// A backslash before a carriage return, or one that ends the file, joins
/// lines for pip's reader and not here. What is then read here is no
/// requirement, so such a lock is refused and nothing is hidden.
const OTHERWISE_THAN_PIP: [&str; 2] = [
    "a backslash before a carriage return",
    "a backslash at the end",
];

#[test]
fn what_is_read_is_what_pip_reads() {
    let recording = recorded();
    let narrow = |line: &str| {
        let words: Vec<&str> = line.split(space).filter(|word| !word.is_empty()).collect();
        words.join(" ")
    };
    let (mut same, mut otherwise) = (0, 0);
    for case in list(&recording["lines"]) {
        let name = text(&case["name"]);
        let bytes = content(&case["content"]);
        let (Some(pip), Ok(read)) = (case["pip"].as_array(), requirement_lines(&bytes)) else {
            continue;
        };
        // Lines that were joined differ in the width of the space between them.
        let pip: Vec<String> = pip.iter().map(|line| narrow(text(line))).collect();
        let read: Vec<String> = read.iter().map(|line| narrow(line)).collect();
        if OTHERWISE_THAN_PIP.contains(&name) {
            assert_ne!(read, pip, "{name}");
            let load = |path: &str| Ok((path == PYTHON_LOCKS[0]).then(|| bytes.clone()));
            let refused = dependencies(&load, &PYTHON_LOCKS).err();
            let refused = refused.map(|failure| failure.to_string());
            assert_eq!(refused.as_deref(), Some("dependency_python_hash"), "{name}");
            otherwise += 1;
        } else {
            assert_eq!(read, pip, "{name}");
            same += 1;
        }
    }
    assert_eq!((same, otherwise), (27, 2));
}

/// An npm lock of `count` packages whose names start with `prefix`.
fn many(prefix: &str, count: usize) -> Vec<u8> {
    let entry =
        |index: usize| format!("\"node_modules/{prefix}{index}\":{{\"version\":\"1.0.0\"}}");
    let packages: Vec<String> = (0..count).map(entry).collect();
    format!(
        "{{\"lockfileVersion\":3,\"packages\":{{{}}}}}",
        packages.join(",")
    )
    .into_bytes()
}

#[test]
fn a_lock_and_a_tree_declare_at_most_ten_thousand_dependencies() {
    let found = npm_dependencies("web/package-lock.json", &many("p", 10_000));
    assert_eq!(found.map(|found| found.len()), Ok(10_000));
    let refused = npm_dependencies("web/package-lock.json", &many("p", 10_001));
    assert_eq!(
        refused.err().map(|fault| fault.0),
        Some("dependency_npm_count")
    );
    let tree = |first: usize, second: usize| {
        move |path: &str| {
            Ok(match path {
                "web/package-lock.json" => Some(many("a", first)),
                "web/e2e/package-lock.json" => Some(many("b", second)),
                _ => None,
            })
        }
    };
    let found = dependencies(&tree(5_000, 5_000), &PYTHON_LOCKS);
    assert_eq!(found.map(|found| found.len()), Ok(10_000));
    let refused = dependencies(&tree(5_000, 5_001), &PYTHON_LOCKS);
    let refused = refused.err().map(|failure| failure.to_string());
    assert_eq!(refused.as_deref(), Some("dependency_count"));
}
