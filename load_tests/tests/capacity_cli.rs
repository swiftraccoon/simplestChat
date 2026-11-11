//! The compiled `load_test capacity` command against the answers recorded
//! from the retired Python command line: exit status, standard output and
//! standard error. No workload runs and no container engine is contacted.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// The answers the retired Python command line gave, with the calibration
/// files it was given.
fn recorded() -> Value {
    serde_json::from_str(include_str!("capacity_cli_recorded.json")).unwrap()
}

/// The compiled command, run from the checkout as the recorded one was.
fn capacity(arguments: &[String]) -> Output {
    let checkout = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    Command::new(env!("CARGO_BIN_EXE_load_test"))
        .arg("capacity")
        .args(arguments)
        .current_dir(checkout)
        .output()
        .unwrap()
}

/// A private directory holding the recorded calibration files, removed on drop.
struct Fixture(PathBuf);

impl Fixture {
    fn new(files: &Value) -> Self {
        let name = format!("simplestchat-capacity-cli-{}", std::process::id());
        let path = std::env::temp_dir().join(name);
        std::fs::create_dir(&path).unwrap();
        for (name, text) in files.as_object().unwrap() {
            std::fs::write(path.join(name), text.as_str().unwrap()).unwrap();
        }
        Self(path)
    }

    /// Recorded text with its placeholder replaced by this directory.
    fn placed(&self, text: &Value) -> String {
        let directory = self.0.to_str().unwrap();
        text.as_str().unwrap().replace("{fixture}", directory)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Suggestions, quota edges, a refused host, a ranking with a lower bound,
/// a calibrated suggestion and both refusals of a differently shaped report.
#[test]
fn compiled_command_matches_the_recorded_python_command_line() {
    let recorded = recorded();
    let fixture = Fixture::new(&recorded["files"]);
    let cases = recorded["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 9);
    for case in cases {
        let arguments = case["arguments"].as_array().unwrap().iter();
        let arguments = arguments.map(|argument| fixture.placed(argument));
        let output = capacity(&arguments.collect::<Vec<_>>());
        let label = &case["arguments"];
        let status = output.status.code().map(i64::from);
        assert_eq!(status, case["status"].as_i64(), "{label}");
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            fixture.placed(&case["stdout"]),
            "{label}"
        );
        assert_eq!(
            String::from_utf8(output.stderr).unwrap(),
            fixture.placed(&case["stderr"]),
            "{label}"
        );
    }
    let ranking = cases[5]["stdout"].as_str().unwrap();
    assert!(ranking.lines().nth(1).unwrap().starts_with("cheap "));
}

/// An incomplete or unknown invocation is a usage error, and help is not.
#[test]
fn compiled_command_refuses_incomplete_invocations_with_usage_status() {
    for arguments in [vec![], vec!["suggest"], vec!["compare"], vec!["invented"]] {
        let arguments = arguments.into_iter().map(String::from);
        let output = capacity(&arguments.collect::<Vec<_>>());
        assert_eq!(output.status.code(), Some(2));
    }
    let help = capacity(&["--help".to_owned()]);
    assert_eq!(help.status.code(), Some(0));
    let text = String::from_utf8(help.stdout).unwrap();
    assert!(text.starts_with("Usage: load_test capacity suggest"));
}
