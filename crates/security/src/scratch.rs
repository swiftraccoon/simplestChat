//! What the replay tests share: a directory of one test's own, removed with
//! it, the bytes of a recorded file, the repositories the recorders built,
//! programs that answer from files, and what a check left as evidence.

use serde_json::{Map, Value, json};
use std::{
    collections::BTreeMap,
    fs::Permissions,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
};

pub(crate) struct Scratch(pub(crate) PathBuf);

impl Scratch {
    pub(crate) fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "simplestchat-security-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("scratch directory");
        Self(path.canonicalize().expect("scratch path"))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Standard base64, as a recording holds bytes that are no text.
pub(crate) fn base64(text: &str) -> Vec<u8> {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut bytes = Vec::new();
    let (mut bits, mut held) = (0_u32, 0);
    for symbol in text.bytes().filter(|symbol| *symbol != b'=') {
        let value = ALPHABET.iter().position(|letter| *letter == symbol);
        bits = bits << 6 | u32::try_from(value.expect("a base64 symbol")).expect("six bits");
        held += 6;
        if held >= 8 {
            held -= 8;
            bytes.push(u8::try_from(bits >> held & 0xff).expect("a byte"));
        }
    }
    bytes
}

fn text(value: &Value) -> &str {
    value.as_str().expect("a recorded string")
}

/// The bytes of a recorded file: text, text beside a mode, base64, a
/// record of so many numbered source files, or one text repeated between a
/// beginning and an end.
pub(crate) fn content(spec: &Value) -> Vec<u8> {
    if let Some(text) = spec.as_str().or_else(|| spec["text"].as_str()) {
        return text.as_bytes().to_vec();
    }
    if let Some(count) = spec["numbered"].as_u64() {
        let field = |index: u64| format!("\"src/f{index}.py\": 1");
        let fields: Vec<String> = (0..count).map(field).collect();
        return format!("{{{}}}", fields.join(", ")).into_bytes();
    }
    if let Some(encoded) = spec.get("base64") {
        return base64(text(encoded));
    }
    let times = usize::try_from(spec["times"].as_u64().expect("a count"));
    let mut written = spec["first"].as_str().unwrap_or_default().to_owned();
    written.push_str(&text(&spec["repeat"]).repeat(times.expect("a small count")));
    written.push_str(spec["then"].as_str().unwrap_or_default());
    written.into_bytes()
}

/// Where the host keeps git.
pub(crate) fn host_git() -> PathBuf {
    let search = std::env::var_os("PATH").expect("a search path");
    let found = std::env::split_paths(&search)
        .map(|directory| directory.join("git"))
        .find(|candidate| candidate.is_file());
    found.expect("git on the search path")
}

/// Write `files` below `root`: a file that is null is removed, one that is
/// a record with a link is a link to what the record names, and one with a
/// mode is given it.
pub(crate) fn written(root: &Path, files: &Map<String, Value>) {
    for (path, spec) in files {
        let target = root.join(path);
        if spec.is_null() {
            std::fs::remove_file(&target).expect("a file to remove");
            continue;
        }
        std::fs::create_dir_all(target.parent().expect("a parent")).expect("directories");
        if let Some(link) = spec.get("link") {
            std::os::unix::fs::symlink(text(link), &target).expect("a link");
            continue;
        }
        std::fs::write(&target, content(spec)).expect("a file");
        if let Some(mode) = spec["mode"].as_u64() {
            let mode = Permissions::from_mode(u32::try_from(mode).expect("a mode"));
            std::fs::set_permissions(&target, mode).expect("a mode that is set");
        }
    }
}

/// Write the program `name` into `tools` with the recorded `answers` to its
/// calls, each a status and what is printed, and answer where it is. The
/// program logs its arguments in hexadecimal, each ended by a zero byte,
/// and where it runs, to `log`, and answers its calls in order from files
/// named after `stem`.
pub(crate) fn scripted(
    tools: &Path,
    log: &Path,
    name: &str,
    stem: &str,
    answers: &Value,
) -> PathBuf {
    for (number, answer) in answers.as_array().expect("answers").iter().enumerate() {
        let said = tools.join(format!("{stem}.{number}.out"));
        std::fs::write(said, content(&answer[1])).expect("an answer");
        let status = tools.join(format!("{stem}.{number}.status"));
        std::fs::write(status, format!("{}\n", answer[0])).expect("a status");
    }
    std::fs::write(tools.join(format!("{stem}.count")), "0\n").expect("a count");
    let (directory, logged) = (tools.display(), log.display());
    let lines = [
        "#!/bin/sh".to_owned(),
        format!("count=$(cat '{directory}/{stem}.count')"),
        format!("echo $((count + 1)) > '{directory}/{stem}.count'"),
        "printf '%s\\t%s\\n' \\".to_owned(),
        "  \"$(printf '%s\\0' \"$@\" | od -An -v -tx1 | tr -d ' \\n')\" \"$(pwd -P)\" \\"
            .to_owned(),
        format!("  >> '{logged}'"),
        format!("cat '{directory}/{stem}'.$count.out"),
        format!("exit \"$(cat '{directory}/{stem}'.$count.status)\""),
        String::new(),
    ];
    let program = tools.join(name);
    std::fs::write(&program, lines.join("\n")).expect("a program");
    std::fs::set_permissions(&program, Permissions::from_mode(0o755)).expect("a mode");
    program
}

/// The calls a scripted program logged, with the place of the case written
/// as the recording writes it.
pub(crate) fn asked(log: &Path, place: &str) -> Value {
    let logged = std::fs::read_to_string(log).unwrap_or_default();
    let call = |line: &str| {
        let (arguments, directory) = line.split_once('\t').expect("a logged call");
        let bytes: Vec<u8> = (0..arguments.len() / 2)
            .map(|at| u8::from_str_radix(&arguments[2 * at..2 * at + 2], 16).expect("hex"))
            .collect();
        let arguments = bytes.strip_suffix(&[0]).expect("an end");
        let argv: Vec<String> = arguments
            .split(|byte| *byte == 0)
            .map(|argument| String::from_utf8_lossy(argument).replace(place, "<place>"))
            .collect();
        json!({"argv": argv, "directory": directory.replace(place, "<place>")})
    };
    json!(logged.lines().map(call).collect::<Vec<_>>())
}

/// The entries below `output`, each from it. The directories `closed` are
/// named and not entered.
fn entries(output: &Path, closed: &[&str]) -> Vec<(String, PathBuf)> {
    fn walk(root: &Path, directory: &Path, closed: &[&str], found: &mut Vec<(String, PathBuf)>) {
        for entry in std::fs::read_dir(directory).expect("a directory") {
            let path = entry.expect("an entry").path();
            let below = path.strip_prefix(root).expect("below");
            if path.is_dir() && !closed.iter().any(|closed| below == Path::new(closed)) {
                walk(root, &path, closed, found);
            }
            found.push((below.to_string_lossy().into_owned(), path));
        }
    }
    let mut found = Vec::new();
    if output.is_dir() {
        walk(output, output, closed, &mut found);
    }
    found
}

/// What a check left in its evidence directory: every entry with its mode.
pub(crate) fn kept(output: &Path, closed: &[&str]) -> Value {
    let mut listed = json!({});
    for (below, path) in entries(output, closed) {
        let mode = path.symlink_metadata().expect("an entry").permissions();
        listed[&below] = json!({"mode": mode.mode() & 0o777, "directory": path.is_dir()});
    }
    listed
}

/// The text of the evidence files that are `wanted`, with the place of the
/// case written as the recording writes it.
pub(crate) fn said(
    output: &Path,
    place: &str,
    closed: &[&str],
    wanted: impl Fn(&str) -> bool,
) -> Value {
    let mut texts = json!({});
    for (below, path) in entries(output, closed) {
        if path.is_file() && wanted(&below) {
            let text = std::fs::read_to_string(&path).expect("text");
            texts[&below] = text.replace(place, "<place>").into();
        }
    }
    texts
}

/// One recorded repository: its commits by name and the files it ends with.
pub(crate) struct Repository {
    pub(crate) commits: BTreeMap<&'static str, String>,
    pub(crate) files: BTreeMap<String, Value>,
}

/// Build the repository of `case` in `root` as the recorder built it: a
/// first commit, an unrelated one, one beside the main line, a second on the
/// main line and uncommitted work, every commit with the recording's fixed
/// stamp.
pub(crate) fn repository(root: &Path, git: &Path, recording: &Value, case: &Value) -> Repository {
    let run = |arguments: &[&str]| {
        let stamp = recording["stamp"].as_object().expect("a stamp");
        let done = Command::new(git)
            .args(arguments)
            .current_dir(root)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .envs(stamp.iter().map(|(name, value)| (name, text(value))))
            .output()
            .expect("git runs");
        assert!(done.status.success(), "git {arguments:?}");
        String::from_utf8(done.stdout)
            .expect("text")
            .trim()
            .to_owned()
    };
    let empty = Map::new();
    let tree = |value: &Value| value.as_object().cloned();
    let first = tree(&case["first"]).unwrap_or_else(|| tree(&recording["tree"]).expect("a tree"));
    let mut files: BTreeMap<String, Value> = first.clone().into_iter().collect();
    let mut commits = BTreeMap::new();
    run(&["init", "-q", "-b", "main"]);
    written(root, &first);
    run(&["add", "-A"]);
    run(&["commit", "-q", "--allow-empty", "-m", "first"]);
    commits.insert("first", run(&["rev-parse", "HEAD"]));
    commits.insert("tree", run(&["rev-parse", "HEAD^{tree}"]));
    run(&["checkout", "-q", "--orphan", "other"]);
    run(&["commit", "-q", "--allow-empty", "-m", "other"]);
    commits.insert("other", run(&["rev-parse", "HEAD"]));
    run(&["checkout", "-q", "main"]);
    run(&["checkout", "-q", "-b", "side"]);
    run(&["commit", "-q", "--allow-empty", "-m", "side"]);
    commits.insert("side", run(&["rev-parse", "HEAD"]));
    run(&["checkout", "-q", "main"]);
    if let Some(second) = case["second"].as_object() {
        written(root, second);
        files.extend(second.clone());
        run(&["add", "-A"]);
        run(&["commit", "-q", "--allow-empty", "-m", "second"]);
    }
    commits.insert("second", run(&["rev-parse", "HEAD"]));
    let ignored = case["ignored"].as_array().map_or(&[][..], Vec::as_slice);
    let ignored: Vec<&str> = ignored.iter().map(text).collect();
    if !ignored.is_empty() {
        let rules: String = ignored.iter().map(|path| format!("{path}\n")).collect();
        std::fs::write(root.join(".gitignore"), &rules).expect("rules");
        files.insert(".gitignore".to_owned(), rules.into());
    }
    let working = case["working"].as_object().unwrap_or(&empty);
    written(root, working);
    files.extend(working.clone());
    files.retain(|path, spec| !spec.is_null() && !ignored.contains(&path.as_str()));
    Repository { commits, files }
}
