//! Replays of the recorded scanner-environment builds.
//!
//! `recorded_environment.json` holds what `build/check-security.sh` did to
//! its cache of environments when it was run with an interpreter that
//! scripts only the creation of an environment and the environment's own pip
//! calls: whether it went on to the gate, what it asked of the interpreters,
//! and what the cache held afterwards. The script exits with the status of
//! the step that failed; here that step's check fails by name.

use super::{ASKED, BUILT, INSTALL, INSTALLED, LOCK, OLDEST, interpreter};
use crate::{
    context::{Context, Failure, Limits},
    digest,
    scratch::Scratch,
};
use serde_json::{Map, Value, json};
use std::{
    ffi::OsString,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    time::Duration,
};

const RECORDED: &str = include_str!("recorded_environment.json");

/// The staging directories a case starts with; any other is this build's.
const KEPT: [&str; 3] = ["old-build", "an-other", "van-ished"];

fn text(value: &Value) -> &str {
    value.as_str().expect("a recorded string")
}

fn spelled(path: &Path) -> &str {
    path.to_str().expect("a path that is text")
}

fn mode(path: &Path) -> u32 {
    let metadata = path.symlink_metadata().expect("an entry");
    metadata.permissions().mode() & 0o777
}

fn allow(path: &Path, bits: u32) {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(bits)).expect("a mode");
}

fn caller(name: &str) -> Option<OsString> {
    match name {
        "PATH" => Some("/usr/bin:/bin".into()),
        "HOME" => Some("/h".into()),
        _ => None,
    }
}

/// The interpreter of an environment: it logs what it is asked and answers
/// pip as the case tells it to.
fn environment_python(programs: &Path, log: &Path) -> String {
    let (programs, log) = (spelled(programs), spelled(log));
    [
        "#!/bin/sh".to_owned(),
        concat!(
            "{ printf 'python'; for argument in \"$@\"; ",
            "do printf '\\t%s' \"$argument\"; done; printf '\\n'; } >> '@log@'"
        )
        .replace("@log@", log),
        "if [ \"$1\" = \"-m\" ] && [ \"$2\" = \"pip\" ]; then".to_owned(),
        "  case \" $* \" in".to_owned(),
        format!("    *\" install \"*) exit \"$(cat '{programs}/install.status')\";;"),
        format!("    *\" check \"*) exit \"$(cat '{programs}/check.status')\";;"),
        "  esac".to_owned(),
        "fi".to_owned(),
        "exit 0".to_owned(),
        String::new(),
    ]
    .join("\n")
}

/// A `python3` that says its version and creates an environment as the case
/// tells it to, and does nothing else.
fn python3(programs: &Path, log: &Path) -> String {
    let (programs, log) = (spelled(programs), spelled(log));
    [
        "#!/bin/sh".to_owned(),
        "if [ \"$1\" = \"--version\" ]; then".to_owned(),
        format!("  cat '{programs}/version'; exit \"$(cat '{programs}/version.status')\""),
        "fi".to_owned(),
        "if [ \"$1\" = \"-m\" ] && [ \"$2\" = \"venv\" ]; then".to_owned(),
        format!("  printf 'venv\\t%s\\n' \"$3\" >> '{log}'"),
        format!("  status=$(cat '{programs}/venv.status')"),
        "  [ \"$status\" = 0 ] || exit \"$status\"".to_owned(),
        "  mkdir -p \"$3/bin\"".to_owned(),
        format!("  cp '{programs}/environment-python' \"$3/bin/python\""),
        "  exit 0".to_owned(),
        "fi".to_owned(),
        "exit 99".to_owned(),
        String::new(),
    ]
    .join("\n")
}

