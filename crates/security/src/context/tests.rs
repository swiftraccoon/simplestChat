//! Replays of the recorded invocation context.
//!
//! `recorded_context.json` holds what `build/security_context.py` answered
//! for child environments, searches of the path, sequences of commands and
//! snapshots of trees built on disk, and how the reference writes a document.
//! Where the reference failed with an exception of its interpreter, `named`
//! gives the code here; `SEARCHED`, `POINTED` and the labels name what is
//! answered differently on purpose.

use super::{Context, Failure, Limits, Run, Snapshot, below_home, environment, executable, mirror};
use crate::{
    Fault,
    record::pretty,
    scratch::{Scratch, base64},
};
use serde_json::{Map, Value, json};
use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

const RECORDED: &str = include_str!("recorded_context.json");

/// A directory of the search path is absolute: the reference also looked
/// below the working directory for an empty or a relative entry.
const SEARCHED: [(&str, Option<&str>); 3] = [
    (
        "an empty entry and the program in the working directory",
        Some("<base>/a/git"),
    ),
    ("a relative entry with the program", Some("<base>/a/git")),
    ("a relative entry alone", None),
];

/// A check's name has no point: the reference dropped what followed the last
/// one from the names of the evidence files.
const POINTED: [&str; 3] = [
    "a name with a point",
    "a name that ends with a point",
    "a name that starts with a point",
];

fn recorded() -> Value {
    serde_json::from_str(RECORDED).expect("the recording is JSON")
}

fn text(value: &Value) -> &str {
    value.as_str().expect("a recorded string")
}

fn list(value: &Value) -> &[Value] {
    value.as_array().expect("a recorded list")
}

fn spelled(path: &Path) -> &str {
    path.to_str().expect("a path that is text")
}

/// A recorded name as the fixed text a check's name is.
fn fixed(name: &str) -> &'static str {
    Box::leak(name.to_owned().into_boxed_str())
}

fn mode(path: &Path) -> u32 {
    let metadata = path.symlink_metadata().expect("an entry");
    metadata.permissions().mode() & 0o777
}

fn named<'a>(code: &'a str, group: &str) -> &'a str {
    match (code, group) {
        ("FileNotFoundError", _) => "tool_entry_missing",
        ("OSError" | "IsADirectoryError", "snapshots") => "security_source_type",
        ("UnicodeDecodeError", _) => "security_source_path",
        ("FileExistsError" | "PermissionError" | "OSError", _) => "tool_entry_unavailable",
        (code, _) => code,
    }
}

/// Counts of the cases answered alike, refused alike and answered differently
/// on purpose.
#[derive(Debug, Default, PartialEq, Eq)]
struct Tally(usize, usize, usize);

fn caller(name: &str) -> Option<OsString> {
    match name {
        "PATH" => std::env::var_os("PATH"),
        "HOME" => Some("/h".into()),
        _ => None,
    }
}

#[test]
fn every_recorded_document_is_written_alike() {
    let recording = recorded();
    let documents = list(&recording["documents"]);
    for document in documents {
        assert_eq!(pretty(&document["value"]), text(&document["text"]));
    }
    assert_eq!(documents.len(), 18);
}

#[test]
fn every_recorded_environment_is_built_alike() {
    let scratch = Scratch::new("ce");
    let mut tally = Tally::default();
    for (index, case) in list(&recorded()["environments"]).iter().enumerate() {
        let name = text(&case["name"]);
        let output = scratch.0.join(index.to_string());
        let home = output.join("home");
        std::fs::create_dir(&output).expect("an output directory");
        if case["existing"] == true {
            std::fs::create_dir(&home).expect("a home");
        }
        let ambient = |name: &str| case["ambient"].get(name).map(|value| text(value).into());
        let built = environment(&home, &ambient, case["linux"] == true);
        if let Some(code) = case["result"].get("code") {
            let refused = built.err().map(|failure| failure.to_string());
            assert_eq!(refused.as_deref(), Some(named(text(code), "")), "{name}");
            tally.1 += 1;
            continue;
        }
        let variables: BTreeMap<String, String> = built
            .expect(name)
            .into_iter()
            .map(|(name, value)| {
                let value = value.into_string().expect("a value that is text");
                (name.to_owned(), value.replace(spelled(&output), "<output>"))
            })
            .collect();
        let expected: BTreeMap<String, String> = case["result"]["variables"]
            .as_object()
            .expect("variables")
            .iter()
            .map(|(name, value)| (name.clone(), text(value).to_owned()))
            .collect();
        assert_eq!(variables, expected, "{name}");
        assert_eq!(json!(mode(&home)), case["result"]["mode"], "{name}");
        tally.0 += 1;
    }
    assert_eq!(tally, Tally(24, 1, 0));
}

