//! Replays of `recorded_sandbox.json`: what the reference in Python made of
//! whole scanner runs in a sandbox whose engine answers as its tests' does,
//! of the bundle of the scanners, of the preparation of a sandbox and of
//! its branches; and what the slots and the owners of scanners hold to.

use super::{
    Cache, Engine, LABEL, Limits, Platform, SLOTS, Sandbox, Scan, Setup, Slots, TOOLS, bundle,
    owner, prepare,
};
use crate::{
    Fault, Result,
    document::Document,
    image::{
        cache, diagnostic,
        policy::tests::{There, tallied},
        secrets::tests::{here, text, there},
    },
    scratch::{Scratch, base64},
};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    io::Read,
    os::unix::fs::{FileTypeExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

const RECORDING: &[u8] = include_bytes!("recorded_sandbox.json");

pub(crate) fn recording() -> Document {
    Document::read(RECORDING).expect("a recording")
}

pub(crate) fn cases(name: &str) -> Vec<Document> {
    let held = recording()
        .get(name)
        .and_then(Document::items)
        .map(<[Document]>::to_vec);
    held.unwrap_or_else(|| panic!("no cases {name}"))
}

/// The whole number `name` of `case`.
pub(crate) fn whole(case: &Document, name: &str) -> u64 {
    let held = case.get(name).and_then(Document::whole);
    u64::try_from(held.unwrap_or_else(|| panic!("no number {name}"))).expect("a count")
}

/// The texts of the list `name` of `case`.
pub(crate) fn texts(case: &Document, name: &str) -> Vec<String> {
    let held = case
        .get(name)
        .and_then(Document::items)
        .unwrap_or_else(|| panic!("no list {name}"));
    held.iter()
        .map(|item| item.text().expect("a text").to_owned())
        .collect()
}

/// The fields of the record `name` of `case`, each a text.
pub(crate) fn pairs(case: &Document, name: &str) -> Vec<(String, String)> {
    let held = case
        .get(name)
        .and_then(Document::fields)
        .unwrap_or_else(|| panic!("no {name}"));
    let pair =
        |(key, value): &(String, Document)| (key.clone(), value.text().expect("a text").to_owned());
    held.iter().map(pair).collect()
}

/// What a tree holds, as the recorder shows one: each entry's kind, the
/// digest of its bytes or its target, and its mode.
pub(crate) fn walked(root: &Path) -> Document {
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let Ok(listing) = fs::read_dir(&directory) else {
            continue;
        };
        for entry in listing {
            let path = entry.expect("an entry").path();
            let relative = path.strip_prefix(root).expect("below the root");
            let name = relative.to_str().expect("text").to_owned();
            let kind = path.symlink_metadata().expect("an entry");
            let mode = Document::from(u64::from(kind.permissions().mode() & 0o7777));
            let shown = if kind.file_type().is_symlink() {
                let target = fs::read_link(&path).expect("a link");
                Document::record([("link", target.to_str().expect("text").into())])
            } else if kind.file_type().is_fifo() {
                Document::record([("pipe", true.into()), ("mode", mode)])
            } else if kind.is_dir() {
                pending.push(path.clone());
                Document::record([("directory", true.into()), ("mode", mode)])
            } else {
                let bytes = fs::read(&path).expect("bytes");
                Document::record([("bytes", crate::digest(&bytes).into()), ("mode", mode)])
            };
            found.push((name, shown));
        }
    }
    found.sort_by(|left, right| left.0.cmp(&right.0));
    Document::Record(found)
}

/// The bytes of a recorded file.
pub(crate) fn unpacked(entry: &Document) -> Vec<u8> {
    let packed = base64(text(entry, "file"));
    let mut bytes = Vec::new();
    let read = flate2::read::ZlibDecoder::new(&packed[..]).read_to_end(&mut bytes);
    read.expect("deflated bytes");
    bytes
}

/// A recorded tree as [`walked`] shows one.
pub(crate) fn expected(tree: &Document) -> Document {
    let mut shown = Vec::new();
    for (name, entry) in tree.fields().expect("a tree") {
        let mode = || entry.get("mode").cloned().expect("a mode");
        let entry = if let Some(target) = entry.get("link") {
            Document::record([("link", target.clone())])
        } else if entry.get("pipe").is_some() {
            Document::record([("pipe", true.into()), ("mode", mode())])
        } else if entry.get("directory").is_some() {
            Document::record([("directory", true.into()), ("mode", mode())])
        } else {
            let bytes = unpacked(entry);
            Document::record([("bytes", crate::digest(&bytes).into()), ("mode", mode())])
        };
        shown.push((name.clone(), entry));
    }
    shown.sort_by(|left, right| left.0.cmp(&right.0));
    Document::Record(shown)
}

/// The names of what a tree holds.
fn names(root: &Path) -> Vec<String> {
    let walked = walked(root);
    walked
        .fields()
        .expect("a tree")
        .iter()
        .map(|(name, _)| name.clone())
        .collect()
}

/// Give every directory below `root` back to its owner, so that it can be
/// removed.
pub(crate) fn reopened(root: &Path) {
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let _ = fs::set_permissions(&directory, fs::Permissions::from_mode(0o700));
        let Ok(listing) = fs::read_dir(&directory) else {
            continue;
        };
        for entry in listing.flatten() {
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                pending.push(entry.path());
            }
        }
    }
}

/// `value` with every time of evidence replaced as the recorder replaced
/// it, since times differ between runs.
pub(crate) fn timeless(value: Document) -> Document {
    match value {
        Document::Record(fields) => Document::Record(
            fields
                .into_iter()
                .map(|(name, value)| match value {
                    Document::Number(number) if name == "elapsedSeconds" && number.is_f64() => {
                        (name, "<elapsed>".into())
                    }
                    other => (name, timeless(other)),
                })
                .collect(),
        ),
        Document::List(items) => Document::List(items.into_iter().map(timeless).collect()),
        other => other,
    }
}

