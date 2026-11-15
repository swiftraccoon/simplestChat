//! Replays of the recorded licence reviews.
//!
//! `recorded_review.json` holds what the review of the reference in
//! Python did in real repositories whose
//! commits have fixed names, with a `curl` that answers its calls in order:
//! how it ended, the lines of its summary, its receipt byte for byte, the
//! evidence it kept with what each command printed, and the addresses it
//! asked. It also holds the truth
//! table of the one reviewed wheel of the controller. Where the reference
//! failed with an exception of its interpreter, `named` gives the code here.

use super::{
    Bounds, Dependency, Evidence, REVIEWED,
    registry_tests::curl,
    review::{CONTROLLER, Controller, Review, check},
    tests::{content, list, text},
};
use crate::{
    context::{Context, Failure, Limits, Outcome},
    fast::Programs,
    scratch::{Scratch, host_git, repository, written},
};
use serde_json::{Map, Value, json};
use std::{
    ffi::OsString,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

const RECORDED: &str = include_str!("recorded_review.json");

fn recorded() -> Value {
    serde_json::from_str(RECORDED).expect("the recording is JSON")
}

/// The host's git, and the scripted `curl` unless it is to be missing.
struct Tools {
    git: PathBuf,
    curl: PathBuf,
    missing: Vec<String>,
}

impl Programs for Tools {
    fn program(&self, name: &'static str) -> Outcome<PathBuf> {
        Err(Failure::Prerequisite(name))
    }

    fn prerequisite(&self, name: &'static str) -> Outcome<PathBuf> {
        if self.missing.iter().any(|missing| missing == name) {
            return Err(Failure::Prerequisite(name));
        }
        match name {
            "git" => Ok(self.git.clone()),
            "curl" => Ok(self.curl.clone()),
            _ => Err(Failure::Prerequisite(name)),
        }
    }

    fn python(&self) -> Outcome<PathBuf> {
        Err(Failure::Prerequisite("python3"))
    }
}

/// The code here for an exception of the reference's interpreter.
fn named(code: &str) -> &str {
    match code {
        "FileNotFoundError" => "tool_entry_missing",
        "IsADirectoryError" => "nonregular_tool_file",
        "JSONDecodeError" => "dependency_npm_lock",
        code => code,
    }
}

/// One recorded case set up in directories of its own: the repository, the
/// copy of its sources, the scripted `curl` and the base the review is given.
struct Staged {
    root: PathBuf,
    output: PathBuf,
    snapshot: PathBuf,
    log: PathBuf,
    programs: Tools,
    base: Option<String>,
}

fn staged(base: &Path, git: &Path, recording: &Value, case: &Value) -> Staged {
    let name = text(&case["name"]);
    let [root, output, tools, snapshot] =
        ["root", "output", "tools", "snapshot"].map(|part| base.join(part));
    for directory in [&root, &output, &tools, &snapshot] {
        std::fs::create_dir_all(directory).expect("a directory");
    }
    let repository = repository(&root, git, recording, case);
    for (commit, named) in case["commits"].as_object().expect("commits") {
        let built = repository.commits.get(commit.as_str());
        assert_eq!(
            built.map(String::as_str),
            Some(text(named)),
            "{name}: {commit}"
        );
    }
    written(&snapshot, &repository.files.clone().into_iter().collect());
    let log = base.join("log");
    for (number, answer) in list(&case["answers"]).iter().enumerate() {
        let said = match answer.get("raw") {
            Some(raw) => content(raw),
            None => {
                let mut said = content(&answer["body"]);
                said.push(b'\n');
                said.extend(text(&answer["code"]).bytes());
                said
            }
        };
        std::fs::write(tools.join(format!("{number}.out")), said).expect("an answer");
        let status = answer["status"].as_i64().expect("a status");
        std::fs::write(
            tools.join(format!("{number}.status")),
            format!("{status}\n"),
        )
        .expect("a status file");
    }
    std::fs::write(tools.join("count"), "0\n").expect("a count");
    let program = tools.join("curl");
    std::fs::write(&program, curl(&tools, &log)).expect("a program");
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).expect("a mode");
    let missing = list(&case["missing"])
        .iter()
        .map(|name| text(name).to_owned());
    let programs = Tools {
        git: git.to_owned(),
        curl: program,
        missing: missing.collect(),
    };
    let given = match text(&case["base"]) {
        "head" => None,
        "unknown" => Some("0".repeat(40)),
        base => Some(
            repository
                .commits
                .get(base)
                .cloned()
                .unwrap_or(base.to_owned()),
        ),
    };
    Staged {
        root,
        output,
        snapshot,
        log,
        programs,
        base: given,
    }
}