/// An environment as an earlier run left it, with a receipt or without.
fn complete(directory: &Path, programs: &Path, receipt: Option<&str>) {
    std::fs::create_dir_all(directory.join("bin")).expect("an environment");
    // The modes the recording's own directories had, whatever this process
    // would give them: the environment, what is in it, and the old build it
    // may lie below. Nothing above those is touched.
    allow(&directory.join("bin"), 0o755);
    allow(directory, 0o755);
    let above = directory.parent().expect("a parent");
    if above
        .file_name()
        .is_some_and(|name| name == ".install.old-build")
    {
        allow(above, 0o755);
    }
    let python = directory.join("bin/python");
    std::fs::copy(programs.join("environment-python"), &python).expect("an interpreter");
    allow(&python, 0o755);
    if let Some(receipt) = receipt {
        std::fs::write(directory.join("ready"), receipt).expect("a receipt");
        allow(&directory.join("ready"), 0o644);
    }
}

/// A project with its lock, scripted interpreters and the state a case
/// starts in. Answers the project, the scripted `python3` and the log.
fn prepared(base: &Path, case: &Value, lock: &str, version: &str) -> [PathBuf; 3] {
    let (project, programs) = (base.join("project"), base.join("programs"));
    std::fs::create_dir_all(project.join("security")).expect("a project");
    std::fs::create_dir(&programs).expect("programs");
    std::fs::write(project.join(LOCK), lock).expect("a lock");
    let log = base.join("log");
    let scripts = [
        ("python3", python3(&programs, &log)),
        ("environment-python", environment_python(&programs, &log)),
    ];
    for (name, script) in scripts {
        std::fs::write(programs.join(name), script).expect("a program");
        allow(&programs.join(name), 0o755);
    }
    let said = [
        ("version", version.to_owned()),
        ("version.status", "0\n".to_owned()),
        ("venv.status", format!("{}\n", case["venv"])),
        ("install.status", format!("{}\n", case["install"])),
        ("check.status", format!("{}\n", case["check"])),
    ];
    for (name, content) in said {
        std::fs::write(programs.join(name), content).expect("what a program says");
    }
    let cache = project.join(".cache/security-environments");
    let wanted = digest(lock.as_bytes());
    let environment = cache.join(&wanted);
    let old = cache.join(".install.old-build");
    let linked = |target: &Path| {
        std::os::unix::fs::symlink(target, &environment).expect("a link");
    };
    let made = |receipt: Option<&str>| {
        complete(&old, &programs, receipt);
        linked(&old);
    };
    std::fs::create_dir_all(&cache).expect("a cache");
    let (held, newline) = (wanted.clone(), format!("{wanted}\n"));
    match text(&case["before"]) {
        "nothing" => std::fs::remove_dir_all(project.join(".cache")).expect("no cache"),
        "complete" => made(Some(&newline)),
        "complete directory" => complete(&environment, &programs, Some(&newline)),
        "link without a receipt" => made(None),
        "link to nothing" => linked(&cache.join(".install.van-ished")),
        "link outside" => {
            complete(&base.join("elsewhere"), &programs, None);
            linked(&base.join("elsewhere"));
        }
        "link below a build" => {
            complete(&old.join("inner"), &programs, None);
            linked(&old.join("inner"));
        }
        "link through a build" => {
            complete(&base.join("elsewhere"), &programs, None);
            std::fs::create_dir(&old).expect("an old build");
            linked(Path::new(&format!(
                "{}/../../../../elsewhere",
                spelled(&old)
            )));
        }
        "link to a staging name outside" => {
            let outside = base.join("elsewhere/.install.old-build");
            complete(&outside, &programs, None);
            linked(&outside);
        }
        "link to another entry" => {
            complete(&cache.join("kept-entry"), &programs, None);
            linked(&cache.join("kept-entry"));
        }
        "relative link" => {
            complete(&old, &programs, None);
            linked(Path::new(".install.old-build"));
        }
        "directory without a receipt" => complete(&environment, &programs, None),
        "file" => std::fs::write(&environment, "not a directory\n").expect("a file"),
        "another receipt" => made(Some(&format!("{}\n", "0".repeat(64)))),
        "receipt without a line end" => made(Some(&held)),
        "receipt with two line ends" => made(Some(&format!("{wanted}\n\n"))),
        "receipt with a space" => made(Some(&format!("{wanted} \n"))),
        "receipt that is a directory" => {
            made(None);
            std::fs::create_dir(old.join("ready")).expect("a directory");
        }
        "other environments" => {
            let other = cache.join(".install.an-other");
            let receipt = format!("{}\n", "1".repeat(64));
            complete(&other, &programs, Some(&receipt));
            let name = cache.join("1".repeat(64));
            std::os::unix::fs::symlink(&other, name).expect("a link");
        }
        other => panic!("an unknown state {other}"),
    }
    [project, programs.join("python3"), log]
}

