//! Replays of `recorded_tier.json`: what the reference's command line did
//! with arguments that end an invocation before a scanner runs, and the
//! summaries its own writer wrote for checks it was handed.
//!
//! The recorder called the reference's `main` in a scratch directory, with
//! the evidence named there, and kept the status, the line it printed, the
//! summary's text and what was left. For the summaries it replaced the
//! tiers by one that records the given checks and ends as told.

use super::{Host, NATIVE, Request, VENDOR, announced, deep, gate, parse, ruled, summary};
use crate::{
    Fault, Result,
    context::{Context, Failure, Limits, Note, Outcome},
    fast::Programs,
    mutation::Mutation,
    scratch::{Scratch, kept},
};
use serde_json::{Value, json};
use std::{
    cell::RefCell,
    ffi::OsString,
    fs::{DirBuilder, Permissions},
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const RECORDING: &str = include_str!("recorded_tier.json");

/// Command lines the reference's reader took and judged, and this one
/// refuses: a value that is empty, an option given twice (the last one
/// counted), one before the tier, one cut short and one joined to its
/// value.
const STRICTER: [&str; 5] = [
    "a base that is empty",
    "an option given twice",
    "an option before the tier",
    "an option cut short",
    "an option joined to its value",
];

/// Command lines with the words of the image tier, which the reference
/// judged by that tier's rules. They are no words of this command until it
/// has that tier.
const ELSEWHERE: [&str; 13] = [
    "an image for the fast tier",
    "an image for the deep tier",
    "the image tier without an image",
    "the image tier with the vendored tree",
    "the image tier without an image and with the vendored tree",
    "the image tier with the vendored tree where automation runs",
    "image tools for the fast tier",
    "image tools for the deep tier",
    "an image database for the fast tier",
    "an image and image tools for the fast tier",
    "image tools and an image database for the fast tier",
    "an image database and a deep check for the fast tier",
    "a directory of artifacts for the fast tier",
];

/// A summary whose text differs and whose content is the same: the fields
/// of a record inside a check are written in the order of their names.
const OTHERWISE: [&str; 1] = ["a diagnosis whose findings name their fields in another order"];

fn text(value: &Value) -> &str {
    value.as_str().expect("a recorded string")
}

/// The code this gate has for what the reference raised.
fn named(class: &str) -> &'static str {
    match class {
        "FileExistsError" => "tool_entry_unavailable",
        "FileNotFoundError" => "tool_entry_missing",
        "JSONDecodeError" => "invalid_tool_json",
        "KeyError" => "invalid_tool_string",
        other => panic!("{other} is not named"),
    }
}

/// A host with an environment and nothing else: no command line here gets
/// as far as a scanner.
struct Bare(Vec<(String, OsString)>);

impl Host for Bare {
    fn ambient(&self, name: &str) -> Option<OsString> {
        let held = self.0.iter().find(|(held, _)| held == name);
        held.map(|(_, value)| value.clone())
    }

    fn now(&self) -> SystemTime {
        UNIX_EPOCH
    }

    fn scanner(&self, _: &'static str) -> Result<PathBuf> {
        Err(Fault("a_scanner_was_asked_for"))
    }

    fn engine(&self) -> Result<(String, String)> {
        Err(Fault("the_engine_was_asked_for"))
    }

    fn platform(&self) -> Result<&'static str> {
        Err(Fault("the_platform_was_asked_for"))
    }

    fn mutants(&self, _: &Path, _: &str) -> Result<()> {
        Err(Fault("the_mutation_tool_was_asked_for"))
    }

    fn mutant(&self, _: &Path, _: &str) -> Result<PathBuf> {
        Err(Fault("the_mutation_tool_was_asked_for"))
    }

    fn mutation(&self, _: &Mutation<'_>) -> Outcome<bool> {
        Err(Failure::Fixed("the_mutation_check_was_asked_for"))
    }
}

/// The request of a recorded command line, with the evidence at `output`.
fn asked(case: &Value, output: &Path) -> Option<Request> {
    let arguments = case["arguments"].as_array().expect("arguments");
    let mut arguments: Vec<String> = arguments.iter().map(|word| text(word).to_owned()).collect();
    arguments.extend(["--output".to_owned(), output.display().to_string()]);
    let (tier, rest) = arguments.split_first().expect("a word");
    parse(tier, rest)
}