#[test]
fn without_a_home_both_tool_homes_must_be_given() {
    let scratch = Scratch::new("ch");
    let partly = |name: &str| (name == "CARGO_HOME").then(|| OsString::from("/c"));
    let refused = environment(&scratch.0.join("a"), &partly, true);
    assert_eq!(refused, Err(Failure::Fixed("security_home_unset")));
    let neither = environment(&scratch.0.join("b"), &|_| None, true);
    assert_eq!(neither, Err(Failure::Fixed("security_home_unset")));
    let other = |name: &str| (name == "RUSTUP_HOME").then(|| OsString::from("/r"));
    let refused = environment(&scratch.0.join("c"), &other, false);
    assert_eq!(refused, Err(Failure::Fixed("security_home_unset")));
    assert_eq!(below_home(OsStr::new("/h"), ".cargo"), "/h/.cargo");
}

#[test]
fn every_recorded_search_is_answered_alike() {
    let scratch = Scratch::new("cs");
    let mut tally = Tally::default();
    for (index, case) in list(&recorded()["executables"]).iter().enumerate() {
        let name = text(&case["name"]);
        let base = scratch.0.join(index.to_string());
        for (relative, kind) in case["layout"].as_object().expect("a layout") {
            let target = base.join(relative);
            std::fs::create_dir_all(target.parent().expect("a parent")).expect("directories");
            match text(kind) {
                "directory" => std::fs::create_dir(&target).expect("a directory"),
                "link" => std::os::unix::fs::symlink("real", &target).expect("a link"),
                kind => {
                    std::fs::write(&target, "#!/bin/sh\n").expect("a file");
                    let bits = if kind == "program" { 0o755 } else { 0o644 };
                    let permissions = std::fs::Permissions::from_mode(bits);
                    std::fs::set_permissions(&target, permissions).expect("a mode");
                }
            }
        }
        let entries: Vec<String> = list(&case["path"])
            .iter()
            .map(|entry| match text(entry) {
                entry if entry.is_empty() || entry.starts_with("rel") => entry.to_owned(),
                entry => format!("{}/{entry}", spelled(&base)),
            })
            .collect();
        let program = match text(&case["program"]) {
            "git" => "git",
            "npm" => "npm",
            other => panic!("an unknown program {other}"),
        };
        let found = executable(program, Some(OsStr::new(&entries.join(":"))));
        let answer = found
            .map(|path| spelled(&path).replace(spelled(&base), "<base>"))
            .map_err(|failure| failure.to_string());
        let reference = &case["result"];
        if let Some((_, different)) = SEARCHED.iter().find(|(case, _)| *case == name) {
            assert_ne!(answer.as_deref().ok(), reference.get("path").map(text));
            assert_eq!(answer.as_deref().ok(), *different, "{name}");
            tally.2 += 1;
        } else if let Some(code) = reference.get("code") {
            let refused = answer.as_ref().err().map(String::as_str);
            assert_eq!(refused, Some(text(code)), "{name}");
            tally.1 += 1;
        } else {
            assert_eq!(answer.as_deref().ok(), Some(text(&reference["path"])));
            tally.0 += 1;
        }
    }
    assert_eq!(tally, Tally(10, 7, 3));
}