/// `value` with the paths below `base`, the owner `owner` and the account
/// `user` named as the recorder named them.
fn masked(value: &Document, base: &Path, owner: &str, user: &str) -> Document {
    let mut shown = value
        .compact()
        .replace(base.to_str().expect("text"), "<base>");
    if !owner.is_empty() {
        shown = shown.replace(owner, "<owner>");
    }
    Document::read(shown.replace(user, "<user>").as_bytes()).expect("a document")
}

/// The scanner base and container identity of the recorded runs.
fn identities() -> (String, String) {
    let recording = recording();
    (
        text(&recording, "base").to_owned(),
        text(&recording, "identifier").to_owned(),
    )
}

/// How the recorded engine answers.
#[derive(Clone, Debug, Default)]
struct Answers {
    fail: bool,
    changed: bool,
    state: Vec<(String, Document)>,
    drop: Vec<String>,
    inspect: Option<String>,
    remove: bool,
    remaining: String,
    inspect_status: i32,
    grow: Vec<u64>,
    status: i32,
    log: Option<String>,
    identity: String,
    pull_fails: bool,
}

impl Answers {
    /// The answers of a recorded run.
    fn of(case: &Document) -> Self {
        let answers = case.get("answers").expect("the answers");
        let truth = |name: &str| answers.get(name) == Some(&Document::Bool(true));
        let number = |name: &str| i32::try_from(whole(answers, name)).expect("a status");
        Self {
            fail: truth("fail"),
            changed: truth("changed"),
            state: answers
                .get("state")
                .and_then(Document::fields)
                .expect("a state")
                .to_vec(),
            drop: texts(answers, "drop"),
            inspect: answers
                .get("inspect")
                .and_then(Document::text)
                .map(str::to_owned),
            remove: answers
                .get("remove")
                .is_some_and(|remove| *remove != Document::Null),
            remaining: text(answers, "remaining").to_owned(),
            inspect_status: number("inspectStatus"),
            grow: answers
                .get("grow")
                .and_then(Document::items)
                .map_or_else(Vec::new, |sizes| {
                    sizes
                        .iter()
                        .map(|size| u64::try_from(size.whole().expect("a size")).expect("a size"))
                        .collect()
                }),
            status: number("status"),
            log: case.get("log").and_then(Document::text).map(str::to_owned),
            ..Self::default()
        }
    }
}

/// What the engine was asked, by every branch.
#[derive(Debug, Default)]
struct Asked {
    calls: Vec<Document>,
    owner: String,
    healthy: Vec<bool>,
}

/// An engine that answers as the reference's tests' engine does.
struct Fake {
    answers: Arc<Answers>,
    asked: Arc<Mutex<Asked>>,
    program: String,
    logs: Option<PathBuf>,
    observed: bool,
}

impl Fake {
    /// What the engine says of the container it was asked about.
    fn inspection(&self, owner: &str) -> (i32, String) {
        let answers = &self.answers;
        if let Some(said) = &answers.inspect {
            return (answers.inspect_status, said.clone());
        }
        let (base, identifier) = identities();
        let owner = if answers.changed { "foreign" } else { owner };
        let mut record = Document::record([
            ("id", identifier.into()),
            ("image", base.into()),
            ("owner", owner.into()),
            ("state", "exited".into()),
            (
                "exitStatus",
                Document::Number(i64::from(answers.status).into()),
            ),
            ("oomKilled", false.into()),
        ]);
        for (name, value) in &answers.state {
            record.put(name, value.clone());
        }
        if let Document::Record(fields) = &mut record {
            fields.retain(|(name, _)| !answers.drop.contains(name));
        }
        (answers.inspect_status, record.compact())
    }
}

impl Engine for Fake {
    fn name(&self) -> &str {
        self.program.rsplit('/').next().unwrap_or_default()
    }

    fn log(&self) -> Option<PathBuf> {
        self.logs.as_ref().map(|logs| logs.join("01-docker.log"))
    }

    fn command(
        &mut self,
        arguments: &[OsString],
        seconds: u64,
        failing: bool,
        healthy: &mut dyn FnMut() -> bool,
    ) -> Result<(i32, String)> {
        let argv: Vec<String> = arguments
            .iter()
            .map(|item| item.to_str().expect("text").to_owned())
            .collect();
        let shown = Document::List(argv.iter().map(|item| item.as_str().into()).collect());
        let mut call = Document::record([
            ("argv", shown),
            ("timeout", seconds.into()),
            ("allowFailure", failing.into()),
        ]);
        if self.observed {
            call.put("healthy", healthy().into());
        }
        let mut asked = self.asked.lock().unwrap_or_else(PoisonError::into_inner);
        asked.calls.push(call);
        let words: Vec<&str> = argv.iter().map(String::as_str).collect();
        let (_, identifier) = identities();
        let answers = Arc::clone(&self.answers);
        match words[..] {
            ["run", ..] => {
                let label = words
                    .iter()
                    .position(|word| *word == "--label")
                    .expect("a label");
                asked.owner = words[label + 1]
                    .split_once('=')
                    .expect("an owner")
                    .1
                    .to_owned();
                let mounts: Vec<usize> = (0..words.len())
                    .filter(|index| words[*index] == "--mount")
                    .collect();
                let output = words[mounts[1] + 1].split_once("src=").expect("a source").1;
                let output = PathBuf::from(output.split_once(',').expect("a target").0);
                for size in &answers.grow {
                    let grown = fs::File::create(output.join("grown")).expect("a file");
                    grown.set_len(*size).expect("a size");
                    asked.healthy.push(healthy());
                }
                if let (Some(log), Some(logs)) = (&answers.log, &self.logs) {
                    fs::write(logs.join("01-docker.log"), log).expect("a log");
                    let outcome = format!("{{\"exitStatus\": {}}}", answers.status);
                    fs::write(logs.join("01-docker.outcome.json"), outcome).expect("an outcome");
                }
                if answers.fail {
                    return Err(Fault("command_timed_out"));
                }
                Ok((answers.status, "scanner output".to_owned()))
            }
            ["container", "inspect", ..] => {
                let owner = asked.owner.clone();
                drop(asked);
                Ok(self.inspection(&owner))
            }
            ["container", "rm", "--force", ..] if answers.remove => {
                Err(Fault("image_engine_command_failed"))
            }
            ["container", "rm", "--force", ..] => Ok((0, identifier)),
            ["ps", "--all", ..] => Ok((0, answers.remaining.clone())),
            ["pull", ..] if answers.pull_fails => Err(Fault("image_engine_command_failed")),
            ["pull", ..] => Ok((0, String::new())),
            ["image", "inspect", ..] => Ok((0, answers.identity.clone())),
            _ => panic!("no answer to {argv:?}"),
        }
    }

