//! Replays of `recorded_toolchains.json`: what the reference in Python
//! made of the host that built, with its five commands answered from the
//! recording, and of lists of packages.

use super::{FORMAT, Host, MAX_REPORT, SHORT, builder_rpm_records, toolchain_evidence};
use crate::{
    document::Document,
    native::{
        problem,
        tests::{replay_with, tallied},
    },
    vendor::Verdict,
};
use std::path::{Path, PathBuf};

const RECORDING: &[u8] = include_bytes!("recorded_toolchains.json");

/// A host that answers the five commands from a case of the recording and
/// refuses any other: a command without an answer fails.
pub(in crate::native) struct Scripted<'a> {
    pub(in crate::native) outputs: &'a Document,
    pub(in crate::native) runtime: PathBuf,
}

impl Host for Scripted<'_> {
    fn run(&self, argv: &[&str], limit: u64) -> Verdict<String> {
        let runtime = self.runtime.display().to_string();
        let file = ["rpm", "--query", "--file", &runtime, "--queryformat"];
        let (name, room) = if argv == ["rustc", "--version", "--verbose"] {
            ("rustc", SHORT)
        } else if argv == ["c++", "--version"] {
            ("cxx", SHORT)
        } else if argv == ["rpm", "--query", "--all", "--queryformat", FORMAT] {
            ("all", MAX_REPORT)
        } else if argv == [&file[..], &[FORMAT]].concat() {
            ("owner", MAX_REPORT)
        } else if argv == [&file[..], &["%{LICENSE}\n"]].concat() {
            ("license", SHORT)
        } else {
            panic!("an unexpected command: {argv:?}")
        };
        assert_eq!(limit, room, "{argv:?}");
        let said = self.outputs.get(name).and_then(Document::text);
        said.map(str::to_owned)
            .ok_or_else(|| problem("Build-host provenance command failed"))
    }
}

/// The host of a case of the recording, for the fixture at `root`.
pub(in crate::native) fn host<'a>(root: &Path, case: &'a Document) -> Scripted<'a> {
    Scripted {
        outputs: case.get("outputs").expect("outputs"),
        runtime: root.join("system/lib/libstdc++.a"),
    }
}

fn recorded() -> Vec<(String, String, String)> {
    replay_with(RECORDING, "toolchain", &|root, case| {
        let host = host(root, case);
        toolchain_evidence(root, &host.runtime, &host)
    })
}

/// Hosts that both refuse in other words: the reference in those of a
/// field that is not there.
const REWORDED: [(&str, &str); 2] = [
    (
        "a manifest without a compiler version",
        "Missing field: rust-version",
    ),
    ("a manifest without a package", "Missing field: package"),
];

#[test]
fn every_recorded_host_comes_to_the_same() {
    // Held alike, refused alike, and refused in other words.
    assert_eq!(tallied(recorded(), &REWORDED), [6, 27, 2]);
}

#[test]
fn every_recorded_list_of_packages_is_sorted_alike() {
    let recording = Document::read(RECORDING).expect("a recording");
    let lists = recording.get("rpms").and_then(Document::items);
    let (mut held, mut refused) = (0, 0);
    for list in lists.expect("lists") {
        let lines = list.get("lines").and_then(Document::items).expect("lines");
        let lines: Vec<&str> = lines.iter().filter_map(Document::text).collect();
        let result = list.get("result").expect("a result");
        match builder_rpm_records(&lines) {
            Ok((packages, keys)) => {
                let listed = |lines: Vec<String>| {
                    Document::List(lines.into_iter().map(Document::from).collect())
                };
                let both = Document::List(vec![listed(packages), listed(keys)]);
                assert_eq!(Some(&both), result.get("held"), "{lines:?}");
                held += 1;
            }
            Err(problem) => {
                let words = result.get("refused").and_then(Document::text);
                assert_eq!(Some(problem.0.as_str()), words, "{lines:?}");
                refused += 1;
            }
        }
    }
    assert_eq!((held, refused), (8, 19));
}

#[test]
fn a_command_of_this_machine_must_end_well_and_complain_of_nothing() {
    use super::Machine;
    let failed = Err(crate::vendor::Problem(
        "Build-host provenance command failed".to_owned(),
    ));
    let run = |script: &str, limit: u64| Machine.run(&["/bin/sh", "-c", script], limit);
    assert_eq!(run("printf 'said\\nmore'", 64), Ok("said\nmore".to_owned()));
    assert_eq!(run("exit 0", 64), Ok(String::new()));
    assert_eq!(run("printf said; exit 3", 64), failed);
    assert_eq!(run("printf said; printf complaint >&2", 64), failed);
    assert_eq!(run("printf complaint >&2; exit 1", 64), failed);
    assert_eq!(Machine.run(&[], 64), failed);
    let words = |came: Verdict<String>| came.err().map(|problem| problem.0);
    let over = Some("command_stdout_limit_exceeded".to_owned());
    assert_eq!(words(run("printf 12345", 4)), over);
    assert_eq!(run("printf 1234", 4), Ok("1234".to_owned()));
    let no_text = Some("Build-host output is no text".to_owned());
    assert_eq!(words(run("printf '\\377'", 64)), no_text);
    let absent = Machine.run(&["/nonexistent/program"], 64);
    assert_eq!(words(absent), Some("tool_entry_missing".to_owned()));
    assert_eq!((super::PATIENCE.as_secs(), super::COMPLAINT), (30, 65536));
    assert_eq!((MAX_REPORT, SHORT), (8 * 1024 * 1024, 65536));
    let asked = "%{NAME}\t%{EPOCHNUM}:%{VERSION}-%{RELEASE}.%{ARCH}\t%{SOURCERPM}\n";
    assert_eq!(FORMAT, asked);
}