#[test]
fn a_search_path_that_is_unset_is_the_system_directories() {
    let found = executable("sh", None).expect("a shell");
    assert!(found == Path::new("/usr/bin/sh") || found == Path::new("/bin/sh"));
    assert_eq!(
        executable("sh", Some(OsStr::new(""))),
        Err(Failure::Prerequisite("sh"))
    );
    let missing = executable("no-such-program", None);
    assert_eq!(missing, Err(Failure::Prerequisite("no-such-program")));
}

/// The locale every command is given on this platform.
const LOCALE: &str = if cfg!(target_os = "linux") {
    "C.UTF-8"
} else {
    "en_US.UTF-8"
};

/// What a command said under `locale`, as the recording, made on a Mac, has
/// it.
fn recorded_locale(said: &str, locale: &str) -> String {
    said.replace(locale, "en_US.UTF-8")
}

#[test]
fn what_a_command_says_is_compared_in_the_recorded_locale() {
    let elsewhere = recorded_locale("C.UTF-8|0|1|off|<output>/home|unsetunset", "C.UTF-8");
    assert_eq!(elsewhere, "en_US.UTF-8|0|1|off|<output>/home|unsetunset");
    assert_eq!(recorded_locale("0 C", "C.UTF-8"), "0 C");
    assert_eq!(
        recorded_locale("en_US.UTF-8|0", "en_US.UTF-8"),
        "en_US.UTF-8|0"
    );
    let recording = recorded();
    let printed = |case: &&Value| text(&case["name"]) == "the fixed environment";
    let case = list(&recording["runs"])
        .iter()
        .find(printed)
        .expect("the case");
    let kept = text(&case["files"]["01-variables.stdout"]["text"]);
    assert_eq!(kept, text(&case["results"][0]["output"]));
    assert!(kept.starts_with("en_US.UTF-8|"));
}

/// What an output directory holds besides the home: each file's mode and its
/// text, or its size when it is long.
fn evidence(output: &Path, root: &Path) -> Value {
    let mut files = Map::new();
    for entry in std::fs::read_dir(output).expect("an output directory") {
        let entry = entry.expect("an entry");
        let name = entry
            .file_name()
            .into_string()
            .expect("a name that is text");
        if name == "home" {
            continue;
        }
        let content = std::fs::read(entry.path()).expect("evidence");
        let mut file = json!({"mode": mode(&entry.path())});
        if content.len() <= 256 {
            let text = String::from_utf8(content).expect("evidence that is text");
            let text = text.replace(spelled(root), "<root>");
            let text = text.replace(spelled(output), "<output>");
            file["text"] = recorded_locale(&text, LOCALE).into();
        } else {
            file["bytes"] = content.len().into();
        }
        files.insert(name, file);
    }
    files.into()
}

fn step(context: &mut Context, step: &Value, below: &Path) -> super::Outcome<(i32, Vec<u8>)> {
    let argv: Vec<&str> = list(&step["argv"]).iter().map(text).collect();
    let accepted: Vec<i32> = step.get("accepted").map_or(vec![0], |accepted| {
        let whole = |status: &Value| i32::try_from(status.as_i64().expect("a status"));
        let statuses = list(accepted).iter();
        statuses
            .map(|status| whole(status).expect("a small status"))
            .collect()
    });
    let variables: Vec<(&str, &OsStr)> = step
        .get("variables")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .map(|(name, value)| (name.as_str(), OsStr::new(text(value))))
        .collect();
    let seconds = step.get("timeout").and_then(Value::as_f64);
    let fill = step.get("fill").and_then(Value::as_u64).unwrap_or(0);
    let filled = vec![b'i'; usize::try_from(fill).expect("a small size")];
    let how = Run {
        directory: (step.get("below") == Some(&json!(true))).then_some(below),
        within: seconds.map_or(Run::default().within, Duration::from_secs_f64),
        accepted: &accepted,
        input: step
            .get("input")
            .map_or(&filled, |input| text(input).as_bytes()),
        variables: &variables,
    };
    context.run(fixed(text(&step["check"])), &argv, &how)
}