#[test]
fn command_lines_end_as_the_reference_ended_them() {
    let recording: Value = serde_json::from_str(RECORDING).expect("a recording");
    // Judged alike, refused alike before anything is kept, no command line
    // for either, stricter here, and the image tier's.
    let mut tally = (0, 0, 0, 0, 0);
    for case in recording["cases"].as_array().expect("cases") {
        let name = text(&case["name"]);
        let scratch = Scratch::new("tier-lines");
        let place = scratch.0.join("case");
        for made in case["made"].as_array().expect("directories") {
            std::fs::create_dir_all(place.join(text(made))).expect("a directory");
        }
        std::fs::create_dir_all(&place).expect("a place");
        if let Some([link, target]) = case["link"].as_array().map(Vec::as_slice) {
            std::os::unix::fs::symlink(text(target), place.join(text(link))).expect("a link");
        }
        let target = place.join(text(&case["output"]));
        let request = asked(case, &target);
        let recorded = &case["result"];
        if recorded["exit"] == 2 {
            assert_eq!(request, None, "{name}");
            tally.2 += 1;
            continue;
        }
        let refused = (STRICTER.contains(&name), ELSEWHERE.contains(&name));
        if refused.0 || refused.1 {
            assert_eq!(
                (request, &recorded["status"]),
                (None, &Value::from(1)),
                "{name}"
            );
            tally.3 += usize::from(refused.0);
            tally.4 += usize::from(refused.1);
            continue;
        }
        let request = request.unwrap_or_else(|| panic!("{name}: no command line"));
        let mut variables = vec![
            ("HOME".to_owned(), place.join("caller").into_os_string()),
            ("PATH".to_owned(), OsString::from("/usr/bin:/bin")),
        ];
        if let Some(marker) = case["marker"].as_str() {
            variables.push((marker.to_owned(), text(&case["value"]).into()));
        }
        let outcome = gate(&place, &request, &Bare(variables));
        if let Some(class) = recorded["raised"].as_str() {
            let failure = outcome.expect_err(name).to_string();
            assert_eq!(failure, named(class), "{name}");
            assert_eq!(kept(&target, &[]), case["kept"], "{name}");
            tally.1 += 1;
            continue;
        }
        let (passed, output) = outcome.unwrap_or_else(|failure| panic!("{name}: {failure}"));
        assert_eq!(i64::from(!passed), recorded["status"], "{name}");
        let line = String::from_utf8(announced(&request, passed, &output)).expect("text");
        let line = line.replace(&place.display().to_string(), "<place>");
        assert_eq!(line, text(&case["printed"]), "{name}");
        let written = std::fs::read_to_string(output.join("summary.json")).expect("a summary");
        assert_eq!(written, text(&case["summary"]), "{name}");
        assert_eq!(kept(&target, &[]), case["kept"], "{name}");
        let mode = target
            .metadata()
            .expect("the evidence")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(u64::from(mode), case["outputMode"], "{name}");
        tally.0 += 1;
    }
    assert_eq!(tally, (20, 2, 13, 5, 13));
}

/// The recorded fields of a check, in their order.
fn note(fields: &Value) -> Note {
    let fields = fields.as_array().expect("fields").iter();
    let field = |pair: &Value| {
        let name: &'static str = Box::leak(text(&pair[0]).to_owned().into_boxed_str());
        (name, pair[1].clone())
    };
    fields.map(field).collect()
}

#[test]
fn summaries_are_written_as_the_reference_wrote_them() {
    let recording: Value = serde_json::from_str(RECORDING).expect("a recording");
    // Passed and failed, each written alike, and written otherwise.
    let mut tally = (0, 0, 0);
    for case in recording["summaries"].as_array().expect("summaries") {
        let name = text(&case["name"]);
        let request = asked(case, Path::new("/evidence")).expect("a command line");
        let checks: Vec<Note> = case["checks"]
            .as_array()
            .expect("checks")
            .iter()
            .map(note)
            .collect();
        let failure = case["failure"].as_str();
        let written = summary(&request, failure, &checks);
        let recorded = text(&case["summary"]);
        let alike = |text: &str| serde_json::from_str::<Value>(text).expect("a summary");
        assert_eq!(alike(&written), alike(recorded), "{name}");
        let passed = failure.is_none();
        assert_eq!(i64::from(!passed), case["result"]["status"], "{name}");
        let line = String::from_utf8(announced(&request, passed, Path::new("/evidence")));
        let line = line
            .expect("text")
            .replace("/evidence", "<place>/evidence/gate");
        assert_eq!(line, text(&case["printed"]), "{name}");
        if OTHERWISE.contains(&name) {
            assert_ne!(written, recorded, "{name}");
            tally.2 += 1;
            continue;
        }
        assert_eq!(written, recorded, "{name}");
        if passed {
            tally.0 += 1;
        } else {
            tally.1 += 1;
        }
    }
    assert_eq!(tally, (7, 8, 1));
}