impl Staged {
    /// Run the review with `controller` as the one reviewed wheel.
    fn judged(&self, controller: Controller<'_>, vendored: bool) -> (Outcome<()>, Context) {
        let ambient = |variable: &str| -> Option<OsString> {
            match variable {
                "PATH" => Some("/usr/bin:/bin".into()),
                "HOME" => Some("/h".into()),
                _ => None,
            }
        };
        let mut context =
            Context::new(&self.root, &self.output, Limits::REVIEWED, &ambient).expect("a context");
        let review = Review {
            snapshot: &self.snapshot,
            base: self.base.as_deref(),
            include_vendor: vendored,
            bounds: Bounds::REVIEWED,
            reviewed: &REVIEWED,
            controller,
        };
        (check(&mut context, &self.programs, &review), context)
    }

    /// The addresses `curl` was asked for, in order.
    fn addresses(&self) -> Vec<String> {
        let asked = std::fs::read_to_string(&self.log).unwrap_or_default();
        let last = |logged: &str| {
            let bytes: Vec<u8> = (0..logged.len() / 2)
                .map(|at| u8::from_str_radix(&logged[2 * at..2 * at + 2], 16).expect("hex"))
                .collect();
            let arguments = bytes.strip_suffix(&[0]).expect("an end");
            let last = arguments.rsplit(|byte| *byte == 0).next();
            String::from_utf8(last.expect("an argument").to_vec()).expect("text")
        };
        let logged = asked.lines().filter_map(|line| line.strip_prefix("argv\t"));
        logged.map(last).collect()
    }

    /// The receipt, when one was written.
    fn receipt(&self) -> Option<String> {
        std::fs::read_to_string(self.output.join("dependency-licenses.json")).ok()
    }
}

/// The lines of the summary as a recording holds them: the time a line
/// took is only said to be there, and no line names a field twice.
fn lines(context: &Context, name: &str) -> Value {
    let lines = context.checks().iter().map(|check| {
        let mut line = Map::new();
        for (field, value) in check {
            let (field, value) = match *field {
                "elapsedSeconds" => ("timed", true.into()),
                field => (field, value.clone()),
            };
            let twice = line.insert(field.to_owned(), value);
            assert!(twice.is_none(), "{name}: {field} twice");
        }
        line.entry("timed").or_insert(false.into());
        Value::Object(line)
    });
    lines.collect()
}

#[test]
fn every_recorded_review_is_made_alike() {
    let recording = recorded();
    let scratch = Scratch::new("lr");
    let git = host_git();
    let (mut passed, mut refused) = (0, 0);
    for (index, case) in list(&recording["reviews"]).iter().enumerate() {
        let name = text(&case["name"]);
        let staged = staged(&scratch.0.join(index.to_string()), &git, &recording, case);
        let vendored = case["includeVendor"] == true;
        let (judged, context) = staged.judged(CONTROLLER, vendored);
        let failure = judged.err().map(|failure| failure.to_string());
        let expected = case["result"].get("code").map(|code| named(text(code)));
        assert_eq!(failure.as_deref(), expected, "{name}");
        if expected.is_none() {
            passed += 1;
        } else {
            refused += 1;
        }
        assert_eq!(lines(&context, name), case["checks"], "{name}");
        assert_eq!(json!(staged.receipt()), case["receipt"], "{name}");
        let receipt = staged.output.join("dependency-licenses.json");
        let mode = receipt
            .metadata()
            .ok()
            .map(|metadata| metadata.permissions().mode() & 0o777);
        assert_eq!(json!(mode), case["receiptMode"], "{name}");
        let mut evidence: Vec<String> = std::fs::read_dir(&staged.output)
            .expect("the output")
            .map(|entry| {
                let entry = entry.expect("an entry").file_name();
                entry.into_string().expect("text")
            })
            .filter(|entry| entry != "home")
            .collect();
        evidence.sort();
        assert_eq!(json!(evidence), case["evidence"], "{name}");
        let printed = evidence.iter().filter(|kept| kept.ends_with(".stdout"));
        let printed: Map<String, Value> = printed
            .map(|kept| {
                let said = std::fs::read(staged.output.join(kept)).expect("evidence");
                (kept.clone(), crate::digest(&said).into())
            })
            .collect();
        assert_eq!(Value::Object(printed), case["printed"], "{name}");
        assert_eq!(json!(staged.addresses()), case["addresses"], "{name}");
    }
    assert_eq!((passed, refused), (29, 31));
}