#[test]
fn every_recorded_command_is_run_alike() {
    let scratch = Scratch::new("cr");
    let mut tally = Tally::default();
    for (index, case) in list(&recorded()["runs"]).iter().enumerate() {
        let name = text(&case["name"]);
        let root = scratch.0.join(format!("{index}r"));
        let output = scratch.0.join(format!("{index}o"));
        std::fs::create_dir_all(root.join("below")).expect("a root");
        std::fs::create_dir(&output).expect("an output directory");
        for existing in list(&case["before"]) {
            let existing = output.join(text(existing));
            std::fs::write(&existing, "").expect("an existing file");
            let private = std::fs::Permissions::from_mode(0o600);
            std::fs::set_permissions(&existing, private).expect("a mode");
        }
        let limits = Limits {
            report: case["report"].as_u64().unwrap_or(Limits::REVIEWED.report),
            grace: Duration::from_millis(200),
            ..Limits::REVIEWED
        };
        let ambient = |name: &str| match name {
            "PATH" => Some(OsString::from("/usr/bin:/bin")),
            other => caller(other),
        };
        let mut context = Context::new(&root, &output, limits, &ambient).expect("a context");
        let mut results = Vec::new();
        for recorded in list(&case["steps"]) {
            results.push(match step(&mut context, recorded, &root.join("below")) {
                Ok((status, printed)) if printed.len() > 256 => {
                    json!({"status": status, "bytes": printed.len()})
                }
                Ok((status, printed)) => {
                    let printed = String::from_utf8(printed).expect("output that is text");
                    let printed = printed.replace(spelled(&root), "<root>");
                    let printed = printed.replace(spelled(&output), "<output>");
                    json!({"status": status, "output": recorded_locale(&printed, LOCALE)})
                }
                Err(failure) => json!({"code": failure.to_string()}),
            });
        }
        if POINTED.contains(&name) {
            assert_eq!(results, [json!({"code": "invalid_scanner_check_name"})]);
            assert!(case["results"][0].get("code").is_none(), "{name}");
            assert_eq!((context.sequence, context.checks().len()), (0, 0), "{name}");
            assert_eq!(evidence(&output, &root), json!({}), "{name}");
            tally.2 += 1;
            continue;
        }
        let expected: Vec<Value> = list(&case["results"])
            .iter()
            .map(|result| match result.get("code") {
                Some(code) => json!({"code": named(text(code), "runs")}),
                None => result.clone(),
            })
            .collect();
        assert_eq!(results, expected, "{name}");
        let mut checks = Vec::new();
        for note in context.checks() {
            let [
                ("name", check),
                ("exitStatus", status),
                ("elapsedSeconds", taken),
            ] = &note[..]
            else {
                panic!("{name}: a check of another shape");
            };
            assert!(
                taken
                    .as_f64()
                    .is_some_and(|taken| (0.0..60.0).contains(&taken))
            );
            checks.push(json!({"name": check, "exitStatus": status}));
        }
        assert_eq!(json!(checks), case["checks"], "{name}");
        assert_eq!(json!(context.sequence), case["sequence"], "{name}");
        assert_eq!(evidence(&output, &root), case["files"], "{name}");
        let refused = expected.iter().any(|result| result.get("code").is_some());
        if refused { tally.1 += 1 } else { tally.0 += 1 }
    }
    assert_eq!(tally, Tally(27, 27, 3));
}

#[test]
fn only_an_absolute_directory_is_searched() {
    let scratch = Scratch::new("cw");
    let tool = scratch.0.join("tool");
    std::fs::write(&tool, "#!/bin/sh\n").expect("a program");
    std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).expect("a mode");
    let here = std::env::current_dir().expect("a working directory");
    let up = "../".repeat(here.components().count() - 1);
    let relative = format!("{up}{}", spelled(&scratch.0).trim_start_matches('/'));
    assert!(Path::new(&relative).join("tool").is_file());
    let refused = executable("tool", Some(OsStr::new(&relative)));
    assert_eq!(refused, Err(Failure::Prerequisite("tool")));
    // An empty entry is the working directory, from which this name is found.
    let reached = fixed(&format!("{relative}/tool"));
    assert!(Path::new(reached).is_file());
    let empty = format!(":{}", spelled(&scratch.0.join("nowhere")));
    let refused = executable(reached, Some(OsStr::new(&empty)));
    assert_eq!(refused, Err(Failure::Prerequisite(reached)));
    assert_eq!(executable("tool", Some(scratch.0.as_os_str())), Ok(tool));
}