/// A host of the deep tier's own checks: an environment, a mutation tool
/// that is installed or not as the case says, and a mutation check that
/// ends as the case says. What it is asked is kept.
struct Deep<'a> {
    variables: Vec<(&'static str, OsString)>,
    install: Option<&'static str>,
    passes: bool,
    root: &'a str,
    place: &'a str,
    installs: RefCell<Vec<Value>>,
    handed: RefCell<Vec<Value>>,
}

impl Deep<'_> {
    fn spelled(&self, path: &Path) -> String {
        let path = path.display().to_string();
        path.replace(self.root, "<root>")
            .replace(self.place, "<place>")
    }
}

impl Host for Deep<'_> {
    fn ambient(&self, name: &str) -> Option<OsString> {
        let held = self.variables.iter().find(|(held, _)| *held == name);
        held.map(|(_, value)| value.clone())
    }

    fn now(&self) -> SystemTime {
        UNIX_EPOCH
    }

    fn scanner(&self, _: &'static str) -> Result<PathBuf> {
        Err(Fault("a_scanner_was_asked_for"))
    }

    fn engine(&self) -> Result<(String, String)> {
        Err(Fault("the_engine_was_asked_for"))
    }

    fn platform(&self) -> Result<&'static str> {
        Ok("linux-aarch64")
    }

    fn mutants(&self, directory: &Path, platform: &str) -> Result<()> {
        self.installs.borrow_mut().push(json!({
            "names": ["cargo-mutants"],
            "directory": self.spelled(directory),
            "platform": platform,
        }));
        self.install.map_or(Ok(()), |code| Err(Fault(code)))
    }

    fn mutant(&self, directory: &Path, platform: &str) -> Result<PathBuf> {
        self.handed.borrow_mut().extend([
            "--tools-directory".into(),
            self.spelled(directory).into(),
            "--tool-platform".into(),
            platform.into(),
        ]);
        Ok(directory.join("bin/cargo-mutants"))
    }

    fn mutation(&self, options: &Mutation<'_>) -> Outcome<bool> {
        (options.tool)()?;
        assert_eq!(options.darwin, cfg!(target_os = "macos"));
        let (given, reviewed) = (options.bounds, crate::mutation::Bounds::REVIEWED);
        assert_eq!(
            (
                given.workspace,
                given.evidence,
                given.entries,
                given.reserve
            ),
            (
                reviewed.workspace,
                reviewed.evidence,
                reviewed.entries,
                reviewed.reserve
            )
        );
        assert_eq!(
            (options.ambient)("HOME"),
            Some(
                options
                    .root
                    .join("../evidence/gate/home")
                    .canonicalize()
                    .expect("a home")
                    .into()
            )
        );
        self.handed.borrow_mut().extend([
            "--openssl-prefix".into(),
            self.spelled(options.openssl).into(),
            "--output".into(),
            self.spelled(options.output).into(),
        ]);
        Ok(self.passes)
    }
}

/// The interpreter of a case and nothing else.
struct Interpreter(PathBuf);

impl Programs for Interpreter {
    fn program(&self, _: &'static str) -> Outcome<PathBuf> {
        Err(Failure::Fixed("a_scanner_was_asked_for"))
    }

    fn prerequisite(&self, _: &'static str) -> Outcome<PathBuf> {
        Err(Failure::Fixed("a_program_of_the_host_was_asked_for"))
    }

    fn python(&self) -> Outcome<PathBuf> {
        Ok(self.0.clone())
    }
}

