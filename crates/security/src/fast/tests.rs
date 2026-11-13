//! Replays of the recorded fast-tier checks.
//!
//! `recorded_drivers.json` holds what the workflow and secret checks of
//! `build/security_check.py` did with scripted scanners: programs that log
//! their arguments, working directory, environment and input, then say and
//! exit as the case tells them to. The repository of a secret-scan case is
//! built with real git and fixed commit times, so its commits have the
//! recorded names. Where the reference failed with an exception of its
//! interpreter, `named` gives the code here.

use super::{Installed, Programs, Scan, canary, secret_scan, workflows};
use crate::{
    context::{Context, Failure, Limits, Outcome, Snapshot, executable},
    policy::{self, Date, Exception},
    scratch::Scratch,
    tools::Site,
};
use serde_json::{Map, Value, json};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

const RECORDED: &str = include_str!("recorded_drivers.json");

/// What a shell adds to the environment of what it runs.
const SHELL: [&str; 5] = ["_", "PWD", "OLDPWD", "SHLVL", "__CF_USER_TEXT_ENCODING"];

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

fn named(code: &str) -> &str {
    match code {
        "JSONDecodeError" => "invalid_tool_json",
        "KeyError" => "scanner_field_missing",
        "TOMLDecodeError" => "invalid_gitleaks_configuration",
        code => code,
    }
}

fn reviews(recording: &Value, case: &Value) -> Vec<Exception> {
    let today = Date::parse(text(&recording["today"])).expect("the recorded day");
    let parsed = |review| policy::parse(review, today).expect("a recorded review");
    list(&case["reviews"]).iter().map(parsed).collect()
}