fn alive(recorded: &Path) -> bool {
    let recorded = std::fs::read_to_string(recorded).expect("a recorded identity");
    let pid = recorded.trim().parse().expect("a number");
    let pid = rustix::process::Pid::from_raw(pid).expect("a process identity");
    rustix::process::test_kill_process(pid).is_ok()
}

#[test]
fn a_stopped_command_leaves_no_process() {
    let scratch = Scratch::new("cg");
    let limits = Limits {
        grace: Duration::from_millis(300),
        ..Limits::REVIEWED
    };
    let mut context = Context::new(&scratch.0, &scratch.0, limits, &caller).expect("a context");
    let script = "trap '' TERM; sleep 30 & echo $! > child; echo $$ > leader; wait";
    let how = Run {
        within: Duration::from_secs(1),
        ..Run::default()
    };
    let stopped = context.run("stubborn", &["/bin/sh", "-c", script], &how);
    assert_eq!(stopped, Err(Failure::Fixed("command_timed_out")));
    for name in ["leader", "child"] {
        let recorded = scratch.0.join(name);
        let waited = Instant::now();
        while alive(&recorded) && waited.elapsed() < Duration::from_secs(3) {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!alive(&recorded), "{name}");
    }
}

#[test]
fn a_command_past_its_time_is_asked_to_end_before_it_is_killed() {
    let scratch = Scratch::new("ca");
    let limits = Limits {
        grace: Duration::from_secs(30),
        ..Limits::REVIEWED
    };
    let mut context = Context::new(&scratch.0, &scratch.0, limits, &caller).expect("a context");
    let script = "trap 'sleep 0.5; echo asked > asked; exit 0' TERM; sleep 30 & wait";
    let how = Run {
        within: Duration::from_secs(1),
        ..Run::default()
    };
    let started = Instant::now();
    let stopped = context.run("asked", &["/bin/sh", "-c", script], &how);
    let taken = started.elapsed();
    assert_eq!(stopped, Err(Failure::Fixed("command_timed_out")));
    assert!((1.4..8.0).contains(&taken.as_secs_f64()), "{taken:?}");
    let asked = std::fs::read_to_string(scratch.0.join("asked"));
    assert_eq!(asked.expect("the request was seen"), "asked\n");
}

#[test]
fn the_time_taken_is_in_seconds_to_the_millisecond() {
    let scratch = Scratch::new("cm");
    let mut context =
        Context::new(&scratch.0, &scratch.0, Limits::REVIEWED, &caller).expect("a context");
    let ran = context.run("timed", &["/bin/sh", "-c", "sleep 0.3"], &Run::default());
    assert_eq!(ran, Ok((0, Vec::new())));
    let taken = context.checks()[0][2].1.as_f64().expect("seconds");
    assert!((0.25..5.0).contains(&taken), "{taken}");
    let thousandths = taken * 1000.0;
    assert!((thousandths.round() - thousandths).abs() < 1e-6, "{taken}");
}

/// Evidence that cannot be kept.
struct Refusing;