/// Text with this machine's project and digest, and this build's staging
/// directory, as the recording spells them.
fn recorded_spelling(written: &str, project: &Path, base: &Path, wanted: &str) -> String {
    let written = written.replace(spelled(project), "<project>");
    let written = written
        .replace(spelled(base), "<base>")
        .replace(wanted, "<digest>");
    let mut spelled = String::new();
    let mut rest = written.as_str();
    while let Some(found) = rest.find(".install.") {
        let (before, staged) = rest.split_at(found + ".install.".len());
        let end = staged.find(['/', '\t', '\n']).unwrap_or(staged.len());
        spelled.push_str(before);
        spelled.push_str(if KEPT.contains(&&staged[..end]) {
            &staged[..end]
        } else {
            "<new>"
        });
        rest = &staged[end..];
    }
    spelled + rest
}

/// What the cache holds: each entry's kind with its mode, or where a link
/// leads. What a scripted interpreter made is listed without its mode.
fn left(cache: &Path, respell: &dyn Fn(&str) -> String) -> Value {
    fn walk(directory: &Path, cache: &Path, entries: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(directory).into_iter().flatten() {
            let path = entry.expect("an entry").path();
            entries.push(
                path.strip_prefix(cache)
                    .expect("below the cache")
                    .to_path_buf(),
            );
            if path.is_dir() && !path.is_symlink() {
                walk(&path, cache, entries);
            }
        }
    }
    let mut entries = Vec::new();
    walk(cache, cache, &mut entries);
    let mut listed = Map::new();
    for relative in entries {
        let path = cache.join(&relative);
        let name = respell(spelled(&relative));
        let scripted = name.contains(".install.<new>/bin");
        let bits = if scripted { 0 } else { mode(&path) };
        let noted = if path.is_symlink() {
            let target = path.read_link().expect("a link");
            json!({"link": respell(spelled(&target))})
        } else if path.is_dir() {
            json!({"directory": bits})
        } else {
            let content = std::fs::read_to_string(&path).ok();
            let receipt = content.filter(|_| name.ends_with("ready"));
            json!({"file": bits, "text": receipt})
        };
        listed.insert(name, noted);
    }
    listed.into()
}