/// The commands that the interpreter of a case logged.
fn logged(log: &Path, root: &str, place: &str) -> Vec<Value> {
    let lines = std::fs::read_to_string(log).unwrap_or_default();
    let call = |line: &str| {
        let bytes: Vec<u8> = (0..line.len() / 2)
            .map(|at| u8::from_str_radix(&line[2 * at..2 * at + 2], 16).expect("hex"))
            .collect();
        let words = bytes
            .strip_suffix(&[0])
            .expect("an end")
            .split(|byte| *byte == 0);
        let spelled = |word: &[u8]| {
            let word = String::from_utf8_lossy(word);
            word.replace(root, "<root>").replace(place, "<place>")
        };
        json!(words.map(spelled).collect::<Vec<_>>())
    };
    lines.lines().map(call).collect()
}

#[test]
fn the_deep_tier_runs_its_own_checks_as_the_reference_ran_them() {
    let recording: Value = serde_json::from_str(RECORDING).expect("a recording");
    // Passed and failed alike, and failed where the reference raised.
    let mut tally = (0, 0, 0);
    for case in recording["deep"].as_array().expect("cases") {
        let name = text(&case["name"]);
        let scratch = Scratch::new("tier-deep");
        let place = scratch.0.join("case");
        let (root, target) = (place.join("root"), place.join("evidence/gate"));
        for made in ["root/target/openssl-4.1.0", "evidence", "static"] {
            std::fs::create_dir_all(place.join(made)).expect("a directory");
        }
        std::os::unix::fs::symlink("static", place.join("linked")).expect("a link");
        DirBuilder::new()
            .mode(0o700)
            .create(&target)
            .expect("the evidence");
        let spelled = place.display().to_string();
        let (python, log) = (place.join("python"), place.join("log"));
        let script = text(&case["script"]).replace("<log>", &log.display().to_string());
        std::fs::write(&python, script).expect("an interpreter");
        std::fs::set_permissions(&python, Permissions::from_mode(0o755)).expect("a mode");
        let arguments = case["arguments"].as_array().expect("arguments").iter();
        let arguments: Vec<String> = arguments
            .map(|word| text(word).replace("<place>", &spelled))
            .collect();
        let request = parse(&arguments[0], &arguments[1..]).expect("a command line");
        let mut variables = vec![
            ("HOME", place.join("caller").into_os_string()),
            ("PATH", OsString::from("/usr/bin:/bin")),
        ];
        if let Some(named) = case["ambient"]["OPENSSL_DIR"].as_str() {
            variables.push(("OPENSSL_DIR", named.replace("<place>", &spelled).into()));
        }
        let install = case["install"].as_str();
        let host = Deep {
            variables,
            install: install.map(|code| &*Box::leak(code.to_owned().into_boxed_str())),
            passes: case["statuses"]["mutation"] == 0,
            root: &root.display().to_string(),
            place: &spelled,
            installs: RefCell::default(),
            handed: RefCell::default(),
        };
        let ambient = |name: &str| host.ambient(name);
        let mut context =
            Context::new(&root, &target, Limits::REVIEWED, &ambient).expect("a context");
        let programs = Interpreter(python);
        let judged = ruled(&request, &host)
            .map_err(Failure::from)
            .and_then(|()| deep(&mut context, &request, &host, &programs, Path::new("git")));
        let failure = judged.err().map(|failure| failure.to_string());
        let recorded = case["summary"]["failure"].as_str();
        assert_eq!(i64::from(failure.is_some()), case["status"], "{name}");
        let written = summary(&request, failure.as_deref(), context.checks());
        let mut written: Value = serde_json::from_str(&written).expect("a summary");
        for check in written["checks"].as_array_mut().expect("checks") {
            check
                .as_object_mut()
                .expect("a check")
                .remove("elapsedSeconds");
        }
        let raised = recorded.is_some_and(|code| code.ends_with("Error"));
        if raised {
            written["failure"] = recorded.into();
            assert_eq!(failure.as_deref(), recorded.map(named), "{name}");
        }
        assert_eq!(written, case["summary"], "{name}");
        // The mutation check is no command here: what the reference gave
        // its command is what the check is handed.
        let mut calls = logged(&log, &root.display().to_string(), &spelled);
        let mutation = |call: &&Value| call[0] == "build/security_mutation.py";
        let there = case["calls"].as_array().expect("calls");
        if let Some(call) = there.iter().find(mutation) {
            let mut handed = vec![Value::from("build/security_mutation.py")];
            handed.extend(host.handed.borrow().iter().cloned());
            calls.push(handed.into());
            assert_eq!(calls.last(), Some(call), "{name}");
        }
        assert_eq!(json!(calls), case["calls"], "{name}");
        assert_eq!(json!(*host.installs.borrow()), case["installs"], "{name}");
        let left = kept(&target, &["home"]);
        let mut left: Vec<&String> = left.as_object().expect("entries").keys().collect();
        left.retain(|entry| *entry != "home");
        // The summary is written by the gate, which is not what runs here.
        let mut there: Vec<&str> = case["kept"]
            .as_array()
            .expect("entries")
            .iter()
            .map(text)
            .collect();
        there.retain(|entry| *entry != "summary.json");
        assert_eq!(json!(left), json!(there), "{name}");
        match (failure.is_some(), raised) {
            (false, _) => tally.0 += 1,
            (true, false) => tally.1 += 1,
            (true, true) => tally.2 += 1,
        }
    }
    assert_eq!(tally, (5, 23, 5));
}