/// What no recording can hold: the reviewed wheel's metadata has one exact
/// SHA-256, and no metadata written for a test has it. The same recorded
/// repository is reviewed again with the wheel's review bound to the
/// metadata the scripted registry serves, and with any one of its other
/// conditions changed.
#[test]
fn the_reviewed_wheel_passes_only_as_it_was_reviewed() {
    let recording = recorded();
    let scratch = Scratch::new("lc");
    let git = host_git();
    let cases = list(&recording["reviews"]);
    let found = cases.iter().find(|case| {
        text(&case["name"]) == "the controller's linter with metadata that nobody reviewed"
    });
    let case = found.expect("the recorded case");
    let receipt: Value =
        serde_json::from_str(text(&case["receipt"])).expect("the recorded receipt");
    let served = text(&receipt["dependencies"][0]["evidence"][0]["metadataSha256"]);
    let bound = Controller {
        metadata: served,
        ..CONTROLLER
    };
    let variants = [
        ("as it was reviewed", bound, None),
        (
            "for another lock",
            Controller {
                manifest: "build/python-requirements.txt",
                ..bound
            },
            Some("dependency_license_not_allowed"),
        ),
        (
            "for other metadata",
            CONTROLLER,
            Some("dependency_license_not_allowed"),
        ),
    ];
    for (index, (name, controller, expected)) in variants.into_iter().enumerate() {
        let staged = staged(&scratch.0.join(index.to_string()), &git, &recording, case);
        let (judged, context) = staged.judged(controller, false);
        let failure = judged.err().map(|failure| failure.to_string());
        assert_eq!(failure.as_deref(), expected, "{name}");
        let kept: Value =
            serde_json::from_str(&staged.receipt().expect("a receipt")).expect("a receipt");
        assert_eq!(kept["passed"], expected.is_none(), "{name}");
        let license = &kept["dependencies"][0]["evidence"][0]["license"];
        assert_eq!(license, "GPL-3.0-or-later", "{name}");
        let status = i64::from(expected.is_some());
        assert_eq!(lines(&context, name)[0]["exitStatus"], status, "{name}");
    }
}

/// The evidence a recorded row stands for, when there is such evidence:
/// a wheel is evidence with a licence and both hashes.
fn evidence(row: &Value) -> Option<Evidence> {
    let said = |field: &str| row.get(field).map(|value| text(value).to_owned());
    match row.get("kind").map(text) {
        Some("wheel") => Some(Evidence::Wheel {
            license: said("license")?,
            source: said("source")?,
            wheel: said("wheelSha256")?,
            metadata: said("metadataSha256")?,
        }),
        Some(_) => Some(Evidence::Sdist {
            source: said("source")?,
            artifact: said("wheelSha256")?,
        }),
        None => Some(Evidence::Npm {
            license: said("license")?,
            source: said("source")?,
            metadata: said("metadataSha256")?,
        }),
    }
}

#[test]
fn the_reviewed_wheel_of_the_controller_is_judged_alike() {
    let recording = recorded();
    let (mut reviewed, mut refused, mut unrepresentable) = (0, 0, 0);
    for row in list(&recording["controller"]) {
        let name = text(&row["name"]);
        let fields = &row["dependency"];
        let ecosystem = if fields["ecosystem"] == "npm" {
            "npm"
        } else {
            "pypi"
        };
        let dependency = Dependency {
            ecosystem,
            manifest: text(&fields["manifest"]).to_owned(),
            name: text(&fields["name"]).to_owned(),
            version: text(&fields["version"]).to_owned(),
            identity: "identity".to_owned(),
            hashes: [CONTROLLER.wheel.to_owned()].into(),
            license: String::new(),
            source: String::new(),
        };
        let Some(evidence) = evidence(&row["evidence"]) else {
            // A wheel without a licence or a hash is not evidence here.
            assert_eq!(row["reviewed"], false, "{name}");
            unrepresentable += 1;
            continue;
        };
        let judged = CONTROLLER.reviewed(&dependency, &evidence);
        assert_eq!(json!(judged), row["reviewed"], "{name}");
        if judged {
            reviewed += 1;
        } else {
            refused += 1;
        }
    }
    assert_eq!((reviewed, refused, unrepresentable), (1, 10, 3));
}