#[test]
fn every_recorded_build_of_the_environment_is_made_alike() {
    let recording: Value = serde_json::from_str(RECORDED).expect("the recording is JSON");
    let lock = text(&recording["lock"]);
    let wanted = digest(lock.as_bytes());
    assert_eq!(json!(wanted), recording["digest"]);
    let scratch = Scratch::new("ev");
    let (mut reused, mut built, mut refused) = (0, 0, 0);
    for (index, case) in recording["cases"]
        .as_array()
        .expect("cases")
        .iter()
        .enumerate()
    {
        let name = text(&case["name"]);
        let base = scratch.0.join(index.to_string());
        std::fs::create_dir(&base).expect("a directory");
        let [project, python3, log] = prepared(&base, case, lock, "Python 3.12.0\n");
        let output = base.join("output");
        std::fs::create_dir(&output).expect("an output directory");
        let mut context =
            Context::new(&project, &output, Limits::REVIEWED, &caller).expect("a context");
        let answered = interpreter(&mut context, &python3);
        let respell = |written: &str| recorded_spelling(written, &project, &base, &wanted);
        let mut expected: Vec<&str> = case["log"]
            .as_array()
            .expect("a log")
            .iter()
            .map(text)
            .collect();
        expected.retain(|line| !line.starts_with("python\tbuild/security_check.py"));
        let logged = std::fs::read_to_string(&log).unwrap_or_default();
        let logged = respell(&logged);
        assert_eq!(logged.lines().collect::<Vec<_>>(), expected, "{name}");
        let cache = project.join(".cache/security-environments");
        let mut recorded = case["left"].clone();
        for (name, entry) in recorded.as_object_mut().expect("entries") {
            if name.contains(".install.<new>/bin") {
                let kind = if entry.get("file").is_some() {
                    "file"
                } else {
                    "directory"
                };
                entry[kind] = 0.into();
            }
        }
        assert_eq!(left(&cache, &respell), recorded, "{name}");
        let outside = base.join("elsewhere/.install.old-build").exists();
        assert_eq!(json!(outside), case["outside"], "{name}");
        assert_eq!(
            json!(base.join("elsewhere").exists()),
            case["elsewhere"],
            "{name}"
        );
        let noted = |check: &str| context.checks().iter().any(|note| note[0].1 == check);
        let rebuilding = text(&case["errors"]).starts_with("Rebuilding an incomplete");
        assert_eq!(noted("scanner-environment-rebuilt"), rebuilding, "{name}");
        if case["status"] == 0 {
            let python = cache.join(&wanted).join("bin/python");
            assert_eq!(answered, Ok(python), "{name}");
            if expected.is_empty() {
                reused += 1
            } else {
                built += 1
            }
            continue;
        }
        let step = |kind: &str| case[kind] != 0;
        let code = if text(&case["errors"]).contains("receipt does not match") {
            "scanner_environment_receipt"
        } else if step("venv") {
            "scanner_command_failed_scanner-environment-venv"
        } else if step("install") {
            "scanner_command_failed_scanner-environment-install"
        } else {
            assert!(step("check"), "{name}");
            "scanner_command_failed_scanner-environment-check"
        };
        let failure = answered.err().map(|failure| failure.to_string());
        assert_eq!(failure.as_deref(), Some(code), "{name}");
        refused += 1;
    }
    assert_eq!((reused, built, refused), (5, 14, 8));
}

#[test]
fn only_an_interpreter_of_the_supported_versions_builds_an_environment() {
    let recording: Value = serde_json::from_str(RECORDED).expect("the recording is JSON");
    let lock = text(&recording["lock"]);
    let first = &recording["cases"][0];
    let scratch = Scratch::new("ep");
    let versions = [
        ("Python 3.12.0\n", true),
        ("Python 3.12\n", true),
        ("Python 3.13.0rc1\n", true),
        ("Python 3.14.10", true),
        ("Python 4.0.0\n", true),
        ("Python 3.11.9\n", false),
        ("Python 3.9.6\n", false),
        ("Python 2.7.18\n", false),
        ("Python 3\n", false),
        ("Python three.twelve\n", false),
        ("python 3.12.0\n", false),
        ("3.12.0\n", false),
        ("", false),
    ];
    for (index, (version, supported)) in versions.into_iter().enumerate() {
        let base = scratch.0.join(index.to_string());
        std::fs::create_dir(&base).expect("a directory");
        let [project, python3, _] = prepared(&base, first, lock, version);
        let output = base.join("output");
        std::fs::create_dir(&output).expect("an output directory");
        let mut context =
            Context::new(&project, &output, Limits::REVIEWED, &caller).expect("a context");
        let answered = interpreter(&mut context, &python3).map(|_| ());
        assert_eq!(context.checks()[0][0].1, "scanner-python-version");
        let refusal = Err(Failure::Fixed("scanner_environment_python"));
        assert_eq!(
            answered,
            if supported { Ok(()) } else { refusal },
            "{version:?}"
        );
        assert_eq!(project.join(".cache").exists(), supported, "{version:?}");
    }
    assert_eq!(OLDEST, (3, 12));
}

#[test]
fn the_build_is_bounded_and_installs_by_hash_alone() {
    let minutes = |count: u64| Duration::from_secs(60 * count);
    assert_eq!(
        (ASKED, BUILT, INSTALLED),
        (Duration::from_secs(30), minutes(5), minutes(15))
    );
    assert_eq!(LOCK, "security/requirements.txt");
    let install = "-m pip --isolated install --disable-pip-version-check \
                   --require-hashes --only-binary=:all: --requirement";
    assert_eq!(INSTALL.join(" "), install);
}