#[test]
fn the_slow_checks_have_their_reviewed_time() {
    assert_eq!(VENDOR, Duration::from_secs(900));
    assert_eq!(NATIVE, Duration::from_secs(1800));
}

#[test]
fn every_word_of_a_command_line_is_kept() {
    let words = [
        "--mutation-tool-platform",
        "darwin-x86_64",
        "--openssl-prefix",
        "a prefix",
        "--deep-check",
        "mutation",
        "--engine",
        "podman",
        "--include-vendor",
        "--output",
        "evidence",
        "--base",
        "a commit",
    ];
    let words = words.map(str::to_owned);
    let request = Request {
        deep: true,
        base: Some("a commit".to_owned()),
        output: Some(PathBuf::from("evidence")),
        include_vendor: true,
        engine: "podman",
        check: "mutation",
        openssl: Some(PathBuf::from("a prefix")),
        platform: Some("darwin-x86_64"),
    };
    assert_eq!(parse("deep", &words), Some(request));
    let usual = Request {
        deep: false,
        base: None,
        output: None,
        include_vendor: false,
        engine: "docker",
        check: "all",
        openssl: None,
        platform: None,
    };
    assert_eq!(parse("fast", &[]), Some(usual.clone()));
    for (word, check, platform) in [
        ("native", "native", "linux-x86_64"),
        ("all", "all", "darwin-x86_64"),
    ] {
        let words = ["--deep-check", word, "--mutation-tool-platform", platform];
        let asked = parse("deep", &words.map(str::to_owned)).expect("a command line");
        assert_eq!((asked.check, asked.platform), (check, Some(platform)));
        assert_eq!((asked.engine, asked.deep), ("docker", true));
    }
}

/// A request for the fast tier that ends at its rules, with the evidence
/// where `output` says.
fn refused(output: Option<&str>) -> Request {
    let words = ["--deep-check", "mutation"].map(str::to_owned);
    let mut request = parse("fast", &words).expect("a command line");
    request.output = output.map(PathBuf::from);
    request
}

fn caller(place: &Path) -> Bare {
    Bare(vec![
        ("HOME".to_owned(), place.join("caller").into_os_string()),
        ("PATH".to_owned(), OsString::from("/usr/bin:/bin")),
    ])
}

fn mode(path: &Path) -> u32 {
    let entry = path.symlink_metadata().expect("an entry");
    entry.permissions().mode() & 0o7777
}

#[test]
fn evidence_that_is_not_named_is_new_and_private_below_the_results() {
    let scratch = Scratch::new("tier-fresh");
    let host = caller(&scratch.0);
    let results = scratch.0.join("results");
    let mut made = Vec::new();
    for _ in 0..3 {
        let (passed, output) = gate(&scratch.0, &refused(None), &host).expect("a gate");
        assert!(!passed);
        assert_eq!(output.parent(), Some(results.as_path()));
        let name = output.file_name().expect("a name").to_string_lossy();
        assert!(name.starts_with("security.fast."), "{name}");
        assert_eq!((mode(&results), mode(&output)), (0o700, 0o700));
        assert_eq!(mode(&output.join("summary.json")), 0o600);
        assert!(!made.contains(&output));
        made.push(output);
    }
    let deep = parse("deep", &["--deep-check".to_owned(), "native".to_owned()]);
    let (_, output) = gate(&scratch.0, &deep.expect("a command line"), &host).expect("a gate");
    let name = output.file_name().expect("a name").to_string_lossy();
    assert!(name.starts_with("security.deep."), "{name}");
}