    fn branch(&self, logs: &Path) -> Box<dyn Engine> {
        Box::new(Self {
            answers: Arc::clone(&self.answers),
            asked: Arc::clone(&self.asked),
            program: self.program.clone(),
            logs: Some(logs.to_path_buf()),
            observed: self.observed,
        })
    }
}

/// The records a run left in `root`, as the recorder showed them.
fn evidence(root: &Path, base: &Path, owner: &str, user: &str) -> Document {
    let mut found = Vec::new();
    for entry in fs::read_dir(root).expect("evidence") {
        let path = entry.expect("an entry").path();
        let name = path
            .file_name()
            .expect("a name")
            .to_str()
            .expect("text")
            .to_owned();
        if !name.ends_with(".json") || !path.is_file() {
            continue;
        }
        let record = Document::read(&fs::read(&path).expect("a record")).expect("a record");
        found.push((name, timeless(masked(&record, base, owner, user))));
    }
    found.sort_by(|left, right| left.0.cmp(&right.0));
    Document::Record(found)
}

/// Runs that the reference failed on in its interpreter's words: an
/// output that is there, the engine's own failures, and containers whose
/// record lacks a field, holds one of another kind or is no record.
const UNREAD_RUNS: [(&str, &str); 10] = [
    (
        "a run into an output that is there",
        "tool_entry_unavailable",
    ),
    ("a run whose controller fails", "command_timed_out"),
    (
        "a controller failure whose container still runs",
        "command_timed_out",
    ),
    (
        "a run whose container image is no text",
        "image_container_identity_changed",
    ),
    (
        "a run whose container has no image",
        "image_container_identity_changed",
    ),
    (
        "a run whose container identity is no text",
        "image_container_identity_changed",
    ),
    (
        "a run whose container has no identity",
        "image_container_identity_changed",
    ),
    (
        "a run whose container inspection is no JSON",
        "image_container_identity_unavailable",
    ),
    (
        "a run whose container inspection is no record",
        "image_container_identity_unavailable",
    ),
    (
        "a run whose container cannot be removed",
        "image_engine_command_failed",
    ),
];

#[test]
fn runs_match_the_recording() {
    let (recorded_base, _) = identities();
    let mut all = Vec::new();
    for case in cases("runs") {
        let name = text(&case, "name").to_owned();
        let scratch = Scratch::new("run");
        let base = scratch.0.clone();
        let root = base.join("evidence");
        fs::create_dir(&root).expect("evidence");
        fs::create_dir(base.join("tools")).expect("tools");
        let logged = case.get("logged") == Some(&Document::Bool(true));
        if logged {
            fs::create_dir(base.join("logs")).expect("logs");
        }
        let asked = Arc::new(Mutex::new(Asked::default()));
        let fake = Fake {
            answers: Arc::new(Answers::of(&case)),
            asked: Arc::clone(&asked),
            program: text(&case, "program").to_owned(),
            logs: logged.then(|| base.join("logs")),
            observed: false,
        };
        let tools = base.join("tools");
        let mut sandbox = Sandbox::new(
            Box::new(fake),
            &root,
            &tools,
            &recorded_base,
            Platform::Amd64,
            Limits::REVIEWED,
        );
        sandbox.sequence = u32::try_from(whole(&case, "sequence")).expect("a count");
        sandbox.allowance = u32::try_from(whole(&case, "limit")).expect("a count");
        sandbox.prefix = text(&case, "prefix").to_owned();
        if case.get("cancelled") == Some(&Document::Bool(true)) {
            sandbox.cancelled.store(true, Ordering::SeqCst);
        }
        if let Some(kind) = case.get("cache").and_then(Document::text) {
            if kind == "file" {
                fs::write(base.join("cache"), b"x").expect("a cache");
            }
            sandbox.cache = Some(Cache {
                directory: base.join("cache"),
                pin: "b".repeat(64),
                limits: cache::Limits::REVIEWED,
            });
        }
        if case.get("destinationExists") == Some(&Document::Bool(true)) {
            fs::create_dir(root.join("out")).expect("an output");
        }
        let mounts: Vec<(String, PathBuf)> = pairs(&case, "mounts")
            .into_iter()
            .map(|(target, source)| (target, base.join(source)))
            .collect();
        let mounts: Vec<(&str, &Path)> = mounts
            .iter()
            .map(|(target, source)| (target.as_str(), source.as_path()))
            .collect();
        let variables = pairs(&case, "extra");
        let variables: Vec<(&str, &str)> = variables
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
            .collect();
        let arguments = texts(&case, "arguments");
        let arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();
        let destination = root.join("out");
        let scan = Scan {
            tool: text(&case, "tool"),
            arguments: &arguments,
            destination: &destination,
            mounts: &mounts,
            online: case.get("online") == Some(&Document::Bool(true)),
            variables: &variables,
        };
        let came = sandbox.run(&scan).map(|(status, said)| {
            Document::List(vec![
                Document::Number(i64::from(status).into()),
                said.into(),
            ])
        });
        let (uid, gid) = sandbox.identity;
        let user = format!("{uid}:{gid}");
        let asked = asked.lock().unwrap_or_else(PoisonError::into_inner);
        let calls = masked(
            &Document::List(asked.calls.clone()),
            &base,
            &asked.owner,
            &user,
        );
        assert_eq!(Some(&calls), case.get("calls"), "{name}");
        let left = evidence(&root, &base, &asked.owner, &user);
        assert_eq!(Some(&left), case.get("evidence"), "{name}");
        assert_eq!(names(&root), texts(&case, "afterNames"), "{name}");
        let state = case.get("state").expect("a state");
        assert_eq!(
            u64::from(sandbox.sequence),
            whole(state, "sequence"),
            "{name}"
        );
        let cancelled = state.get("cancelled") == Some(&Document::Bool(true));
        assert_eq!(sandbox.cancelled(), cancelled, "{name}");
        let healthy: Vec<Document> = asked
            .healthy
            .iter()
            .map(|held| Document::Bool(*held))
            .collect();
        assert_eq!(
            Some(&Document::List(healthy)),
            case.get("healthy"),
            "{name}"
        );
        all.push((name, here(came), there(&case)));
    }
    let tally = tallied(all, &[(&UNREAD_RUNS, There::Raised)]);
    assert_eq!(tally, [20, 27, 10, 0, 0]);
}