/// Scripted scanners by name.
struct Scripted(BTreeMap<&'static str, PathBuf>);

impl Programs for Scripted {
    fn program(&self, name: &'static str) -> Outcome<PathBuf> {
        Ok(self.0[name].clone())
    }
}

/// A program that logs how it was run, then says and exits as its files
/// tell it to. A scanner run in several ways picks its files by its
/// arguments.
fn script(name: &str, directory: &Path, log: &Path) -> String {
    let select = if name == "gitleaks" {
        concat!(
            "case \" $* \" in *\" --exit-code=10 \"*) kind=gitleaks-selftest;; ",
            "*\" --pre-commit \"*) kind=gitleaks-working;; ",
            "*\" --log-opts=\"*) kind=gitleaks-range;; *) kind=gitleaks-tree;; esac"
        )
    } else {
        ":"
    };
    let (directory, log) = (spelled(directory), spelled(log));
    [
        "#!/bin/sh".to_owned(),
        format!("kind={name}"),
        select.to_owned(),
        "canary=$(grep -a -o 'ghp_[A-Za-z0-9]*' \"$2/content000000\" 2>/dev/null | head -1)"
            .to_owned(),
        "index=".to_owned(),
        "case \"$kind\" in gitleaks-working) index=$(git ls-files | tr '\\n' ' ');; esac"
            .to_owned(),
        "{".to_owned(),
        "  printf 'program\\t%s\\n' \"$kind\"".to_owned(),
        concat!(
            "  printf 'argv'; for argument in \"$@\"; ",
            "do printf '\\t%s' \"$argument\"; done; printf '\\n'"
        )
        .to_owned(),
        "  printf 'cwd\\t%s\\n' \"$(pwd -P)\"".to_owned(),
        "  printf 'env\\t%s\\n' \"$(env | LC_ALL=C sort | tr '\\n' '\\t')\"".to_owned(),
        "  printf 'stdin\\t%s\\n' \"$(od -An -tx1 | tr -d ' \\n')\"".to_owned(),
        "  printf 'index\\t%s\\n' \"$index\"".to_owned(),
        format!("}} >> '{log}'"),
        format!("sed \"s|@2@|$2|g; s|@canary@|$canary|g\" '{directory}'/\"$kind\".out"),
        format!("exit \"$(cat '{directory}'/\"$kind\".status)\""),
        String::new(),
    ]
    .join("\n")
}

/// Write the scripted programs and what each kind of run says.
fn programs(directory: &Path, log: &Path, behaviours: &[(&'static str, &Value)]) -> Scripted {
    let mut paths = BTreeMap::new();
    for (kind, behaviour) in behaviours {
        let status = behaviour[0].as_i64().expect("a status");
        std::fs::write(directory.join(format!("{kind}.out")), text(&behaviour[1])).expect("output");
        std::fs::write(
            directory.join(format!("{kind}.status")),
            format!("{status}\n"),
        )
        .expect("a status file");
        let name = if kind.starts_with("gitleaks") {
            "gitleaks"
        } else {
            kind
        };
        if !paths.contains_key(name) {
            let path = directory.join(name);
            std::fs::write(&path, script(name, directory, log)).expect("a program");
            let executable = std::fs::Permissions::from_mode(0o755);
            std::fs::set_permissions(&path, executable).expect("a mode");
            paths.insert(name, path);
        }
    }
    Scripted(paths)
}

/// The name of a `NAME=value` pair.
fn variable(pair: &str) -> &str {
    pair.split_once('=').map_or(pair, |(name, _)| name)
}

/// The log as the recording holds it: this machine's paths and the shell's
/// own variables taken out, and each run's environment as what it adds to
/// the group's fixed one or loses of it.
fn logged(log: &Path, replacements: &[(String, String)], fixed: &Value) -> Value {
    let Ok(mut written) = std::fs::read_to_string(log) else {
        return json!([]);
    };
    for (machine, token) in replacements {
        written = written.replace(machine, token);
    }
    let locale = if cfg!(target_os = "linux") {
        "C.UTF-8"
    } else {
        "en_US.UTF-8"
    };
    let fixed: Vec<&str> = list(fixed).iter().map(text).collect();
    let (mut records, mut current) = (Vec::new(), Map::new());
    for line in written.lines() {
        let (key, value) = line.split_once('\t').unwrap_or((line, ""));
        if key == "program" && !current.is_empty() {
            records.push(Value::Object(std::mem::take(&mut current)));
        }
        match key {
            "argv" if value.is_empty() => drop(current.insert(key.to_owned(), json!([]))),
            "argv" => {
                let arguments: Vec<&str> = value.split('\t').collect();
                current.insert(key.to_owned(), json!(arguments));
            }
            "env" => {
                let mut pairs: Vec<String> = value
                    .split('\t')
                    .filter(|pair| !pair.is_empty() && !SHELL.contains(&variable(pair)))
                    .map(|pair| pair.replace(locale, "en_US.UTF-8"))
                    .collect();
                pairs.sort_unstable();
                let held = |pair: &&str| pairs.iter().any(|own| variable(own) == variable(pair));
                let lost: Vec<&str> = fixed.iter().filter(|pair| !held(pair)).copied().collect();
                pairs.retain(|pair| !fixed.contains(&pair.as_str()));
                current.insert("added".to_owned(), json!(pairs));
                current.insert("lost".to_owned(), json!(lost));
            }
            _ => drop(current.insert(key.to_owned(), value.into())),
        }
    }
    if !current.is_empty() {
        records.push(Value::Object(current));
    }
    records.into()
}

/// What the invocation recorded, without the times taken.
fn noted(context: &Context, replacements: &[(String, String)]) -> Value {
    let mut checks = Vec::new();
    for note in context.checks() {
        let mut check = Map::new();
        for (name, value) in note {
            if *name != "elapsedSeconds" {
                check.insert((*name).to_owned(), value.clone());
            }
        }
        checks.push(Value::Object(check));
    }
    let mut written = serde_json::to_string(&checks).expect("checks");
    for (machine, token) in replacements {
        written = written.replace(machine, token);
    }
    serde_json::from_str(&written).expect("checks again")
}

fn settled(case: &Value, judged: Outcome<()>) -> bool {
    let name = text(&case["name"]);
    let refused = judged.err().map(|failure| failure.to_string());
    let expected = case["result"].get("code").map(|code| named(text(code)));
    assert_eq!(refused.as_deref(), expected, "{name}");
    expected.is_none()
}

fn caller(search: Option<OsString>) -> impl Fn(&str) -> Option<OsString> {
    move |name| match name {
        "PATH" => search.clone(),
        "HOME" => Some("/h".into()),
        _ => None,
    }
}

#[test]
fn every_recorded_workflow_check_is_made_alike() {
    let recording = recorded();
    let scratch = Scratch::new("fw");
    let (mut held, mut refused) = (0, 0);
    for (index, case) in list(&recording["workflows"]).iter().enumerate() {
        let name = text(&case["name"]);
        let base = scratch.0.join(index.to_string());
        let [root, output, tools] = ["root", "output", "tools"].map(|part| base.join(part));
        for directory in [&root, &output, &tools] {
            std::fs::create_dir_all(directory).expect("a directory");
        }
        let log = base.join("log");
        let behaviours = [
            ("actionlint", &case["actionlint"]),
            ("zizmor", &case["zizmor"]),
        ];
        let scripted = programs(&tools, &log, &behaviours);
        let ambient = caller(Some("/usr/bin:/bin".into()));
        let mut context =
            Context::new(&root, &output, Limits::REVIEWED, &ambient).expect("a context");
        let judged = workflows(&mut context, &scripted, &reviews(&recording, case));
        if settled(case, judged) {
            held += 1
        } else {
            refused += 1
        }
        let replacements = [
            (&root, "<root>"),
            (&output, "<output>"),
            (&tools, "<tools>"),
        ]
        .map(|(path, token)| (spelled(path).to_owned(), token.to_owned()));
        assert_eq!(noted(&context, &replacements), case["checks"], "{name}");
        let fixed = &recording["environments"]["workflows"];
        assert_eq!(logged(&log, &replacements, fixed), case["log"], "{name}");
    }
    assert_eq!((held, refused), (6, 14));
}

/// The fixed names and times of the recorded commits.
const STAMP: [(&str, &str); 6] = [
    ("GIT_AUTHOR_NAME", "a"),
    ("GIT_AUTHOR_EMAIL", "a@example.invalid"),
    ("GIT_COMMITTER_NAME", "a"),
    ("GIT_COMMITTER_EMAIL", "a@example.invalid"),
    ("GIT_AUTHOR_DATE", "2026-09-30T12:00:00+00:00"),
    ("GIT_COMMITTER_DATE", "2026-09-30T12:00:00+00:00"),
];

/// Build the recorded repository below `root` and answer its four commits:
/// the first, one without history, the head, and one beside the head.
fn repository(root: &Path, git: &Path, configuration: &str, vendored: bool) -> [String; 4] {
    let run = |arguments: &[&str]| {
        let mut command = Command::new(git);
        command.args(arguments).current_dir(root).env_clear();
        command.env("PATH", std::env::var_os("PATH").unwrap_or_default());
        command
            .env("HOME", root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null");
        command.env("GIT_CONFIG_SYSTEM", "/dev/null").envs(STAMP);
        command.stderr(Stdio::null());
        let done = command.output().expect("git ran");
        assert!(done.status.success(), "{arguments:?}");
        String::from_utf8(done.stdout)
            .expect("text")
            .trim()
            .to_owned()
    };
    let write = |name: &str, content: &str| {
        let path = root.join(name);
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("directories");
        std::fs::write(path, content).expect("a file");
    };
    run(&["init", "-q", "-b", "main"]);
    write("src/a.rs", "fn a() {}\n");
    write("tests/fixture.pem", "-----BEGIN PUBLIC KEY-----\n");
    write("security/gitleaks.toml", configuration);
    if vendored {
        write("vendor/lib/x.c", "int x;\n");
        write("vendor/README.md", "vendored\n");
    }
    run(&["add", "."]);
    run(&["commit", "-q", "-m", "first"]);
    let first = run(&["rev-parse", "HEAD"]);
    run(&["checkout", "-q", "--orphan", "other"]);
    run(&["commit", "-q", "-m", "other"]);
    let other = run(&["rev-parse", "HEAD"]);
    run(&["checkout", "-q", "main"]);
    run(&["checkout", "-q", "-b", "side"]);
    write("src/side.rs", "fn side() {}\n");
    run(&["add", "."]);
    run(&["commit", "-q", "-m", "side"]);
    let side = run(&["rev-parse", "HEAD"]);
    run(&["checkout", "-q", "main"]);
    write("src/b.rs", "fn b() {}\n");
    run(&["add", "."]);
    run(&["commit", "-q", "-m", "second"]);
    let second = run(&["rev-parse", "HEAD"]);
    write("src/c.rs", "fn c() {}\n");
    [first, other, second, side]
}

#[test]
fn every_recorded_secret_scan_is_made_alike() {
    let recording = recorded();
    let scratch = Scratch::new("fs");
    let search = std::env::var_os("PATH");
    let git = executable("git", search.as_deref()).expect("git on the search path");
    let good = [
        json!([
            10,
            r#"[{"RuleID":"github-pat","Secret":"REDACTED","File":"@2@/content000000"}]"#
        ]),
        json!([0, "[]"]),
    ];
    let kinds = [
        "gitleaks-selftest",
        "gitleaks-tree",
        "gitleaks-range",
        "gitleaks-working",
    ];
    let (mut held, mut refused) = (0, 0);
    for (index, case) in list(&recording["secrets"]).iter().enumerate() {
        let name = text(&case["name"]);
        let base = scratch.0.join(index.to_string());
        let [root, output, tools] = ["root", "output", "tools"].map(|part| base.join(part));
        for directory in [&root, &output, &tools] {
            std::fs::create_dir_all(directory).expect("a directory");
        }
        let vendored = case["vendored"] == true;
        let commits = repository(&root, &git, text(&case["configuration"]), vendored);
        let labels = ["first", "other", "second", "side"];
        let recorded = labels.map(|label| text(&case["commits"][label]));
        assert_eq!(commits.each_ref().map(String::as_str), recorded, "{name}");
        let log = base.join("log");
        let said = |kind: &'static str| {
            let usual = if kind == "gitleaks-selftest" {
                &good[0]
            } else {
                &good[1]
            };
            (kind, case["behaviours"].get(kind).unwrap_or(usual))
        };
        let scripted = programs(&tools, &log, &kinds.map(said));
        let ambient = caller(search.clone());
        let mut context =
            Context::new(&root, &output, Limits::REVIEWED, &ambient).expect("a context");
        let snapshot = context
            .snapshot(Snapshot::Source, &git)
            .expect("a snapshot");
        let given = match text(&case["base"]) {
            "none" => None,
            "unknown" => Some("0".repeat(40)),
            "first" => Some(commits[0].clone()),
            "other" => Some(commits[1].clone()),
            "second" => Some(commits[2].clone()),
            "side" => Some(commits[3].clone()),
            literal => Some(literal.to_owned()),
        };
        let reviews = reviews(&recording, case);
        let scan = Scan {
            reviews: &reviews,
            base: given.as_deref(),
            snapshot: &snapshot,
            include_vendor: case["includeVendor"] == true,
            git: &git,
        };
        let judged = secret_scan(&mut context, &scripted, &scan);
        if settled(case, judged) {
            held += 1
        } else {
            refused += 1
        }
        let search = search.clone().and_then(|search| search.into_string().ok());
        let mut replacements: Vec<(String, String)> = [
            (&root, "<root>"),
            (&output, "<output>"),
            (&tools, "<tools>"),
        ]
        .map(|(path, token)| (spelled(path).to_owned(), token.to_owned()))
        .into();
        replacements.push((
            search.expect("a search path that is text"),
            "<path>".to_owned(),
        ));
        let tokens = ["<first>", "<other>", "<second>", "<side>"];
        for (commit, label) in commits.iter().zip(tokens) {
            replacements.push((commit.clone(), label.to_owned()));
        }
        assert_eq!(noted(&context, &replacements), case["checks"], "{name}");
        let fixed = &recording["environments"]["secrets"];
        assert_eq!(logged(&log, &replacements, fixed), case["log"], "{name}");
        let mut left: Vec<String> = std::fs::read_dir(&output)
            .expect("the output")
            .map(|entry| {
                entry
                    .expect("an entry")
                    .file_name()
                    .into_string()
                    .expect("text")
            })
            .filter(|name| {
                name != "home" && !name.starts_with(|first: char| first.is_ascii_digit())
            })
            .collect();
        left.sort_unstable();
        assert_eq!(json!(left), case["left"], "{name}");
    }
    assert_eq!((held, refused), (14, 36));
}

#[test]
fn the_canary_is_shaped_like_a_credential() {
    let canary = canary();
    assert_eq!(canary.len(), 40);
    assert!(canary.starts_with("ghp_"));
    assert!(canary[4..].bytes().all(|byte| byte.is_ascii_alphanumeric()));
}

fn mode(path: &Path) -> u32 {
    let metadata = path.symlink_metadata().expect("an entry");
    metadata.permissions().mode() & 0o777
}

/// A repository, a snapshot of it and scripted scanners that find nothing but
/// the canary, with `git` as the git the scan asks about the repository.
fn scanned(scratch: &Scratch, git: Option<&str>) -> (Outcome<()>, PathBuf) {
    let recording = recorded();
    let search = std::env::var_os("PATH");
    let real = executable("git", search.as_deref()).expect("git on the search path");
    let [root, output, tools] = ["root", "output", "tools"].map(|part| scratch.0.join(part));
    for directory in [&root, &output, &tools] {
        std::fs::create_dir_all(directory).expect("a directory");
    }
    let plain = text(&recording["secrets"][0]["configuration"]);
    repository(&root, &real, plain, true);
    let found = r#"[{"RuleID":"github-pat","Secret":"REDACTED","File":"@2@/content000000"}]"#;
    let (finds, clean) = (json!([10, found]), json!([0, "[]"]));
    let behaviours = [
        ("gitleaks-selftest", &finds),
        ("gitleaks-tree", &clean),
        ("gitleaks-range", &clean),
        ("gitleaks-working", &clean),
    ];
    let scripted = programs(&tools, &scratch.0.join("log"), &behaviours);
    let asked = scratch.0.join("asked-git");
    if let Some(body) = git {
        std::fs::write(&asked, format!("#!/bin/sh\n{body}\n")).expect("a scripted git");
        let executable = std::fs::Permissions::from_mode(0o755);
        std::fs::set_permissions(&asked, executable).expect("a mode");
    }
    let ambient = caller(search);
    let mut context = Context::new(&root, &output, Limits::REVIEWED, &ambient).expect("a context");
    let snapshot = context
        .snapshot(Snapshot::Source, &real)
        .expect("a snapshot");
    let scan = Scan {
        reviews: &[],
        base: None,
        snapshot: &snapshot,
        include_vendor: false,
        git: if git.is_some() { &asked } else { &real },
    };
    (secret_scan(&mut context, &scripted, &scan), output)
}

#[test]
fn the_evidence_of_the_self_test_is_private() {
    let scratch = Scratch::new("fp");
    let (judged, output) = scanned(&scratch, None);
    assert_eq!(judged, Ok(()));
    assert_eq!(mode(&output.join("secret-canary.original")), 0o600);
    assert_eq!(mode(&output.join("secret-canary.toml")), 0o600);
    assert_eq!(mode(&output.join("secret-canary-inputs")), 0o700);
    let configuration = std::fs::read_to_string(output.join("secret-canary.toml"));
    assert_eq!(
        configuration.expect("a configuration"),
        "[extend]\nuseDefault = true\n"
    );
    let original = std::fs::read(output.join("secret-canary.original")).expect("the canary");
    let expected = format!(
        "\x7fELF\x02\x01\x01\x00TOKEN={} # gitleaks:allow\n",
        canary()
    );
    assert_eq!(original, expected.as_bytes());
}

#[test]
fn the_git_directory_must_be_a_directory_that_is_named_in_text() {
    let scratch = Scratch::new("fg");
    let file = scratch.0.join("a-file");
    std::fs::write(&file, "no directory").expect("a file");
    let (judged, _) = scanned(
        &scratch,
        Some(&format!("printf '%s\\n' '{}'", spelled(&file))),
    );
    assert_eq!(judged, Err(Failure::Fixed("secret_diff_git_directory")));
    let scratch = Scratch::new("fh");
    let (judged, _) = scanned(&scratch, Some("printf '\\377\\376\\n'"));
    assert_eq!(judged, Err(Failure::Fixed("secret_diff_git_directory")));
    let scratch = Scratch::new("fi");
    let (judged, _) = scanned(&scratch, Some("printf '/nowhere/at/all\\n'"));
    assert_eq!(judged, Err(Failure::Fixed("tool_entry_missing")));
}

#[test]
fn an_installation_answers_from_its_own_directory() {
    let scratch = Scratch::new("ft");
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let lock = manifest.join("../../build/security-tools.lock.json");
    let site = Site {
        lock: &lock,
        cache: &scratch.0.join("cache"),
        platform: "linux-x86_64",
        limits: crate::tools::Limits::REVIEWED,
    };
    let directory = scratch.0.join("installed");
    let private = std::fs::Permissions::from_mode(0o700);
    std::fs::create_dir_all(directory.join("bin")).expect("an installation");
    std::fs::set_permissions(&directory, private.clone()).expect("a mode");
    std::fs::set_permissions(directory.join("bin"), private).expect("a mode");
    std::fs::write(directory.join("receipt.json"), "no receipt").expect("a receipt");
    let owned = std::fs::Permissions::from_mode(0o600);
    std::fs::set_permissions(directory.join("receipt.json"), owned).expect("a mode");
    let installed = Installed {
        site,
        directory: &directory,
    };
    let refused = installed.program("gitleaks");
    assert_eq!(refused, Err(Failure::Fixed("invalid_tool_json")));
}