#[test]
fn evidence_is_named_from_the_sources_and_needs_no_results() {
    let scratch = Scratch::new("tier-named");
    let host = caller(&scratch.0);
    let (_, output) = gate(&scratch.0, &refused(Some("gate")), &host).expect("a gate");
    assert_eq!(output, scratch.0.join("gate"));
    assert!(!scratch.0.join("results").exists());
}

#[test]
fn results_that_are_no_directory_are_refused() {
    let scratch = Scratch::new("tier-results");
    std::fs::write(scratch.0.join("results"), "").expect("a file");
    let failure = gate(&scratch.0, &refused(None), &caller(&scratch.0)).expect_err("no evidence");
    assert_eq!(failure.to_string(), "tool_entry_unavailable");
    std::fs::remove_file(scratch.0.join("results")).expect("removed");
    std::os::unix::fs::symlink("nowhere", scratch.0.join("results")).expect("a link");
    let failure = gate(&scratch.0, &refused(None), &caller(&scratch.0)).expect_err("no evidence");
    assert_eq!(failure.to_string(), "tool_entry_unavailable");
}

#[test]
fn an_invocation_without_a_home_has_no_summary() {
    let scratch = Scratch::new("tier-home");
    let host = Bare(vec![("PATH".to_owned(), OsString::from("/usr/bin:/bin"))]);
    let failure = gate(&scratch.0, &refused(Some("gate")), &host).expect_err("no home");
    assert_eq!(failure.to_string(), "security_home_unset");
    assert!(!scratch.0.join("gate/summary.json").exists());
}

#[test]
fn a_preferred_directory_is_searched_first() {
    let scratch = Scratch::new("tier-prefer");
    let host = caller(&scratch.0);
    let ambient = |name: &str| host.ambient(name);
    let mut context = Context::new(&scratch.0, &scratch.0, Limits::REVIEWED, &ambient);
    let context = context.as_mut().expect("a context");
    context.prefer(Path::new("/pinned/bin"));
    let search = context.variable("PATH").expect("a search path");
    assert_eq!(search, "/pinned/bin:/usr/bin:/bin");
    assert_eq!(
        context.variable("HOME"),
        Some(scratch.0.join("home").as_os_str())
    );
}

#[test]
fn the_lock_names_a_tool_s_version_and_itself() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let lock = root.join("build/security-tools.lock.json");
    let content = std::fs::read(&lock).expect("the lock");
    let parsed: Value = serde_json::from_slice(&content).expect("a document");
    for name in ["semgrep-core", "cargo-mutants"] {
        let (version, digest) = crate::tools::locked(&lock, name).expect("a locked tool");
        assert_eq!(version, text(&parsed["tools"][name]["version"]));
        assert_eq!(digest, crate::digest(&content));
    }
    let unknown = crate::tools::locked(&lock, "no-such-tool");
    assert_eq!(unknown, Err(Fault("unknown_tool")));
    let missing = crate::tools::locked(&root.join("no-such-lock.json"), "semgrep-core");
    assert_eq!(missing, Err(Fault("tool_entry_missing")));
    let host = crate::tools::host().expect("a platform the lock names");
    let (system, machine) = host.split_once('-').expect("two parts");
    assert_eq!(
        system,
        if cfg!(target_os = "macos") {
            "darwin"
        } else {
            "linux"
        }
    );
    assert!(
        ["x86_64", "aarch64", "arm64"].contains(&machine),
        "{machine}"
    );
}

#[test]
fn a_moment_has_its_day() {
    let day = |seconds: u64| crate::policy::Date::at(UNIX_EPOCH + Duration::from_secs(seconds));
    assert_eq!(Some(day(0)), crate::policy::Date::new(1970, 1, 1));
    assert_eq!(Some(day(86_399)), crate::policy::Date::new(1970, 1, 1));
    assert_eq!(Some(day(86_400)), crate::policy::Date::new(1970, 1, 2));
    assert_eq!(
        Some(day(1_790_769_600)),
        crate::policy::Date::new(2026, 9, 30)
    );
    let before = UNIX_EPOCH - Duration::from_secs(1);
    assert_eq!(
        Some(crate::policy::Date::at(before)),
        crate::policy::Date::new(1970, 1, 1)
    );
}