/// Bundles that the reference failed on in its interpreter's words: a
/// scanner that is a link, and a bundle that is there.
const UNREAD_BUNDLES: [(&str, &str); 2] = [
    (
        "a bundle of a tool that is a link",
        "tool_entry_unavailable",
    ),
    ("a bundle where one is already", "tool_entry_unavailable"),
];

/// Verified scanners as the recorder installed them in `tools`, each an
/// executable that names itself.
fn installed(tools: &Path, mode: u32) {
    fs::create_dir(tools).expect("tools");
    for tool in TOOLS {
        let path = tools.join(tool);
        fs::write(&path, format!("#!/bin/sh\n# {tool}\n")).expect("a scanner");
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).expect("a mode");
    }
}

#[test]
fn bundles_match_the_recording() {
    let mut all = Vec::new();
    for case in cases("bundles") {
        let name = text(&case, "name").to_owned();
        let scratch = Scratch::new("bundle");
        let tools = scratch.0.join("tools");
        installed(&tools, 0o755);
        if case.get("link") == Some(&Document::Bool(true)) {
            fs::remove_file(tools.join("grype")).expect("a scanner");
            fs::write(tools.join("real-grype"), b"real").expect("a scanner");
            std::os::unix::fs::symlink("real-grype", tools.join("grype")).expect("a link");
        }
        let output = scratch.0.join("output");
        fs::create_dir(&output).expect("an output");
        if case.get("bundled") == Some(&Document::Bool(true)) {
            fs::create_dir(output.join("scanner-tools")).expect("a bundle");
        }
        let mut asked = Vec::new();
        let mut locate = |tool: &'static str, platform: &'static str| {
            asked.push(Document::record([
                ("name", tool.into()),
                ("platform", platform.into()),
            ]));
            Ok(tools.join(tool))
        };
        let platform = Platform::parsed(text(&case, "platform")).expect("a platform");
        let came = bundle(&mut locate, &output, platform).map(|path| {
            let relative = path.strip_prefix(&scratch.0).expect("below the scratch");
            Document::from(relative.to_str().expect("text"))
        });
        assert_eq!(Some(&Document::List(asked)), case.get("asked"), "{name}");
        let agrees = here(came.clone()) == there(&case);
        if agrees {
            assert_eq!(
                walked(&output),
                expected(case.get("after").expect("a tree")),
                "{name}"
            );
        }
        reopened(&scratch.0);
        all.push((name, here(came), there(&case)));
    }
    let tally = tallied(all, &[(&UNREAD_BUNDLES, There::Raised)]);
    assert_eq!(tally, [2, 0, 2, 0, 0]);
}

/// Preparations that the reference failed on in its interpreter's words:
/// the engine's own failure, and directories of the evidence that are
/// there.
const UNREAD_PREPARES: [(&str, &str); 3] = [
    ("a pull that fails", "image_engine_command_failed"),
    (
        "a preparation where the engine's configuration is",
        "tool_entry_unavailable",
    ),
    (
        "a preparation where the bundle is",
        "tool_entry_unavailable",
    ),
];

/// The reference of the scanner base the preparations were recorded with.
fn reference() -> String {
    format!("docker.io/library/fedora:44@sha256:{}", "d".repeat(64))
}