impl std::io::Write for Refusing {
    fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
        Err(std::io::Error::other("no room"))
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn a_command_whose_evidence_cannot_be_kept_is_stopped() {
    let scratch = Scratch::new("cf");
    let context =
        Context::new(&scratch.0, &scratch.0, Limits::REVIEWED, &caller).expect("a context");
    let started = Instant::now();
    let argv = ["/bin/sh", "-c", "printf said; sleep 30"];
    let stopped = context.pump(&argv, &Run::default(), (Refusing, Refusing));
    assert_eq!(stopped, Err(Failure::Fixed("tool_entry_unavailable")));
    assert!(started.elapsed() < Duration::from_secs(10));
    let quiet = context.pump(
        &["/bin/sh", "-c", "exit 4"],
        &Run::default(),
        (Refusing, Refusing),
    );
    assert_eq!(quiet, Ok(4));
}

#[test]
fn a_root_that_is_a_link_is_no_linked_directory() {
    let scratch = Scratch::new("cl");
    std::fs::create_dir_all(scratch.0.join("real/d")).expect("directories");
    std::fs::write(scratch.0.join("real/d/f"), "f\n").expect("a file");
    std::os::unix::fs::symlink("real", scratch.0.join("root")).expect("a link");
    let (copy, manifest) = (scratch.0.join("copy"), scratch.0.join("manifest.json"));
    let root = scratch.0.join("root");
    mirror(b"d/f\0", &root, &copy, &manifest, &Limits::REVIEWED).expect("a copy");
    assert_eq!(std::fs::read(copy.join("d/f")).expect("the copy"), b"f\n");
}

#[test]
fn a_command_needs_a_program() {
    let scratch = Scratch::new("cp");
    let (root, output) = (scratch.0.join("root"), scratch.0.join("output"));
    std::fs::create_dir(&root).expect("a root");
    std::fs::create_dir(&output).expect("an output directory");
    let mut context = Context::new(&root, &output, Limits::REVIEWED, &caller).expect("a context");
    let none: [&str; 0] = [];
    let refused = context.run("nothing", &none, &Run::default());
    assert_eq!(refused, Err(Failure::Fixed("invalid_scanner_command")));
    assert_eq!((context.root(), context.output()), (&*root, &*output));
    context.note(vec![("name", "noted".into()), ("exitStatus", 0.into())]);
    let ran = context.run("says", &["/bin/sh", "-c", "printf said"], &Run::default());
    assert_eq!(ran, Ok((0, b"said".to_vec())));
    let names: Vec<&Value> = context.checks().iter().map(|note| &note[0].1).collect();
    assert_eq!(names, ["noted", "says"]);
}

#[test]
fn the_bounds_are_the_reviewed_ones() {
    let mebibyte = 1024 * 1024;
    assert_eq!(
        Limits::REVIEWED,
        Limits {
            source: 16 * mebibyte,
            tree: 128 * mebibyte,
            files: 20_000,
            report: 16 * mebibyte,
            grace: Duration::from_secs(5),
        }
    );
    let plain = Run::default();
    assert_eq!(
        (plain.within, plain.accepted),
        (Duration::from_secs(180), &[0][..])
    );
    assert!(plain.directory.is_none() && plain.input.is_empty() && plain.variables.is_empty());
}

#[test]
fn a_failure_names_its_check_or_its_program() {
    assert_eq!(Failure::Fixed("some_code").to_string(), "some_code");
    let failed = Failure::Command("gitleaks-tree").to_string();
    assert_eq!(failed, "scanner_command_failed_gitleaks-tree");
    let missing = Failure::Prerequisite("git").to_string();
    assert_eq!(missing, "missing_security_prerequisite_git");
    assert_eq!(Failure::from(Fault("a_fault")), Failure::Fixed("a_fault"));
    assert_eq!(
        (Snapshot::Source.label(), Snapshot::Final.label()),
        ("source", "final-source")
    );
}

fn git() -> PathBuf {
    executable("git", std::env::var_os("PATH").as_deref()).expect("git on the search path")
}

/// Build a recorded tree below `root`.
fn build(root: &Path, tree: &Value, git: &Path) {
    let run = |directory: &Path, arguments: &[&str]| {
        let mut command = Command::new(git);
        command.args(arguments).current_dir(directory).env_clear();
        command.env("PATH", std::env::var_os("PATH").unwrap_or_default());
        command
            .env("HOME", root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null");
        command.env("GIT_CONFIG_SYSTEM", "/dev/null");
        command.stdout(Stdio::null()).stderr(Stdio::null());
        assert!(
            command.status().expect("git ran").success(),
            "{arguments:?}"
        );
    };
    run(root, &["init", "-q"]);
    for operation in list(tree) {
        let path = root.join(text(&operation[1]));
        let above = path.parent().expect("a parent");
        match text(&operation[0]) {
            "file" => {
                std::fs::create_dir_all(above).expect("directories");
                std::fs::write(&path, text(&operation[2])).expect("a file");
            }
            "fill" => {
                let size = usize::try_from(operation[2].as_u64().expect("a size"));
                std::fs::write(&path, "f".repeat(size.expect("a small size"))).expect("a file");
            }
            "link" => {
                std::fs::create_dir_all(above).expect("directories");
                std::os::unix::fs::symlink(text(&operation[2]), &path).expect("a link");
            }
            "directory" => std::fs::create_dir_all(&path).expect("a directory"),
            "fifo" => {
                let made = Command::new("mkfifo").arg(&path).status();
                assert!(made.expect("mkfifo ran").success());
            }
            "add" => run(root, &["add", "--", text(&operation[1])]),
            "remove" if path.is_dir() && !path.is_symlink() => {
                std::fs::remove_dir_all(&path).expect("a removed directory");
            }
            "remove" => std::fs::remove_file(&path).expect("a removed file"),
            "mode" => {
                let bits = u32::try_from(operation[2].as_u64().expect("a mode"));
                let permissions = std::fs::Permissions::from_mode(bits.expect("a small mode"));
                std::fs::set_permissions(&path, permissions).expect("a mode");
            }
            "repository" => {
                std::fs::create_dir_all(&path).expect("a directory");
                run(&path, &["init", "-q"]);
                std::fs::write(path.join("inner"), "inner\n").expect("a file");
                run(&path, &["add", "--", "inner"]);
                let identity = ["-c", "user.name=n", "-c", "user.email=e@example.invalid"];
                run(
                    &path,
                    &[&identity[..], &["commit", "-q", "-m", "inner"]].concat(),
                );
            }
            other => panic!("an unknown operation {other}"),
        }
    }
}

/// Every entry below a snapshot: its mode and, for a file, its size and the
/// start of its text.
fn copied(directory: &Path, prefix: &str, entries: &mut Map<String, Value>) {
    for entry in std::fs::read_dir(directory).expect("a snapshot directory") {
        let path = entry.expect("an entry").path();
        let name = path.file_name().and_then(OsStr::to_str).expect("a name");
        let name = format!("{prefix}{name}");
        let mut noted = json!({"mode": mode(&path)});
        if path.is_dir() {
            copied(&path, &format!("{name}/"), entries);
        } else {
            let content = std::fs::read(&path).expect("a copy");
            noted["bytes"] = content.len().into();
            let text: String = String::from_utf8_lossy(&content)
                .chars()
                .take(128)
                .collect();
            noted["text"] = text.into();
        }
        entries.insert(name, noted);
    }
}

#[test]
fn every_recorded_snapshot_is_taken_alike() {
    let scratch = Scratch::new("ct");
    let git = git();
    let (mut tally, mut unrepresentable) = (Tally::default(), 0);
    for (index, case) in list(&recorded()["snapshots"]).iter().enumerate() {
        let name = text(&case["name"]);
        let which = match text(&case["label"]) {
            "source" => Snapshot::Source,
            "final-source" => Snapshot::Final,
            _ => {
                // Only the two fixed copies exist here; any other label is no input.
                unrepresentable += 1;
                continue;
            }
        };
        let root = scratch.0.join(format!("{index}r"));
        let output = scratch.0.join(format!("{index}o"));
        std::fs::create_dir(&root).expect("a root");
        std::fs::create_dir(&output).expect("an output directory");
        build(&root, &case["tree"], &git);
        for existing in list(&case["before"]).iter().map(text) {
            if existing.ends_with(".json") {
                std::fs::write(output.join(existing), "").expect("an existing manifest");
            } else {
                std::fs::create_dir(output.join(existing)).expect("an existing directory");
            }
        }
        let mut limits = Limits::REVIEWED;
        if let Some(small) = case["limits"].as_object() {
            limits.source = small["source"].as_u64().expect("a source limit");
            limits.tree = small["tree"].as_u64().expect("a tree limit");
            let files = usize::try_from(small["files"].as_u64().expect("a file limit"));
            limits.files = files.expect("a small file limit");
        }
        let destination = output.join(which.label());
        let manifest = output.join(format!("{}-manifest.json", which.label()));
        let mut context = Context::new(&root, &output, limits, &caller).expect("a context");
        let taken = if let Some(listing) = case["listing"].as_str() {
            let listing = base64(listing);
            mirror(&listing, &root, &destination, &manifest, &limits).map(|()| destination)
        } else {
            let times = if case["twice"] == true { 2 } else { 1 };
            let mut taken = context.snapshot(which, &git);
            for _ in 1..times {
                taken = context.snapshot(which, &git);
            }
            let checks: Vec<Value> = context
                .checks()
                .iter()
                .map(|note| json!({"name": note[0].1, "exitStatus": note[1].1}))
                .collect();
            assert_eq!(json!(checks), case["checks"], "{name}");
            taken
        };
        let reference = &case["result"];
        if let Some(code) = reference.get("code") {
            let refused = taken.err().map(|failure| failure.to_string());
            assert_eq!(
                refused.as_deref(),
                Some(named(text(code), "snapshots")),
                "{name}"
            );
            tally.1 += 1;
            continue;
        }
        let directory = taken.expect(name);
        let mut entries = Map::new();
        copied(&directory, "", &mut entries);
        assert_eq!(Value::from(entries), reference["files"], "{name}");
        let written = std::fs::read_to_string(&manifest).expect("a manifest");
        assert_eq!(
            (written.as_str(), mode(&manifest)),
            (text(&reference["manifest"]), 0o600)
        );
        assert_eq!(
            json!(spelled(&directory)),
            json!(spelled(&output.join(which.label())))
        );
        assert_eq!(mode(&directory), 0o700, "{name}");
        tally.0 += 1;
    }
    assert_eq!((tally, unrepresentable), (Tally(30, 47, 0), 8));
}

#[test]
fn the_inventory_asks_git_for_tracked_and_unignored_files() {
    let scratch = Scratch::new("ci");
    let (root, output) = (scratch.0.join("root"), scratch.0.join("output"));
    std::fs::create_dir(&root).expect("a root");
    std::fs::create_dir(&output).expect("an output directory");
    std::fs::write(root.join("listed"), "content\n").expect("a file");
    let asked = scratch.0.join("asked");
    let lister = scratch.0.join("lister");
    let script = format!(
        "#!/bin/sh\n{{ pwd -P; printf '%s\\n' \"$@\"; }} > '{}'\nprintf 'listed\\0'\n",
        spelled(&asked)
    );
    std::fs::write(&lister, script).expect("a lister");
    std::fs::set_permissions(&lister, std::fs::Permissions::from_mode(0o755)).expect("a mode");
    let mut context = Context::new(&root, &output, Limits::REVIEWED, &caller).expect("a context");
    let taken = context
        .snapshot(Snapshot::Final, &lister)
        .expect("a snapshot");
    assert_eq!(taken, output.join("final-source"));
    let asked = std::fs::read_to_string(asked).expect("what was asked");
    let expected = "ls-files\n-z\n--cached\n--others\n--exclude-standard\n";
    assert_eq!(asked, format!("{}\n{expected}", spelled(&root)));
    assert_eq!(
        std::fs::read(taken.join("listed")).expect("a copy"),
        b"content\n"
    );
    let manifest = std::fs::read_to_string(output.join("final-source-manifest.json"));
    let digest = "434728a410a78f56fc1b5899c3593436e61ab0c731e9072d95e96db290205e53";
    assert_eq!(
        manifest.expect("a manifest"),
        format!("{{\n  \"listed\": \"{digest}\"\n}}\n")
    );
    assert!(output.join("01-final-source-inventory.stdout").is_file());
}