#[test]
fn preparations_match_the_recording() {
    let mut all = Vec::new();
    for case in cases("prepares") {
        let name = text(&case, "name").to_owned();
        let scratch = Scratch::new("prepare");
        let base = scratch.0.clone();
        let tools = base.join("tools");
        installed(&tools, 0o600);
        let output = base.join("output");
        fs::create_dir(&output).expect("an output");
        fs::set_permissions(&output, fs::Permissions::from_mode(0o700)).expect("a mode");
        if case.get("configured") == Some(&Document::Bool(true)) {
            fs::create_dir(output.join("engine-config")).expect("a configuration");
        }
        if case.get("bundled") == Some(&Document::Bool(true)) {
            fs::create_dir(output.join("scanner-tools")).expect("a bundle");
        }
        let environment: BTreeMap<String, OsString> = pairs(&case, "environment")
            .into_iter()
            .map(|(key, value)| (key, value.into()))
            .collect();
        let ambient = |name: &str| environment.get(name).cloned();
        let asked = Arc::new(Mutex::new(Asked::default()));
        let answers = Answers {
            identity: text(&case, "identity").to_owned(),
            pull_fails: case.get("pullFails") == Some(&Document::Bool(true)),
            ..Answers::default()
        };
        let mut made: Option<(Vec<(&'static str, OsString)>, PathBuf)> = None;
        let mut engine = |given: Vec<(&'static str, OsString)>, logs: PathBuf| -> Box<dyn Engine> {
            made = Some((given, logs));
            Box::new(Fake {
                answers: Arc::new(answers.clone()),
                asked: Arc::clone(&asked),
                program: "/usr/local/bin/docker".to_owned(),
                logs: None,
                observed: true,
            })
        };
        let mut located = Vec::new();
        let mut locate = |tool: &'static str, platform: &'static str| {
            located.push(Document::record([
                ("name", tool.into()),
                ("platform", platform.into()),
            ]));
            Ok(tools.join(tool))
        };
        let cache = case
            .get("cache")
            .and_then(Document::text)
            .map(|directory| Cache {
                directory: PathBuf::from(directory),
                pin: "b".repeat(64),
                limits: cache::Limits::REVIEWED,
            });
        let base_reference = reference();
        let setup = Setup {
            output: &output,
            ambient: &ambient,
            locate: &mut locate,
            platform: Platform::parsed(text(&case, "platform")).expect("a platform"),
            base: &base_reference,
            cancelled: Arc::new(AtomicBool::new(false)),
            cache,
            limits: Limits::REVIEWED,
        };
        let prepared = prepare(setup, &mut engine);
        let shown = |path: &Path| {
            path.to_str()
                .expect("text")
                .replace(base.to_str().expect("text"), "<base>")
        };
        let sandbox = prepared.as_ref().ok().map(|sandbox| {
            let (given, logs) = made.clone().expect("an engine");
            let given = given
                .into_iter()
                .map(|(key, value)| (key.to_owned(), Document::from(shown(Path::new(&value)))));
            let mut given: Vec<(String, Document)> = given.collect();
            given.sort_by(|left, right| left.0.cmp(&right.0));
            Document::record([
                ("env", Document::Record(given)),
                ("output", shown(&sandbox.output).into()),
                ("tools", shown(&sandbox.tools).into()),
                ("base", sandbox.base.as_str().into()),
                ("platform", sandbox.platform.name().into()),
                ("logs", shown(&logs).into()),
                ("sequence", u64::from(sandbox.sequence).into()),
                ("limit", u64::from(sandbox.allowance).into()),
                ("prefix", sandbox.prefix.as_str().into()),
                (
                    "cache",
                    sandbox
                        .cache
                        .as_ref()
                        .map_or(Document::Null, |cache| shown(&cache.directory).into()),
                ),
                ("cancelled", sandbox.cancelled().into()),
            ])
        });
        let recorded = case.get("sandbox").cloned().expect("a sandbox").sorted();
        let recorded = match recorded {
            Document::Record(mut fields) => {
                fields.retain(|(name, _)| name != "engine");
                Document::Record(fields)
            }
            other => other,
        };
        assert_eq!(
            sandbox.map_or(Document::Null, Document::sorted),
            recorded,
            "{name}"
        );
        let calls = asked
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .calls
            .clone();
        assert_eq!(Some(&Document::List(calls)), case.get("calls"), "{name}");
        assert_eq!(Some(&Document::List(located)), case.get("asked"), "{name}");
        let came = prepared.map(|sandbox| Document::from(sandbox.base));
        if here(came.clone()) == there(&case) {
            assert_eq!(
                walked(&output),
                expected(case.get("after").expect("a tree")),
                "{name}"
            );
        }
        reopened(&base);
        all.push((name, here(came), there(&case)));
    }
    let tally = tallied(all, &[(&UNREAD_PREPARES, There::Raised)]);
    assert_eq!(tally, [6, 5, 3, 0, 0]);
}

#[test]
fn branches_match_the_recording() {
    let (recorded_base, _) = identities();
    for case in cases("branches") {
        let name = text(&case, "name").to_owned();
        let scratch = Scratch::new("branch");
        let base = scratch.0.clone();
        let root = base.join("evidence");
        fs::create_dir_all(root.join("scanner-commands")).expect("a log");
        let asked = Arc::new(Mutex::new(Asked::default()));
        let fake = Fake {
            answers: Arc::new(Answers::default()),
            asked: Arc::clone(&asked),
            program: "/usr/bin/docker".to_owned(),
            logs: Some(root.join("scanner-commands")),
            observed: false,
        };
        let tools = base.join("tools");
        let selected = Sandbox::new(
            Box::new(fake),
            &root,
            &tools,
            &recorded_base,
            Platform::Amd64,
            Limits::REVIEWED,
        );
        let labels = texts(&case, "names");
        let allowances = case
            .get("allowances")
            .and_then(Document::items)
            .expect("allowances");
        let allowances: Vec<u32> = allowances
            .iter()
            .map(|allowance| u32::try_from(allowance.whole().expect("a count")).expect("a count"))
            .collect();
        let mut made = Vec::new();
        let mut asked_for: Vec<(&str, u32)> = labels
            .iter()
            .map(String::as_str)
            .zip(allowances.iter().copied())
            .collect();
        if case.get("twice") == Some(&Document::Bool(true)) {
            asked_for.push(asked_for[0]);
        }
        let mut first = None;
        for (label, allowance) in asked_for {
            match selected.branch(label, allowance) {
                Ok(branch) => made.push(branch),
                Err(fault) => first = Some(format!("refused {fault}")),
            }
        }
        let shown = |path: &Path| {
            path.to_str()
                .expect("text")
                .replace(base.to_str().expect("text"), "<base>")
        };
        let described: Vec<Document> = made
            .iter()
            .map(|branch| {
                Document::record([
                    ("sequence", u64::from(branch.sequence).into()),
                    ("prefix", branch.prefix.as_str().into()),
                    ("limit", u64::from(branch.allowance).into()),
                    (
                        "logs",
                        shown(&root.join(format!("{}commands", branch.prefix))).into(),
                    ),
                    ("slots", Arc::ptr_eq(&branch.slots, &selected.slots).into()),
                    (
                        "cancelled",
                        Arc::ptr_eq(&branch.cancelled, &selected.cancelled).into(),
                    ),
                    ("base", branch.base.as_str().into()),
                ])
            })
            .collect();
        let mut outcomes =
            vec![first.unwrap_or_else(|| format!("held {}", Document::List(described).pretty()))];
        let runs = whole(&case, "runs");
        for branch in &mut made {
            for index in 0..runs {
                let destination = root.join(format!("{}out-{index}", branch.prefix));
                let scan = Scan {
                    tool: "syft",
                    arguments: &["--help"],
                    destination: &destination,
                    mounts: &[],
                    online: false,
                    variables: &[],
                };
                let came = branch.run(&scan).map(|(status, said)| {
                    Document::List(vec![
                        Document::Number(i64::from(status).into()),
                        said.into(),
                    ])
                });
                outcomes.push(here(came));
            }
        }
        let recorded: Vec<String> = case
            .get("outcomes")
            .and_then(Document::items)
            .expect("outcomes")
            .iter()
            .map(|outcome| there(&Document::record([("result", outcome.clone())])))
            .collect();
        assert_eq!(outcomes.len(), recorded.len(), "{name}");
        for (index, (here, there)) in outcomes.iter().zip(&recorded).enumerate() {
            if there.starts_with("RAISED ") {
                // A branch whose log is there: the reference's interpreter
                // said that the file exists.
                assert_eq!(
                    (index, here.as_str()),
                    (0, "refused tool_entry_unavailable"),
                    "{name}"
                );
            } else {
                assert_eq!(here, there, "{name}");
            }
        }
        assert_eq!(names(&root), texts(&case, "afterNames"), "{name}");
    }
}

#[test]
fn bounds_and_names_are_the_reviewed_ones() {
    let recording = recording();
    let limits = recording.get("limits").expect("the limits");
    let bound = |name: &str| whole(limits, name);
    let reviewed = Limits::REVIEWED;
    for (held, recorded) in [
        (reviewed.output, "MAX_OUTPUT"),
        (reviewed.update, "MAX_DB_UPDATE"),
        (reviewed.entries, "MAX_FILES"),
        (reviewed.seconds, "SCAN_SECONDS"),
        (reviewed.scratch, "DEFAULT_SCRATCH_BYTES"),
        (reviewed.catalog, "SYFT_SCAN_SCRATCH_BYTES"),
        (u64::from(reviewed.scanners), "MAX_SCANNERS"),
        (reviewed.log, "MAX_COMMAND_LOG"),
        (
            u64::try_from(SLOTS).expect("a count"),
            "MAX_ACTIVE_SCANNERS",
        ),
        (diagnostic::FINDINGS, "FINDINGS_EXIT"),
    ] {
        assert_eq!(held, bound(recorded), "{recorded}");
    }
    assert_eq!(LABEL, text(limits, "LABEL"));
    assert_eq!(TOOLS.to_vec(), texts(limits, "TOOLS"));
    let platforms = pairs(limits, "PLATFORMS");
    assert_eq!(platforms.len(), 2);
    for (engine, tools) in platforms {
        let platform = Platform::parsed(&engine).expect("a platform");
        assert_eq!(
            (platform.name(), platform.tools()),
            (engine.as_str(), tools.as_str())
        );
    }
    for other in ["linux/386", "linux/arm64/v8", "Linux/amd64", ""] {
        assert_eq!(Platform::parsed(other), None, "{other}");
    }
}

#[test]
fn owners_are_random_identifiers_of_the_fourth_version() {
    let first = owner().expect("an owner");
    let second = owner().expect("an owner");
    assert_ne!(first, second);
    for owner in [first, second] {
        assert!(crate::image::small_hex(&owner, 32), "{owner}");
        assert_eq!(&owner[12..13], "4", "{owner}");
        assert!("89ab".contains(&owner[16..17]), "{owner}");
    }
}

/// What a scanner does when an engine that answers well runs it.
type Behaviour = Arc<dyn Fn(&mut dyn FnMut() -> bool) -> Result<i32> + Send + Sync>;

/// An engine that answers well, but for a container that may be another's,
/// and runs a scanner as `run` says.
struct Plain {
    run: Behaviour,
    changed: bool,
    owner: String,
    calls: Arc<Mutex<Vec<Vec<String>>>>,
}

impl Plain {
    fn new(run: Behaviour, changed: bool) -> (Self, Arc<Mutex<Vec<Vec<String>>>>) {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let engine = Self {
            run,
            changed,
            owner: String::new(),
            calls: Arc::clone(&calls),
        };
        (engine, calls)
    }
}

impl Engine for Plain {
    fn name(&self) -> &str {
        "docker"
    }

    fn log(&self) -> Option<PathBuf> {
        None
    }

    fn command(
        &mut self,
        arguments: &[OsString],
        _seconds: u64,
        _failing: bool,
        healthy: &mut dyn FnMut() -> bool,
    ) -> Result<(i32, String)> {
        let argv: Vec<String> = arguments
            .iter()
            .map(|item| item.to_str().expect("text").to_owned())
            .collect();
        self.calls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(argv.clone());
        let (base, identifier) = identities();
        match argv[0].as_str() {
            "run" => {
                let label = argv
                    .iter()
                    .position(|word| word == "--label")
                    .expect("a label");
                self.owner = argv[label + 1]
                    .split_once('=')
                    .expect("an owner")
                    .1
                    .to_owned();
                Ok(((self.run)(healthy)?, String::new()))
            }
            "container" if argv[1] == "inspect" => {
                let owner = if self.changed {
                    "foreign"
                } else {
                    self.owner.as_str()
                };
                let record = Document::record([
                    ("id", identifier.into()),
                    ("image", base.into()),
                    ("owner", owner.into()),
                    ("state", "exited".into()),
                    ("exitStatus", 0_u64.into()),
                    ("oomKilled", false.into()),
                ]);
                Ok((0, record.compact()))
            }
            "container" => Ok((0, identifier)),
            "ps" => Ok((0, String::new())),
            other => panic!("no answer to {other}"),
        }
    }

    fn branch(&self, _logs: &Path) -> Box<dyn Engine> {
        Box::new(Self {
            run: Arc::clone(&self.run),
            changed: self.changed,
            owner: String::new(),
            calls: Arc::clone(&self.calls),
        })
    }
}

/// A sandbox of `engine` with its evidence in a new directory of `scratch`.
fn sandboxed(engine: Plain, scratch: &Scratch) -> Sandbox {
    let root = scratch.0.join("evidence");
    fs::create_dir(&root).expect("evidence");
    let (base, _) = identities();
    let tools = scratch.0.join("tools");
    Sandbox::new(
        Box::new(engine),
        &root,
        &tools,
        &base,
        Platform::Amd64,
        Limits::REVIEWED,
    )
}

/// A scan of nothing by the catalog into `destination`.
fn plain(destination: &Path) -> Scan<'_> {
    Scan {
        tool: "syft",
        arguments: &["--help"],
        destination,
        mounts: &[],
        online: false,
        variables: &[],
    }
}

#[test]
fn an_unproved_removal_cancels_a_waiter_before_its_slot_is_freed() {
    let (began, beginning) = mpsc::channel();
    let (release, released) = mpsc::channel::<()>();
    let released = Mutex::new(released);
    let run: Behaviour = Arc::new(move |_| {
        began.send(()).expect("a start");
        let waited = released
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .recv_timeout(Duration::from_secs(3));
        waited.expect("a release");
        Ok(0)
    });
    let scratch = Scratch::new("waiter");
    let (engine, calls) = Plain::new(run, true);
    let mut first = sandboxed(engine, &scratch);
    first.slots = Arc::new(Slots::new(1));
    let mut waiter = first.branch("waiter", 1).expect("a branch");
    let (database, queued) = (scratch.0.join("db"), scratch.0.join("queued"));
    let update = Scan {
        tool: "grype",
        arguments: &["db", "update"],
        destination: &database,
        mounts: &[],
        online: true,
        variables: &[],
    };
    std::thread::scope(|scope| {
        let running = scope.spawn(|| first.run(&update));
        beginning
            .recv_timeout(Duration::from_secs(3))
            .expect("the first scanner began");
        let waiting = scope.spawn(|| waiter.run(&plain(&queued)));
        std::thread::sleep(Duration::from_millis(300));
        release.send(()).expect("a release");
        let first = running.join().expect("the first scanner");
        assert_eq!(first, Err(Fault("image_container_identity_changed")));
        assert_eq!(
            waiting.join().expect("the waiter"),
            Err(Fault("image_scan_cancelled"))
        );
    });
    assert_eq!(waiter.sequence, 0);
    assert!(!queued.exists());
    let calls = calls.lock().unwrap_or_else(PoisonError::into_inner).clone();
    assert_eq!(calls.iter().filter(|call| call[0] == "run").count(), 1);
    assert!(!calls.iter().any(|call| call[..2] == ["container", "rm"]));
    let cancelled = AtomicBool::new(false);
    assert!(first.slots.taken(&cancelled).is_ok());
    let evidence = first.output.join("scanner-result-01.json");
    let diagnostic = Document::read(&fs::read(evidence).expect("a diagnostic")).expect("a record");
    assert_eq!(
        diagnostic.get("cleanupVerified"),
        Some(&Document::Bool(false))
    );
}

#[test]
fn two_scanners_run_at_once_and_no_more() {
    let active = Arc::new(AtomicUsize::new(0));
    let most = Arc::new(AtomicUsize::new(0));
    let (now, peak) = (Arc::clone(&active), Arc::clone(&most));
    let started = Arc::new(AtomicUsize::new(0));
    // The first scanner outlasts the others, so that a slot freed twice
    // would let a third run beside it.
    let run: Behaviour = Arc::new(move |_| {
        let running = now.fetch_add(1, Ordering::SeqCst) + 1;
        peak.fetch_max(running, Ordering::SeqCst);
        let first = started.fetch_add(1, Ordering::SeqCst) == 0;
        std::thread::sleep(Duration::from_millis(if first { 600 } else { 150 }));
        now.fetch_sub(1, Ordering::SeqCst);
        Ok(0)
    });
    let scratch = Scratch::new("slots");
    let (engine, calls) = Plain::new(run, false);
    let selected = sandboxed(engine, &scratch);
    let mut branches: Vec<Sandbox> = ["database", "sbom", "secrets", "fourth"]
        .iter()
        .map(|name| selected.branch(name, 2).expect("a branch"))
        .collect();
    std::thread::scope(|scope| {
        for (index, branch) in branches.iter_mut().enumerate() {
            let destination = scratch.0.join(format!("out-{index}"));
            scope.spawn(move || branch.run(&plain(&destination)).expect("a scan"));
        }
    });
    assert_eq!(most.load(Ordering::SeqCst), SLOTS);
    assert_eq!(active.load(Ordering::SeqCst), 0);
    assert_eq!(
        calls.lock().unwrap_or_else(PoisonError::into_inner).len(),
        4 * 4
    );
}

#[test]
fn cancellation_stops_a_running_scanner_and_removes_its_container() {
    let (began, beginning) = mpsc::channel();
    let run: Behaviour = Arc::new(move |healthy| {
        began.send(()).expect("a start");
        let deadline = Instant::now() + Duration::from_secs(3);
        while healthy() {
            assert!(Instant::now() < deadline, "cancellation was not observed");
            std::thread::sleep(Duration::from_millis(10));
        }
        Err(Fault("command_lease_lost"))
    });
    let scratch = Scratch::new("cancel");
    let (engine, calls) = Plain::new(run, false);
    let mut sandbox = sandboxed(engine, &scratch);
    let cancelled = Arc::clone(&sandbox.cancelled);
    let destination = scratch.0.join("out");
    std::thread::scope(|scope| {
        let running = scope.spawn(|| sandbox.run(&plain(&destination)));
        beginning
            .recv_timeout(Duration::from_secs(3))
            .expect("the scanner began");
        cancelled.store(true, Ordering::SeqCst);
        assert_eq!(
            running.join().expect("the scanner"),
            Err(Fault("command_lease_lost"))
        );
    });
    let (_, identifier) = identities();
    let calls = calls.lock().unwrap_or_else(PoisonError::into_inner).clone();
    assert!(calls.contains(&vec![
        "container".to_owned(),
        "rm".into(),
        "--force".into(),
        identifier
    ]));
    let next = scratch.0.join("next");
    assert_eq!(
        sandbox.run(&plain(&next)),
        Err(Fault("image_scan_cancelled"))
    );
}

#[test]
fn a_cancelled_preparation_pulls_nothing_it_needs() {
    let scratch = Scratch::new("cancelled-preparation");
    let tools = scratch.0.join("tools");
    installed(&tools, 0o600);
    let output = scratch.0.join("output");
    fs::create_dir(&output).expect("an output");
    let asked = Arc::new(Mutex::new(Asked::default()));
    let answers = Answers {
        identity: format!("sha256:{}", "c".repeat(64)),
        ..Answers::default()
    };
    let mut engine = |_: Vec<(&'static str, OsString)>, _: PathBuf| -> Box<dyn Engine> {
        Box::new(Fake {
            answers: Arc::new(answers.clone()),
            asked: Arc::clone(&asked),
            program: "/usr/bin/docker".to_owned(),
            logs: None,
            observed: true,
        })
    };
    let base_reference = reference();
    let setup = Setup {
        output: &output,
        ambient: &|_| None,
        locate: &mut |tool, _| Ok(tools.join(tool)),
        platform: Platform::Amd64,
        base: &base_reference,
        cancelled: Arc::new(AtomicBool::new(true)),
        cache: None,
        limits: Limits::REVIEWED,
    };
    let sandbox = prepare(setup, &mut engine).expect("a sandbox");
    assert!(sandbox.cancelled());
    let calls = asked
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .calls
        .clone();
    assert_eq!(calls.len(), 2);
    for call in calls {
        assert_eq!(call.get("healthy"), Some(&Document::Bool(false)));
    }
    reopened(&scratch.0);
}

#[test]
fn a_waiter_is_refused_when_cancelled_while_the_slot_stays_held() {
    let (began, beginning) = mpsc::channel();
    let (release, released) = mpsc::channel::<()>();
    let released = Mutex::new(released);
    let run: Behaviour = Arc::new(move |_| {
        began.send(()).expect("a start");
        let waited = released
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .recv_timeout(Duration::from_secs(5));
        waited.expect("a release");
        Ok(0)
    });
    let scratch = Scratch::new("held");
    let (engine, _) = Plain::new(run, false);
    let mut holder = sandboxed(engine, &scratch);
    holder.slots = Arc::new(Slots::new(1));
    let mut waiter = holder.branch("waiter", 1).expect("a branch");
    let cancelled = Arc::clone(&holder.cancelled);
    let (held, queued) = (scratch.0.join("held"), scratch.0.join("queued"));
    std::thread::scope(|scope| {
        let holding = scope.spawn(|| holder.run(&plain(&held)));
        beginning
            .recv_timeout(Duration::from_secs(3))
            .expect("the holder began");
        let (answered, answer) = mpsc::channel();
        let waiting = &mut waiter;
        let queued = &queued;
        scope.spawn(move || answered.send(waiting.run(&plain(queued))));
        std::thread::sleep(Duration::from_millis(200));
        cancelled.store(true, Ordering::SeqCst);
        let refused = answer.recv_timeout(Duration::from_secs(3));
        release.send(()).expect("a release");
        let refused = refused.expect("the waiter answered while the slot was held");
        assert_eq!(refused, Err(Fault("image_scan_cancelled")));
        assert_eq!(holding.join().expect("the holder"), Ok((0, String::new())));
    });
    assert_eq!(waiter.sequence, 0);
    assert!(!queued.exists());
}

#[test]
fn a_branch_counts_its_own_scanners_and_keeps_the_cache_and_the_account() {
    let run: Behaviour = Arc::new(|_| Ok(0));
    let scratch = Scratch::new("branch-own");
    let (engine, _) = Plain::new(run, false);
    let mut selected = sandboxed(engine, &scratch);
    selected.cache = Some(Cache {
        directory: scratch.0.join("cache"),
        pin: "b".repeat(64),
        limits: cache::Limits::REVIEWED,
    });
    selected
        .run(&plain(&scratch.0.join("first")))
        .expect("a scan");
    assert_eq!(selected.sequence, 1);
    let branch = selected.branch("sbom", 2).expect("a branch");
    assert_eq!(branch.sequence, 0);
    let directory = |sandbox: &Sandbox| sandbox.cache.as_ref().map(|cache| cache.directory.clone());
    assert_eq!(directory(&branch), directory(&selected));
    assert_eq!(
        (branch.identity, branch.superuser),
        (selected.identity, selected.superuser)
    );
    assert_eq!(
        (branch.limits, branch.platform),
        (selected.limits, selected.platform)
    );
    assert_eq!(
        (&branch.output, &branch.tools),
        (&selected.output, &selected.tools)
    );
}

#[test]
fn a_diagnostic_that_cannot_be_kept_is_answered_before_a_failed_removal() {
    let (recorded_base, _) = identities();
    let scratch = Scratch::new("unkept");
    let root = scratch.0.join("evidence");
    fs::create_dir(&root).expect("evidence");
    fs::write(root.join("scanner-result-01.json"), b"{}\n").expect("a diagnostic");
    let asked = Arc::new(Mutex::new(Asked::default()));
    let answers = Answers {
        remove: true,
        ..Answers::default()
    };
    let fake = Fake {
        answers: Arc::new(answers),
        asked: Arc::clone(&asked),
        program: "/usr/bin/docker".to_owned(),
        logs: None,
        observed: false,
    };
    let tools = scratch.0.join("tools");
    let mut sandbox = Sandbox::new(
        Box::new(fake),
        &root,
        &tools,
        &recorded_base,
        Platform::Amd64,
        Limits::REVIEWED,
    );
    let destination = root.join("out");
    assert_eq!(
        sandbox.run(&plain(&destination)),
        Err(Fault("image_filesystem_error"))
    );
    assert!(sandbox.cancelled());
    let calls = asked
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .calls
        .len();
    assert_eq!(calls, 3);
}

#[test]
fn a_scanner_that_is_no_regular_file_is_not_bundled() {
    let scratch = Scratch::new("unbundled");
    let tools = scratch.0.join("tools");
    installed(&tools, 0o755);
    fs::remove_file(tools.join("grype")).expect("a scanner");
    let made = std::process::Command::new("mkfifo")
        .arg(tools.join("grype"))
        .status();
    assert!(made.expect("mkfifo").success());
    let output = scratch.0.join("output");
    fs::create_dir(&output).expect("an output");
    let came = bundle(
        &mut |tool, _| Ok(tools.join(tool)),
        &output,
        Platform::Amd64,
    );
    assert_eq!(came, Err(Fault("nonregular_tool_file")));
    reopened(&scratch.0);
}
