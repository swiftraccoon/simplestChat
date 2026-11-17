//! Replays of `recorded_suites.json`: what the reference in Python made of
//! the reviewed corpus and of its variants, of the closure of the worker's
//! sources, of the sandbox, the build's commands and environment, the
//! replay's command line, the build's budget, image identities and
//! machines. Where the reference failed in the words of its interpreter,
//! the tables name the code that refuses the case here.

use super::corpus::{Case, corpus};
use crate::{Fault, document::Document, scratch::Scratch};
use std::{
    cell::Cell,
    fs,
    os::unix::fs::{PermissionsExt as _, symlink},
    path::{Path, PathBuf},
    rc::Rc,
    time::Duration,
};

const RECORDING: &[u8] = include_bytes!("recorded_suites.json");

/// The recording.
pub(crate) fn recording() -> Document {
    Document::read(RECORDING).expect("a recording")
}

/// The text `name` of `value`.
pub(crate) fn text<'a>(value: &'a Document, name: &str) -> &'a str {
    let held = value.get(name).and_then(Document::text);
    held.unwrap_or_else(|| panic!("no text {name}"))
}

/// What a case came to there: what was held, the refusal, or the class of
/// what the reference's interpreter raised.
pub(crate) fn there(case: &Document) -> String {
    let result = case.get("result").expect("a result");
    if let Some(held) = result.get("held") {
        return format!("held {}", held.pretty());
    }
    if let Some(words) = result.get("refused").and_then(Document::text) {
        return format!("refused {words}");
    }
    format!("RAISED {}", text(result, "raised"))
}

/// What a case came to here.
pub(crate) fn here(came: crate::Result<Document>) -> String {
    match came {
        Ok(held) => format!("held {}", held.pretty()),
        Err(fault) => format!("refused {fault}"),
    }
}

/// Hold every case to the recording, but for those that `raised` names
/// with the code that refuses them here, where the reference raised.
/// Answers how many came to the same held, the same refusal, and to a
/// refusal here where the reference raised.
pub(crate) fn tallied(all: Vec<(String, String, String)>, raised: &[(&str, &str)]) -> [usize; 3] {
    let mut tally = [0; 3];
    for (name, here, there) in all {
        if let Some((_, code)) = raised.iter().find(|(other, _)| *other == name) {
            assert_eq!(here, format!("refused {code}"), "{name}");
            assert!(there.starts_with("RAISED "), "{name}");
            tally[2] += 1;
        } else {
            assert_eq!(here, there, "{name}");
            tally[usize::from(here.starts_with("refused "))] += 1;
        }
    }
    tally
}

/// The bytes that the hexadecimal text `spelled` spells.
fn unspelled(spelled: &str) -> Vec<u8> {
    (0..spelled.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&spelled[index..index + 2], 16).expect("hex"))
        .collect()
}

/// Build what `entry` records at `path`: a directory, a file of the bytes
/// it spells or of the bytes it repeats so many times, or a link to a place
/// below `root`.
fn built(root: &Path, path: &Path, entry: &Document) {
    if entry.get("directory").is_some() {
        fs::create_dir_all(path).expect("a directory");
        return;
    }
    fs::create_dir_all(path.parent().expect("a parent")).expect("directories");
    if let Some(target) = entry.get("link").and_then(Document::text) {
        let target = target.replace("<root>", &root.to_string_lossy());
        symlink(target, path).expect("a link");
    } else if let Some(repeated) = entry.get("repeat").and_then(Document::text) {
        let count = entry
            .get("count")
            .and_then(Document::whole)
            .expect("a count");
        let repeated = unspelled(repeated).repeat(usize::try_from(count).expect("a size"));
        fs::write(path, repeated).expect("a file");
    } else {
        fs::write(path, unspelled(text(entry, "hex"))).expect("a file");
    }
}

/// The tree `security/native` below `root` that the recorded `base` and a
/// case's `changed` entries make, an entry of nothing being absent.
pub(crate) fn tree(root: &Path, base: &Document, changed: &Document) {
    let mut entries: Vec<(String, Document)> = base.fields().expect("a base").to_vec();
    for (name, entry) in changed.fields().expect("changes") {
        entries.retain(|(other, _)| other != name);
        if *entry != Document::Null {
            entries.push((name.clone(), entry.clone()));
        }
    }
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    let native = root.join("security/native");
    fs::create_dir_all(&native).expect("a tree");
    for (name, entry) in &entries {
        built(root, &native.join(name), entry);
    }
}

/// A case as the recorder listed it.
fn shown(case: &Case) -> Document {
    Document::record([
        ("bytes", (case.data.len() as u64).into()),
        ("family", case.family.as_str().into()),
        ("id", case.identifier.as_str().into()),
        ("path", case.relative.as_str().into()),
        ("sha256", case.digest.as_str().into()),
    ])
}

/// The corpora the reference failed in its interpreter's words: cases that
/// are no list, a field named twice, no JSON, and an input it could not open
/// through its final link, to nothing or to a reviewed input.
const UNREAD_CORPORA: [(&str, &str); 5] = [
    ("a manifest whose cases are no list", "native_corpus_unread"),
    ("a manifest naming a field twice", "native_corpus_unread"),
    ("a manifest that is no JSON", "native_corpus_unread"),
    ("an input that is a link", "native_input_unreadable"),
    (
        "an input that is a link to a reviewed input",
        "native_input_unreadable",
    ),
];

#[test]
fn every_recorded_corpus_is_read_alike() {
    let recording = recording();
    let recorded = recording.get("corpus").expect("corpora");
    let base = recorded.get("base").expect("a base");
    let cases = recorded
        .get("cases")
        .and_then(Document::items)
        .expect("cases");
    let mut all = Vec::new();
    for case in cases {
        let scratch = Scratch::new("corpus");
        tree(&scratch.0, base, case.get("changed").expect("changes"));
        let came =
            corpus(&scratch.0).map(|cases| Document::List(cases.iter().map(shown).collect()));
        all.push((text(case, "name").to_owned(), here(came), there(case)));
    }
    assert_eq!(tallied(all, &UNREAD_CORPORA), [8, 45, 5]);
}

/// The cases of the part `name` of the recording.
fn part(name: &str) -> Vec<Document> {
    let recording = recording();
    let held = recording.get(name).and_then(Document::items);
    held.unwrap_or_else(|| panic!("no part {name}")).to_vec()
}

/// The whole number `name` of `value`.
fn whole(value: &Document, name: &str) -> i128 {
    let held = value.get(name).and_then(Document::whole);
    held.unwrap_or_else(|| panic!("no number {name}"))
}

/// Texts as a recorded list.
fn listed(words: &[String]) -> Document {
    Document::List(words.iter().map(|word| word.as_str().into()).collect())
}

#[test]
fn every_recorded_budget_is_reckoned_alike() {
    let mut all = Vec::new();
    for case in part("budgets") {
        let cpus = u64::try_from(whole(&case, "cpus")).expect("cpus");
        let memory = u64::try_from(whole(&case, "memoryBytes")).expect("bytes");
        let requested = case.get("override").and_then(Document::text);
        let came = crate::budget::budget(cpus, memory, requested).map(|budget| {
            Document::List(vec![
                budget.workers.into(),
                budget.ram_mib.into(),
                u64::try_from(budget.query_ram_mib).expect("a size").into(),
            ])
        });
        let name = format!("{cpus} {memory} {requested:?}");
        all.push((name, here(came), there(&case)));
    }
    assert_eq!(tallied(all, &[]), [147, 469, 0]);
}

#[test]
fn every_recorded_sandbox_is_made_alike() {
    let mut all = Vec::new();
    for case in part("sandbox") {
        let numbers: Vec<i128> = case
            .get("budget")
            .and_then(Document::items)
            .expect("a budget")
            .iter()
            .map(|number| number.whole().expect("a number"))
            .collect();
        let budget = crate::budget::Budget {
            workers: u64::try_from(numbers[0]).expect("workers"),
            ram_mib: u64::try_from(numbers[1]).expect("memory"),
            query_ram_mib: i64::try_from(numbers[2]).expect("the queries' memory"),
        };
        let (image, run_id) = (text(&case, "image"), text(&case, "runId"));
        let came = super::sandbox::sandbox_args(image, run_id, &"d".repeat(64), budget, "amd64");
        let name = format!("{numbers:?} {image} {run_id}");
        all.push((name, here(came.map(|words| listed(&words))), there(&case)));
    }
    assert_eq!(tallied(all, &[]), [3, 18, 0]);
}

#[test]
fn every_recorded_build_is_commanded_alike() {
    let mut all = Vec::new();
    for case in part("commands") {
        let (mode, workers) = (text(&case, "mode"), whole(&case, "workers"));
        let came = super::sandbox::compile_commands(mode, u64::try_from(workers).expect("workers"));
        // The reference's record of the phases is sorted by their names.
        let came = came.map(|phases| {
            let mut phases: Vec<(String, Document)> = phases
                .into_iter()
                .map(|(phase, words)| (phase.to_owned(), listed(&words)))
                .collect();
            phases.sort_by(|left, right| left.0.cmp(&right.0));
            Document::Record(phases)
        });
        all.push((format!("{mode} {workers}"), here(came), there(&case)));
    }
    assert_eq!(tallied(all, &[]), [12, 12, 0]);
}

#[test]
fn every_recorded_environment_is_made_alike() {
    let mut all = Vec::new();
    for case in part("environments") {
        let ambient = case.get("ambient").expect("an environment").clone();
        let lookup = |name: &str| {
            ambient
                .get(name)
                .and_then(Document::text)
                .map(str::to_owned)
        };
        let family = case.get("family").and_then(Document::text);
        let came = super::sandbox::worker_environment(&lookup, family).map(|pairs| {
            let mut pairs: Vec<(String, Document)> = pairs
                .into_iter()
                .map(|(name, value)| (name, value.as_str().into()))
                .collect();
            pairs.sort_by(|left, right| left.0.cmp(&right.0));
            Document::Record(pairs)
        });
        all.push((format!("{family:?}"), here(came), there(&case)));
    }
    assert_eq!(tallied(all, &[]), [9, 1, 0]);
}

#[test]
fn the_recorded_replay_runs_one_input_once() {
    let recording = recording();
    let replay = recording.get("replay").expect("a replay");
    let path = text(replay, "path");
    let came = super::sandbox::replay_args("/work/build/mediasoup-worker-fuzzer", path);
    assert_eq!(&listed(&came), replay.get("argv").expect("a command line"));
}

#[test]
fn every_recorded_identity_and_machine_is_read_alike() {
    let mut all = Vec::new();
    for case in part("ids") {
        let value = text(&case, "value");
        let came = super::sandbox::canonical_image_id(value).map(|id| id.as_str().into());
        all.push((value.to_owned(), here(came), there(&case)));
    }
    for case in part("machines") {
        let machine = text(&case, "machine");
        let came = super::sandbox::native_architecture(machine).map(Document::from);
        all.push((machine.to_owned(), here(came), there(&case)));
    }
    assert_eq!(tallied(all, &[]), [6, 8, 0]);
}

const INPUTS: &[u8] = include_bytes!("recorded_inputs.json");

/// The recorded fixture tree of an image's inputs, built below `root`.
fn inputs_fixture(root: &Path, recorded: &Document) {
    use std::os::unix::fs::PermissionsExt as _;
    for (name, entry) in recorded
        .get("tree")
        .and_then(Document::fields)
        .expect("a tree")
    {
        let path = root.join(name);
        fs::create_dir_all(path.parent().expect("a parent")).expect("directories");
        fs::write(&path, text(entry, "text")).expect("a file");
        let mode =
            u32::from_str_radix(text(entry, "mode").trim_start_matches("0o"), 8).expect("a mode");
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).expect("a mode");
    }
}

/// The paths of `files` relative to `root`, as recorded.
fn relative(files: &[std::path::PathBuf], root: &Path) -> Document {
    let named = files.iter().map(|path| {
        let local = path.strip_prefix(root).expect("below the root");
        Document::from(local.to_string_lossy().as_ref())
    });
    Document::List(named.collect())
}

#[test]
fn the_recorded_inputs_are_bound_and_copied_alike() {
    use std::os::unix::fs::PermissionsExt as _;
    let recorded = Document::read(INPUTS).expect("a recording");
    let scratch = Scratch::new("inputs");
    let root = scratch.0.join("root");
    inputs_fixture(&root, &recorded);
    let files = super::inputs::source_files(&root).expect("the inputs");
    assert_eq!(
        &relative(&files, &root),
        recorded.get("files").expect("files")
    );
    // The reference bound its own code too; nothing else is left out here.
    let reference = recorded
        .get("reference")
        .and_then(Document::items)
        .expect("its files");
    assert_eq!(reference.len() - files.len(), 6);
    let destination = scratch.0.join("context");
    fs::create_dir(&destination).expect("a context");
    let digest = super::inputs::inputs_digest(&root, Some(&destination)).expect("a digest");
    assert_eq!(digest, text(&recorded, "digest"));
    let context = recorded
        .get("context")
        .and_then(Document::fields)
        .expect("a context");
    assert_eq!(context.len(), files.len());
    for (name, mode) in context {
        let found = fs::metadata(destination.join(name))
            .expect("a copy")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            format!("0o{found:o}"),
            mode.text().expect("a mode"),
            "{name}"
        );
    }
}

#[test]
fn every_recorded_closure_refusal_is_alike() {
    let recorded = Document::read(INPUTS).expect("a recording");
    let worker = super::inputs::WORKER;
    let mut all = Vec::new();
    for case in recorded
        .get("refusals")
        .and_then(Document::items)
        .expect("refusals")
    {
        let scratch = Scratch::new("closure");
        let root = scratch.0.join("root");
        inputs_fixture(&root, &recorded);
        let base = root.join(worker);
        let name = text(case, "name");
        match name {
            "a missing source" => fs::remove_dir_all(base.join("fbs")).expect("removed"),
            "a source that is a link" => {
                symlink(root.join("elsewhere"), base.join("src/link")).expect("a link")
            }
            "a source directory that is a link" => {
                fs::remove_dir_all(base.join("fbs")).expect("removed");
                symlink(base.join("src"), base.join("fbs")).expect("a link");
            }
            "a source that is a pipe" => {
                let made = std::process::Command::new("mkfifo")
                    .arg(base.join("src/pipe"))
                    .status();
                assert!(made.expect("mkfifo").success());
            }
            "a link inside a generated tree" => {
                symlink(root.join("elsewhere"), base.join("include/FBS/link")).expect("a link");
            }
            "a source file that is a directory" => {
                fs::remove_file(base.join("tasks.py")).expect("removed");
                fs::create_dir(base.join("tasks.py")).expect("a directory");
            }
            "a named source that is a pipe" => {
                fs::remove_file(base.join("build.rs")).expect("removed");
                let made = std::process::Command::new("mkfifo")
                    .arg(base.join("build.rs"))
                    .status();
                assert!(made.expect("mkfifo").success());
            }
            "a generated part that is a link" => {
                fs::remove_dir_all(base.join("include/FBS")).expect("removed");
                symlink(root.join("elsewhere"), base.join("include/FBS")).expect("a link");
            }
            "a sibling of a generated part" => {
                let sibling = base.join("include/FBSextra.h");
                fs::write(sibling, "file include/FBSextra.h\n").expect("a file");
            }
            "one more source than the most" => {
                let many = base.join("src/many");
                fs::create_dir(&many).expect("a directory");
                for index in 0..whole(case, "count") {
                    fs::write(many.join(format!("{index:05}.txt")), b"").expect("a file");
                }
            }
            other => panic!("no edit for {other}"),
        }
        let came = super::inputs::source_files(&root).map(|files| relative(&files, &root));
        all.push((name.to_owned(), here(came), there(case)));
    }
    assert_eq!(tallied(all, &[]), [4, 6, 0]);
}

const CHECKS: &[u8] = include_bytes!("recorded_checks.json");

/// The cases of the part `name` of the recorded checks.
fn checks(name: &str) -> Vec<Document> {
    let recorded = Document::read(CHECKS).expect("a recording");
    let held = recorded.get(name).and_then(Document::items);
    held.unwrap_or_else(|| panic!("no part {name}")).to_vec()
}

/// The bytes that the hexadecimal text `name` of `case` spells.
fn spelled(case: &Document, name: &str) -> Vec<u8> {
    let spelled = text(case, name);
    (0..spelled.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&spelled[index..index + 2], 16).expect("hex"))
        .collect()
}

/// A scratch directory named as the reference resolves it.
fn resolved(scratch: &Scratch) -> std::path::PathBuf {
    scratch.0.canonicalize().expect("a scratch directory")
}

/// The binaries the reference failed on in its interpreter's words: a final
/// link it could not open, and a directory it could not read.
const UNREAD_BINARIES: [(&str, &str); 2] = [
    ("a link", "native_input_unreadable"),
    ("a directory", "compiled_binary_not_regular"),
];

#[test]
fn every_recorded_binary_is_identified_alike() {
    let mut all = Vec::new();
    for case in checks("binaries") {
        let scratch = Scratch::new("binary");
        let root = resolved(&scratch);
        let name = text(&case, "name");
        let path = root.join("binary");
        if case.get("link").is_some() {
            fs::write(root.join("target"), [0; 64]).expect("a target");
            symlink(root.join("target"), &path).expect("a link");
        } else if case.get("directory").is_some() {
            fs::create_dir(&path).expect("a directory");
        } else {
            fs::write(&path, spelled(&case, "hex")).expect("a binary");
        }
        let architecture =
            super::sandbox::native_architecture(text(&case, "machine")).expect("a machine");
        let came = super::compiled::binary_identity(&path, architecture)
            .map(|(size, digest)| Document::List(vec![size.into(), digest.as_str().into()]));
        all.push((name.to_owned(), here(came), there(&case)));
    }
    assert_eq!(tallied(all, &UNREAD_BINARIES), [3, 7, 2]);
}

/// A receipt the reference took and is refused here: a version that is the
/// truth value its interpreter counts as one.
const STRICTER_RECEIPTS: [(&str, &str); 1] = [(
    "a receipt of another schemaVersion True",
    "compiled_receipt_mismatch",
)];

#[test]
fn every_recorded_receipt_is_held_alike() {
    let image = format!("sha256:{}", "a".repeat(64));
    let (mut all, mut stricter) = (Vec::new(), 0);
    for case in checks("receipts") {
        let scratch = Scratch::new("receipt");
        let directory = resolved(&scratch).join("compiled");
        fs::create_dir(&directory).expect("a directory");
        fs::write(directory.join("binary"), spelled(&case, "hex")).expect("a binary");
        fs::write(directory.join("receipt.json"), text(&case, "receipt")).expect("a receipt");
        if let Some(extra) = case.get("extra").and_then(Document::text) {
            fs::write(directory.join(extra), "x").expect("a file");
        }
        let name = text(&case, "name");
        let came =
            super::compiled::compiled_receipt(&directory, "asan", &image, &"d".repeat(64), "amd64");
        let came = here(came.map(|receipt| receipt.sorted()));
        if let Some((_, code)) = STRICTER_RECEIPTS.iter().find(|(other, _)| *other == name) {
            assert_eq!(came, format!("refused {code}"), "{name}");
            assert!(there(&case).starts_with("held "), "{name}");
            stricter += 1;
            continue;
        }
        all.push((name.to_owned(), came, there(&case)));
    }
    let unread = [("a receipt that is no JSON", "native_receipt_unread")];
    assert_eq!((tallied(all, &unread), stricter), ([1, 12, 1], 1));
}

/// The control groups whose quota the reference failed to read as two
/// numbers in its interpreter's words.
const UNREAD_GROUPS: [(&str, &str); 2] = [
    ("a quota of one word", "codeql_resource_number"),
    ("a quota that is no number", "codeql_resource_number"),
];

#[test]
fn every_recorded_control_group_is_read_alike() {
    let mut all = Vec::new();
    for case in checks("cgroups") {
        let scratch = Scratch::new("cgroup");
        let root = resolved(&scratch).join("cgroup");
        fs::create_dir(&root).expect("a root");
        for (relative, content) in case.get("files").and_then(Document::fields).expect("files") {
            let path = root.join(relative);
            fs::create_dir_all(path.parent().expect("a parent")).expect("directories");
            fs::write(&path, format!("{}\n", content.text().expect("text"))).expect("a file");
        }
        let membership = resolved(&scratch).join("membership");
        if let Some(listed) = case.get("membership").and_then(Document::text) {
            fs::write(&membership, listed).expect("a membership");
        }
        let came =
            crate::budget::cgroup_limits(&root, &membership).map(|(mut cpus, mut memory)| {
                cpus.sort_unstable();
                memory.sort_unstable();
                let list = |values: Vec<u64>| {
                    Document::List(values.into_iter().map(Document::from).collect())
                };
                Document::List(vec![list(cpus), list(memory)])
            });
        all.push((text(&case, "name").to_owned(), here(came), there(&case)));
    }
    assert_eq!(tallied(all, &UNREAD_GROUPS), [14, 7, 2]);
}

/// An engine that answers from a script and keeps what it was asked.
pub(crate) struct Scripted {
    pub answers: std::collections::VecDeque<crate::Result<Vec<u8>>>,
    pub asked: Vec<Vec<String>>,
}

impl super::engine::Engine for Scripted {
    fn ask(
        &mut self,
        arguments: &[String],
        _within: std::time::Duration,
    ) -> crate::Result<Vec<u8>> {
        self.asked.push(arguments.to_vec());
        self.answers.pop_front().expect("an answer")
    }
    fn removing(
        &mut self,
        arguments: &[String],
        within: std::time::Duration,
    ) -> crate::Result<Vec<u8>> {
        self.ask(arguments, within)
    }
    fn run(
        &mut self,
        arguments: &[String],
        _input: &[u8],
        _within: std::time::Duration,
        _logs: (&Path, &Path),
        _room: (u64, u64),
    ) -> crate::Result<i32> {
        self.asked.push(arguments.to_vec());
        self.answers.pop_front().expect("an answer").map(|_| 0)
    }
}

/// An engine that answers `answers` in turn.
pub(crate) fn scripted(answers: Vec<Vec<u8>>) -> Scripted {
    Scripted {
        answers: answers.into_iter().map(Ok).collect(),
        asked: Vec::new(),
    }
}

#[test]
fn every_recorded_image_is_inspected_alike() {
    let mut all = Vec::new();
    for case in checks("images") {
        let info = case.get("info").cloned().unwrap_or(Document::Null);
        let mut engine = scripted(vec![info.compact().into_bytes()]);
        let image = text(&case, "image");
        let came = super::engine::checked_image(&mut engine, image, &"d".repeat(64), "amd64");
        let came = came.map(|info| {
            let mut names: Vec<&str> = info
                .fields()
                .expect("a record")
                .iter()
                .map(|(name, _)| name.as_str())
                .collect();
            names.sort_unstable();
            Document::List(names.into_iter().map(Document::from).collect())
        });
        if let Some(asked) = case.get("asked").and_then(Document::whole) {
            assert_eq!(engine.asked.len() as i128, asked, "{}", text(&case, "name"));
        }
        all.push((text(&case, "name").to_owned(), here(came), there(&case)));
    }
    let unread = [("an image without a configuration", "native_engine_unread")];
    assert_eq!(tallied(all, &unread), [2, 9, 1]);
}

#[test]
fn every_recorded_cleanup_asks_and_removes_alike() {
    let image = format!("sha256:{}", "a".repeat(64));
    let mut all = Vec::new();
    for case in checks("cleanups") {
        let mut answers = vec![text(&case, "listing").as_bytes().to_vec()];
        if let Some(info) = case.get("info").filter(|info| **info != Document::Null) {
            answers.push(info.compact().into_bytes());
            answers.push(Vec::new());
        }
        let mut engine = scripted(answers);
        let came =
            super::engine::cleanup(&mut engine, &"c".repeat(32), &image).map(|()| Document::Null);
        let name = text(&case, "name");
        let calls = case.get("calls").and_then(Document::items).expect("calls");
        let recorded: Vec<Vec<String>> = calls
            .iter()
            .map(|call| {
                let words = call.items().expect("a call");
                words[1..]
                    .iter()
                    .map(|word| word.text().expect("a word").to_owned())
                    .collect()
            })
            .collect();
        assert_eq!(engine.asked, recorded, "{name}");
        all.push((name.to_owned(), here(came), there(&case)));
    }
    assert_eq!(tallied(all, &[]), [3, 6, 0]);
}

// ---- the suites' operations, through an engine that answers as a sandbox
// ---- of the image would

const IMAGE: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const CONTAINER: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const RUN_ID: &str = "cccccccccccccccccccccccccccccccc";

/// A compiled executable of `machine`, of `size` bytes.
fn executable(machine: u16, size: usize) -> Vec<u8> {
    let mut data = vec![0; size];
    data[..6].copy_from_slice(b"\x7fELF\x02\x01");
    data[18..20].copy_from_slice(&machine.to_le_bytes());
    data
}

/// The packages the image lists as those it was built from.
const RPMS: &[u8] = b"glibc 0:2.43-1 x86_64\n";
/// The image's environment as `env -0` prints it: one of its values holds a
/// line end and what looks like another variable.
const AMBIENT: &str = concat!(
    "PATH=/opt/llvm/bin:/usr/bin:/bin\0",
    "CC=/opt/llvm/bin/clang\0",
    "NOTE=one\nCXX=/untrusted/c++\0",
    "FAKE_SECRET=canary\0",
    "MEDIASOUP_BUILD_JOBS=2\0",
);
/// How the sandbox's `stat` answers for a file it was not told otherwise
/// of: a regular file of `{}` bytes.
const STAT: &str = "--format=%f %s %.9Y";
/// The script that writes a new private file in the sandbox.
const WRITE: &str = "umask 077 && set -C && exec /usr/bin/cat > \"$1\"";
/// Where a kept executable is restored in a sandbox.
const RESTORED: &str = "/work/native-program";
/// Where the asan suite is built in a sandbox.
const BUILT: &str = "/work/build/mediasoup-worker-test-asan-address";
/// The most bytes of a phase's log, as the reference spelled them.
const MAX_LOG: u64 = 16 << 20;
/// The most bytes one step of a worker phase may print on either stream.
const STEP_LOG: u64 = MAX_LOG / 2;
/// One mebibyte.
const MIB: u64 = 1 << 20;

/// What a command of a script does instead of what the sandbox would do.
#[derive(Clone, Debug)]
pub(crate) enum Act {
    /// It ends with this status; a question that does not end with 0 is
    /// refused as the engine refuses one.
    Ends(i32),
    /// The engine stops it, for this code.
    Stopped(&'static str),
    /// It prints this, as the answer to a question.
    Prints(Vec<u8>),
    /// It says this many bytes on each of its two streams, as far as its
    /// room goes, and ends with 0 when it had room for all of it.
    Says(u64, u64),
    /// It lasts this long, as far as its bound goes, and then does what it
    /// would have done.
    Lasts(Duration),
    /// The run is cancelled while it runs.
    Cancels,
}

/// A command of a script: the words it holds, and what it does.
type Cue = (Vec<String>, Act);

/// The command of a script that holds `words` and does `act`.
fn on(words: &[&str], act: Act) -> Cue {
    (super::engine::words(words), act)
}

/// An engine that answers each command as the run's sandbox of `IMAGE`
/// would, unless its script says otherwise, and keeps what it was asked,
/// within which bounds, and every file written in the sandbox.
pub(crate) struct Sandboxed {
    /// Every command, in order.
    pub asked: Vec<Vec<String>>,
    /// The bound of each command, and the room of each that was run.
    pub bounds: Vec<(Duration, Option<(u64, u64)>)>,
    /// The logs of each command that was run.
    pub streams: Vec<Option<(PathBuf, PathBuf)>>,
    /// The host's architecture.
    pub architecture: &'static str,
    /// The inputs the image is labelled with.
    pub inputs: String,
    /// The sandbox the engine lists as the run's.
    pub listed: String,
    /// The executable a build leaves.
    pub built: Vec<u8>,
    /// What an export copies out instead of the built executable.
    pub exported: Option<Vec<u8>>,
    /// The files written in the sandbox, by path, in order.
    pub files: Vec<(String, Vec<u8>)>,
    /// What a command holding all of some words does instead; each is done
    /// once, the first that matches first.
    pub script: Vec<Cue>,
    /// The run's clock, which only commands move.
    pub clock: Rc<Cell<Duration>>,
    /// Whether the run was cancelled.
    pub cancelled: bool,
    /// A file of the host's that a command holding a word changes, and its
    /// new bytes.
    pub tampered: Option<(&'static str, PathBuf, Vec<u8>)>,
    /// A directory of the host's whose files are looked at as the sandbox
    /// is removed.
    pub watched: Option<PathBuf>,
    /// What those files were then, by path below it, with their modes.
    pub at_removal: Vec<(String, u32)>,
}

impl Sandboxed {
    fn new(architecture: &'static str, inputs: &str) -> Self {
        let machine = if architecture == "amd64" { 62 } else { 183 };
        Self {
            asked: Vec::new(),
            bounds: Vec::new(),
            streams: Vec::new(),
            architecture,
            inputs: inputs.to_owned(),
            listed: String::new(),
            built: executable(machine, 96),
            exported: None,
            files: Vec::new(),
            script: Vec::new(),
            clock: Rc::new(Cell::new(Duration::ZERO)),
            cancelled: false,
            tampered: None,
            watched: None,
            at_removal: Vec::new(),
        }
    }

    /// The bytes at `path` in the sandbox, where there are some.
    fn held(&self, path: &str) -> Option<Vec<u8>> {
        let written = self.files.iter().rev().find(|(held, _)| held == path);
        if let Some((_, data)) = written {
            return Some(data.clone());
        }
        if path == "/opt/check/builder-rpms.txt" {
            return Some(RPMS.to_vec());
        }
        let target = path.strip_prefix("/work/build/");
        let built = target.filter(|target| {
            super::sandbox::TARGETS
                .iter()
                .any(|(_, name)| name == target)
        });
        built.map(|_| self.built.clone())
    }

    /// What the script says the command `arguments` does, which it then
    /// says no more.
    fn act(&mut self, arguments: &[String]) -> Option<Act> {
        let held = |words: &Vec<String>| words.iter().all(|word| arguments.contains(word));
        let index = self.script.iter().position(|(words, _)| held(words))?;
        Some(self.script.remove(index).1)
    }

    /// Note the command `arguments`, change the tampered file if it holds
    /// the word, and answer whether the run was cancelled before it.
    fn note(&mut self, arguments: &[String], bounds: (Duration, Option<(u64, u64)>)) -> bool {
        self.asked.push(arguments.to_vec());
        self.bounds.push(bounds);
        self.streams.push(None);
        if let Some((word, path, data)) = &self.tampered
            && arguments.iter().any(|held| held == word)
        {
            fs::write(path, data).expect("a changed file");
        }
        self.cancelled
    }

    /// Let `lasting` pass on the run's clock.
    fn pass(&self, lasting: Duration) {
        self.clock.set(self.clock.get() + lasting);
    }

    /// Look at the watched directory's files as the sandbox is removed.
    fn removal(&mut self) {
        let Some(watched) = self.watched.clone() else {
            return;
        };
        let mut pending = vec![watched.clone()];
        while let Some(directory) = pending.pop() {
            for entry in fs::read_dir(&directory).expect("a directory").flatten() {
                let path = entry.path();
                let mode = fs::symlink_metadata(&path)
                    .expect("a file")
                    .permissions()
                    .mode();
                let local = path.strip_prefix(&watched).expect("below");
                self.at_removal
                    .push((local.to_string_lossy().into_owned(), mode & 0o7777));
                if path.is_dir() {
                    pending.push(path);
                }
            }
        }
        self.at_removal.sort();
    }

    /// What the sandbox, its engine or its image answers to `arguments`.
    fn answer(&mut self, arguments: &[String]) -> crate::Result<Vec<u8>> {
        let words: Vec<&str> = arguments.iter().map(String::as_str).collect();
        let held = |path: &str| self.held(path).ok_or(Fault("command_failed"));
        let answer = match words.as_slice() {
            ["image", "inspect", "--format", "{{json .}}", IMAGE] => {
                let labels = [(super::sandbox::INPUT_LABEL, self.inputs.as_str().into())];
                let info = Document::record([
                    ("Id", IMAGE.into()),
                    ("Os", "linux".into()),
                    ("Architecture", self.architecture.into()),
                    (
                        "Config",
                        Document::record([("Labels", Document::record(labels))]),
                    ),
                ]);
                info.compact().into_bytes()
            }
            ["create", ..] => format!("{CONTAINER}\n").into_bytes(),
            ["start", CONTAINER] | ["rm", "--force", CONTAINER] => Vec::new(),
            ["ps", ..] => self.listed.clone().into_bytes(),
            ["inspect", "--format", "{{json .}}", CONTAINER] => {
                let labels = [(super::sandbox::RUN_LABEL, RUN_ID.into())];
                let info = Document::record([
                    ("Id", CONTAINER.into()),
                    ("Image", IMAGE.into()),
                    (
                        "Config",
                        Document::record([("Labels", Document::record(labels))]),
                    ),
                ]);
                info.compact().into_bytes()
            }
            ["exec", CONTAINER, "/usr/bin/env", "-0"] => AMBIENT.as_bytes().to_vec(),
            ["exec", CONTAINER, "/usr/bin/stat", STAT, path] => {
                let size = held(path)?.len();
                format!("81ed {size} 1700000000.000000000\n").into_bytes()
            }
            ["exec", CONTAINER, "/usr/bin/stat", "--format=%s", path] => {
                format!("{}\n", held(path)?.len()).into_bytes()
            }
            ["exec", CONTAINER, "/usr/bin/head", bytes, path] => {
                let bytes = bytes.strip_prefix("--bytes=").expect("a count");
                let count: usize = bytes.parse().expect("a count");
                held(path)?.into_iter().take(count).collect()
            }
            ["exec", CONTAINER, "/usr/bin/sha256sum", path] => {
                format!("{}  {path}\n", crate::digest(&held(path)?)).into_bytes()
            }
            other => panic!("an unexpected question {other:?}"),
        };
        match words.as_slice() {
            ["create", ..] => self.listed = format!("{CONTAINER}\n"),
            ["rm", ..] => {
                self.removal();
                self.listed.clear();
            }
            _ => {}
        }
        Ok(answer)
    }
}

/// Append `count` bytes of `letter` to the log `path`.
fn said(path: &Path, count: u64, letter: u8) {
    use std::io::Write as _;
    let mut log = fs::OpenOptions::new()
        .append(true)
        .open(path)
        .expect("a log");
    let said = vec![letter; usize::try_from(count).expect("a size")];
    log.write_all(&said).expect("said");
}

impl super::engine::Engine for Sandboxed {
    fn ask(&mut self, arguments: &[String], within: Duration) -> crate::Result<Vec<u8>> {
        if self.note(arguments, (within, None)) {
            return Err(Fault("command_lease_lost"));
        }
        match self.act(arguments) {
            None | Some(Act::Ends(0)) => {}
            Some(Act::Ends(_)) => return Err(Fault("command_failed")),
            Some(Act::Stopped(code)) => return Err(Fault(code)),
            Some(Act::Prints(printed)) => return Ok(printed),
            Some(Act::Says(..)) => panic!("a question says nothing"),
            Some(Act::Lasts(lasting)) => {
                self.pass(lasting.min(within));
                if lasting > within {
                    return Err(Fault("command_timed_out"));
                }
            }
            Some(Act::Cancels) => {
                self.cancelled = true;
                return Err(Fault("command_lease_lost"));
            }
        }
        self.answer(arguments)
    }

    fn removing(&mut self, arguments: &[String], within: Duration) -> crate::Result<Vec<u8>> {
        self.note(arguments, (within, None));
        match self.act(arguments) {
            Some(Act::Prints(printed)) => Ok(printed),
            Some(Act::Stopped(code)) => Err(Fault(code)),
            None => self.answer(arguments),
            Some(other) => panic!("a removal does not {other:?}"),
        }
    }

    fn run(
        &mut self,
        arguments: &[String],
        input: &[u8],
        within: Duration,
        logs: (&Path, &Path),
        room: (u64, u64),
    ) -> crate::Result<i32> {
        let cancelled = self.note(arguments, (within, Some(room)));
        let kept = (logs.0.to_path_buf(), logs.1.to_path_buf());
        if let Some(streams) = self.streams.last_mut() {
            *streams = Some(kept);
        }
        if cancelled {
            return Err(Fault("command_lease_lost"));
        }
        match self.act(arguments) {
            None => {}
            Some(Act::Ends(status)) => return Ok(status),
            Some(Act::Stopped(code)) => return Err(Fault(code)),
            Some(Act::Prints(_)) => panic!("a command that is run prints into its logs"),
            Some(Act::Says(output, error)) => {
                said(logs.0, output.min(room.0), b'o');
                said(logs.1, error.min(room.1), b'e');
                if output > room.0 {
                    return Err(Fault("command_stdout_limit_exceeded"));
                }
                if error > room.1 {
                    return Err(Fault("command_stderr_limit_exceeded"));
                }
                return Ok(0);
            }
            Some(Act::Lasts(lasting)) => {
                self.pass(lasting.min(within));
                if lasting > within {
                    return Err(Fault("command_timed_out"));
                }
            }
            Some(Act::Cancels) => {
                self.cancelled = true;
                return Err(Fault("command_lease_lost"));
            }
        }
        let words: Vec<&str> = arguments.iter().map(String::as_str).collect();
        match words.as_slice() {
            ["build", .., "--iidfile", iid, _] if !Path::new(iid).exists() => {
                fs::write(iid, IMAGE).expect("an identity");
            }
            ["exec", CONTAINER, "/usr/bin/cat", path] => {
                let held = self.held(path).expect("a built executable");
                let data = self.exported.clone().unwrap_or(held);
                let room = usize::try_from(room.0).expect("a room");
                fs::write(logs.0, &data[..data.len().min(room)]).expect("an export");
                if data.len() > room {
                    return Err(Fault("command_stdout_limit_exceeded"));
                }
            }
            [
                "exec",
                "--interactive",
                CONTAINER,
                "/bin/sh",
                "-c",
                WRITE,
                "sh",
                path,
            ] => {
                if self.files.iter().any(|(held, _)| held == path) {
                    return Ok(2);
                }
                self.files.push(((*path).to_owned(), input.to_vec()));
            }
            _ => {}
        }
        Ok(0)
    }
}

impl super::engine::Engine for &mut Sandboxed {
    fn ask(&mut self, arguments: &[String], within: Duration) -> crate::Result<Vec<u8>> {
        (**self).ask(arguments, within)
    }

    fn removing(&mut self, arguments: &[String], within: Duration) -> crate::Result<Vec<u8>> {
        (**self).removing(arguments, within)
    }

    fn run(
        &mut self,
        arguments: &[String],
        input: &[u8],
        within: Duration,
        logs: (&Path, &Path),
        room: (u64, u64),
    ) -> crate::Result<i32> {
        (**self).run(arguments, input, within, logs, room)
    }
}

/// The checkout this crate was built in.
fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the checkout")
}

/// What a build may use in these tests.
const BUDGET: crate::budget::Budget = crate::budget::Budget {
    workers: 2,
    ram_mib: 6144,
    query_ram_mib: 14_000,
};
/// The code the reference's entry point gave its interpreter's errors.
const UNUSABLE: &str = "invalid_native_security_input";
/// The image's Meson.
const MESON: &str = "/opt/native-tools/pip_meson_ninja/bin/meson";

/// A number of seconds.
fn secs(seconds: u64) -> Duration {
    Duration::from_secs(seconds)
}

/// The budget of these tests, asked for.
fn budgeted() -> crate::Result<crate::budget::Budget> {
    Ok(BUDGET)
}

/// Whether `engine` was asked a command holding `word`.
fn asked(engine: &Sandboxed, word: &str) -> bool {
    engine
        .asked
        .iter()
        .any(|command| command.iter().any(|held| held == word))
}

/// Whether `engine` removed the run's sandbox, by its full identity.
fn removed(engine: &Sandboxed) -> bool {
    let removal = super::engine::words(&["rm", "--force", CONTAINER]);
    engine.asked.contains(&removal)
}

/// A request of `mode` in `IMAGE`.
fn request<'a>(mode: &'a str, compiled: Option<&'a Path>) -> super::run::Request<'a> {
    super::run::Request {
        mode,
        image: IMAGE,
        compiled,
    }
}

/// A checkout of the recorded inputs, the digest of the image's inputs
/// that it binds, and the scratch directory it is in.
struct Bench {
    scratch: Scratch,
    root: PathBuf,
    inputs: String,
}

impl Bench {
    fn new(name: &str) -> Self {
        let scratch = Scratch::new(name);
        let root = scratch.0.join("root");
        inputs_fixture(&root, &Document::read(INPUTS).expect("a recording"));
        // The recording's pin is no document; the image's toolchain is the
        // reviewed one.
        let pin = "security/native/toolchain.json";
        fs::write(
            root.join(pin),
            fs::read(repository().join(pin)).expect("a pin"),
        )
        .expect("a pin");
        let inputs = super::inputs::inputs_digest(&root, None).expect("the inputs");
        Self {
            scratch,
            root,
            inputs,
        }
    }

    /// The new directory `name` beside the checkout.
    fn directory(&self, name: &str) -> PathBuf {
        let path = self.scratch.0.join(name);
        fs::create_dir(&path).expect("a directory");
        path
    }

    /// An engine of the checkout's image on amd64.
    fn engine(&self) -> Sandboxed {
        Sandboxed::new("amd64", &self.inputs)
    }

    /// The reviewed corpus of the checkout.
    fn cases(&self) -> Vec<Case> {
        corpus(&self.root).expect("the corpus")
    }
}

/// What `operation` comes to on a host of the checkout `root` and `engine`,
/// on `architecture`, within what `budget` answers.
fn hosted<T>(
    root: &Path,
    engine: &mut Sandboxed,
    architecture: crate::Result<&'static str>,
    budget: &dyn Fn() -> crate::Result<crate::budget::Budget>,
    operation: impl FnOnce(&mut super::run::Host<'_>) -> T,
) -> T {
    let clock = Rc::clone(&engine.clock);
    let now = move || clock.get();
    let mut host = super::run::Host {
        root,
        engine,
        architecture,
        budget,
        run_id: RUN_ID.to_owned(),
        clock: &now,
    };
    operation(&mut host)
}

/// Run the suite `mode` of `bench`'s checkout through `engine`, with its
/// evidence in `output`, from the build kept in `compiled` where there is
/// one.
fn ran(
    bench: &Bench,
    engine: &mut Sandboxed,
    mode: &str,
    compiled: Option<&Path>,
    output: &Path,
) -> crate::Result<Document> {
    hosted(&bench.root, engine, Ok("amd64"), &budgeted, |host| {
        super::run::run(host, &request(mode, compiled), output)
    })
}

/// Compile the suite `mode` of `bench`'s checkout through `engine` into
/// `directory`, with its evidence in `output`.
fn compiling(
    bench: &Bench,
    engine: &mut Sandboxed,
    mode: &str,
    directory: &Path,
    output: &Path,
) -> crate::Result<Document> {
    hosted(&bench.root, engine, Ok("amd64"), &budgeted, |host| {
        super::run::compile(host, &request(mode, Some(directory)), output)
    })
}

/// The build of the suite `mode` kept in `bench`'s new directory `name`.
fn kept(bench: &Bench, mode: &str, name: &str) -> PathBuf {
    let directory = bench.scratch.0.join(name);
    let output = bench.directory(&format!("{name}-evidence"));
    let mut engine = bench.engine();
    compiling(bench, &mut engine, mode, &directory, &output).expect("a kept build");
    directory
}

/// Words as owned arguments.
fn words(words: &[&str]) -> Vec<String> {
    super::engine::words(words)
}

/// The command that runs `argv` in the sandbox.
fn exec(argv: &[&str]) -> Vec<String> {
    let mut command = words(&["exec", CONTAINER]);
    command.extend(words(argv));
    command
}

/// The command that runs `argv` in the sandbox with its standard input.
fn fed(argv: &[&str]) -> Vec<String> {
    let mut command = words(&["exec", "--interactive", CONTAINER]);
    command.extend(words(argv));
    command
}

/// The command that writes the new private file `path` in the sandbox.
fn writing(path: &str) -> Vec<String> {
    fed(&["/bin/sh", "-c", WRITE, "sh", path])
}

/// What a worker command runs with in the sandbox of `AMBIENT`, for the
/// parser family `family`: the image's own variables that are allowed,
/// none other, and the fixed settings.
fn environment(family: Option<&str>) -> Vec<String> {
    let mut environment = words(&[
        "/usr/bin/env",
        "-i",
        "PATH=/opt/llvm/bin:/usr/bin:/bin",
        "CC=/opt/llvm/bin/clang",
        "MEDIASOUP_BUILD_JOBS=2",
        "MESON_ARGS=--wrap-mode=nodownload",
        "NINJA=/opt/native-tools/pip_meson_ninja/bin/ninja",
        concat!(
            "ASAN_OPTIONS=halt_on_error=1:print_stacktrace=1:detect_leaks=1:symbolize=1:",
            "detect_stack_use_after_return=1:strict_init_order=1:",
            "check_initialization_order=1:detect_container_overflow=1"
        ),
        "UBSAN_OPTIONS=halt_on_error=1:print_stacktrace=1",
    ]);
    if let Some(family) = family {
        environment.push(format!("MS_FUZZ_{}=1", family.to_ascii_uppercase()));
    }
    environment
}

/// The command that runs `argv` in the worker's copy for `family`.
fn worked(argv: &[&str], family: Option<&str>) -> Vec<String> {
    let mut command = words(&["exec", "--workdir", "/work/worker", CONTAINER]);
    command.extend(environment(family));
    command.extend(words(argv));
    command
}

/// The image's inspection.
fn inspected() -> Vec<String> {
    words(&["image", "inspect", "--format", "{{json .}}", IMAGE])
}

/// The commands that inspect the image of `inputs` and create and start
/// the run's sandbox.
fn started(inputs: &str) -> Vec<Vec<String>> {
    let created = super::sandbox::sandbox_args(IMAGE, RUN_ID, inputs, BUDGET, "amd64");
    let mut created = created.expect("a sandbox");
    created.pop();
    created.extend(words(&["--entrypoint=/usr/bin/sleep", IMAGE, "2100"]));
    vec![inspected(), created, words(&["start", CONTAINER])]
}

/// The commands that remove the run's sandbox.
fn cleaned() -> Vec<Vec<String>> {
    let label = format!("label=org.simplestchat.native-security.run={RUN_ID}");
    vec![
        words(&[
            "ps",
            "--all",
            "--no-trunc",
            "--filter",
            &label,
            "--format",
            "{{.ID}}",
        ]),
        words(&["inspect", "--format", "{{json .}}", CONTAINER]),
        words(&["rm", "--force", CONTAINER]),
    ]
}

/// The commands that read the image's environment and restore the
/// worker's sources and its unit fixtures.
fn sourced() -> Vec<Vec<String>> {
    let mut commands = vec![
        exec(&["/usr/bin/env", "-0"]),
        exec(&[
            "/usr/bin/cp",
            "-R",
            "--preserve=mode,timestamps",
            "/opt/worker",
            "/work/worker",
        ]),
        exec(&["/usr/bin/mkdir", "/work/worker/test/data"]),
    ];
    for fixture in ["packet1.raw", "packet2.raw", "packet3.raw"] {
        commands.push(writing(&format!("/work/worker/test/data/{fixture}")));
    }
    commands
}

/// The commands of the build of the asan suite with two compilers.
fn phases() -> Vec<Vec<String>> {
    let target = "mediasoup-worker-test-asan-address";
    vec![
        worked(
            &[
                MESON,
                "setup",
                "--prefix",
                "/work/install",
                "--bindir",
                "",
                "--libdir",
                "",
                "--buildtype",
                "release",
                "-Db_ndebug=true",
                "--wrap-mode=nodownload",
                "-Dms_build_tests=true",
                "-Db_sanitize=address",
                "-Db_lundef=false",
                "/work/build",
            ],
            None,
        ),
        worked(
            &[
                MESON,
                "compile",
                "-C",
                "/work/build",
                "-j",
                "2",
                "flatbuffers-generator",
            ],
            None,
        ),
        worked(
            &[MESON, "compile", "-C", "/work/build", "-j", "2", target],
            None,
        ),
        worked(
            &[
                MESON,
                "install",
                "-C",
                "/work/build",
                "--no-rebuild",
                "--tags",
                target,
            ],
            None,
        ),
    ]
}

/// The commands that look at the executable `path` in the sandbox.
fn identified(path: &str) -> Vec<Vec<String>> {
    vec![
        exec(&["/usr/bin/stat", STAT, path]),
        exec(&["/usr/bin/head", "--bytes=20", path]),
        exec(&["/usr/bin/sha256sum", path]),
        exec(&["/usr/bin/stat", STAT, path]),
    ]
}

/// The commands that read the image's list of packages.
fn catalogued() -> Vec<Vec<String>> {
    let rpms = "/opt/check/builder-rpms.txt";
    vec![
        exec(&["/usr/bin/stat", STAT, rpms]),
        exec(&["/usr/bin/head", "--bytes=1048577", rpms]),
    ]
}

/// What the phase `phase` complained of in `output`.
fn complaints(output: &Path, phase: &str) -> Vec<u8> {
    fs::read(output.join(format!("{phase}.stderr.log"))).expect("a log")
}

/// The line the reference's worker printed for its failure `code`.
fn failure(code: &str) -> String {
    format!("Native security check failed: {code}\n")
}

/// Whether `log` is what a fake step complained of, `e` after `e`, and
/// then the worker's failure `code`.
fn logged(log: &[u8], code: &str) -> bool {
    let line = failure(code);
    log.ends_with(line.as_bytes())
        && log[..log.len() - line.len()]
            .iter()
            .all(|byte| *byte == b'e')
}

/// Where `engine` began to remove the run's sandbox, which must be the
/// last it was asked.
fn cleanup(engine: &Sandboxed) -> usize {
    let start = engine.asked.len().saturating_sub(3);
    assert_eq!(engine.asked[start..], cleaned()[..], "a cleanup last");
    start
}

/// Whether the command `command` holds every one of `words`.
fn holds(command: &[String], words: &[String]) -> bool {
    words.iter().all(|word| command.contains(word))
}

/// The names below `directory` that begin with `prefix`.
fn begun(directory: &Path, prefix: &str) -> Vec<String> {
    let entries = fs::read_dir(directory).expect("a directory").flatten();
    let names = entries.map(|entry| entry.file_name().to_string_lossy().into_owned());
    names.filter(|name| name.starts_with(prefix)).collect()
}

/// The script by which the built executable is looked at as `stated`,
/// before it is read and after.
fn stated_twice(stated: &str) -> Vec<Cue> {
    let stated = Act::Prints(stated.as_bytes().to_vec());
    vec![
        on(&[STAT, BUILT], stated.clone()),
        on(&[STAT, BUILT], stated),
    ]
}

/// A number of seconds as the reference rounded and wrote one.
fn timed(seconds: f64) -> Document {
    Document::Number(serde_json::Number::from_f64(seconds).expect("a number"))
}

#[test]
fn preparation_builds_for_the_real_architecture_with_the_pinned_toolchain() {
    let bench = Bench::new("prepare");
    let pin = fs::read(bench.root.join("security/native/toolchain.json")).expect("a pin");
    let pin = Document::read(&pin).expect("a pin");
    for (architecture, platform, other) in [
        ("amd64", "linux-x86_64", "arm64"),
        ("arm64", "linux-aarch64", "amd64"),
    ] {
        let output = bench.directory(&format!("prepared-{architecture}"));
        let mut engine = Sandboxed::new(architecture, &bench.inputs);
        let prepared = hosted(
            &bench.root,
            &mut engine,
            Ok(architecture),
            &budgeted,
            |host| super::run::prepare(host, &output),
        );
        let wanted = Document::record([
            ("schemaVersion", 1_u64.into()),
            ("status", "passed".into()),
            ("imageId", IMAGE.into()),
            ("inputsSha256", bench.inputs.as_str().into()),
            ("architecture", architecture.into()),
        ]);
        assert_eq!(prepared, Ok(wanted), "{architecture}");
        let asset = pin
            .get("platforms")
            .and_then(|platforms| platforms.get(platform))
            .expect("an asset");
        let build = &engine.asked[0];
        let context = build[build.len() - 1].clone();
        let mut wanted = words(&[
            "build",
            &format!("--platform=linux/{architecture}"),
            "--file",
            &format!("{context}/build/native-security.Dockerfile"),
            "--label",
            &format!("org.simplestchat.native-security.inputs={}", bench.inputs),
        ]);
        for (name, field) in [
            ("LLVM_URL", "url"),
            ("LLVM_SHA256", "sha256"),
            ("LLVM_BYTES", "bytes"),
            ("LLVM_DIRECTORY", "directory"),
        ] {
            let value = asset.get(field).expect("a field");
            let value = value.text().map_or_else(|| value.compact(), str::to_owned);
            wanted.extend(["--build-arg".to_owned(), format!("{name}={value}")]);
        }
        let iid = output.join("image.id");
        wanted.extend(words(&["--iidfile", &iid.to_string_lossy(), &context]));
        assert_eq!(engine.asked, [build.clone(), inspected()], "{architecture}");
        assert_eq!(*build, wanted, "{architecture}");
        let temporary = std::env::temp_dir().join("native-security-context-");
        assert!(
            context.starts_with(&*temporary.to_string_lossy()),
            "{context}"
        );
        assert!(!Path::new(&context).exists(), "{architecture}");
        assert_eq!(engine.bounds[0], (secs(2400), Some((MAX_LOG, MAX_LOG))));
        let logs = (
            output.join("prepare.stdout.log"),
            output.join("prepare.stderr.log"),
        );
        assert_eq!(engine.streams[0], Some(logs));
        let output = bench.directory(&format!("other-{architecture}"));
        let mut engine = Sandboxed::new(other, &bench.inputs);
        let refused = hosted(
            &bench.root,
            &mut engine,
            Ok(architecture),
            &budgeted,
            |host| super::run::prepare(host, &output),
        );
        let refused = refused.err();
        assert_eq!(refused, Some(Fault("native_image_architecture_mismatch")));
    }
    // A failed build, and identities written in other ways.
    let spaced = format!("\x1c\x0b {IMAGE}\t\r\n\x1f").into_bytes();
    let accented = format!("{IMAGE}\u{e9}").into_bytes();
    for (index, (script, written, code)) in [
        (
            vec![on(&["build"], Act::Ends(1))],
            None,
            Some("builder_failed"),
        ),
        (vec![], Some(spaced), None),
        (vec![], Some(accented), Some(UNUSABLE)),
        (
            vec![],
            Some(format!("{IMAGE} x").into_bytes()),
            Some("invalid_engine_image_id"),
        ),
        // The reference read at most eighty bytes of it.
        (
            vec![],
            Some(format!("{IMAGE}{}", "\n".repeat(9)).into_bytes()),
            None,
        ),
        (
            vec![],
            Some(format!("{IMAGE}{}", "\n".repeat(10)).into_bytes()),
            Some("input_size_exceeded"),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let output = bench.directory(&format!("identity-{index}"));
        let mut engine = bench.engine();
        engine.script = script;
        engine.tampered = written.map(|written| ("build", output.join("image.id"), written));
        let prepared = hosted(&bench.root, &mut engine, Ok("amd64"), &budgeted, |host| {
            super::run::prepare(host, &output)
        });
        match code {
            Some(code) => assert_eq!(prepared.err(), Some(Fault(code)), "{index}"),
            None => assert_eq!(text(&prepared.expect("prepared"), "imageId"), IMAGE),
        }
    }
    // A log that is there already, as the reference's interpreter refused
    // one, and a pin that is no document; the engine is asked nothing.
    let output = bench.directory("logged");
    fs::write(output.join("prepare.stderr.log"), b"").expect("a log");
    let mut engine = bench.engine();
    let prepared = hosted(&bench.root, &mut engine, Ok("amd64"), &budgeted, |host| {
        super::run::prepare(host, &output)
    });
    assert_eq!(prepared.err(), Some(Fault(UNUSABLE)));
    assert!(engine.asked.is_empty());
    let output = bench.directory("unpinned");
    fs::write(bench.root.join("security/native/toolchain.json"), "{}").expect("a pin");
    let prepared = hosted(&bench.root, &mut engine, Ok("amd64"), &budgeted, |host| {
        super::run::prepare(host, &output)
    });
    assert_eq!(prepared.err(), Some(Fault("native_toolchain_unread")));
    assert!(engine.asked.is_empty());
}

#[test]
fn a_sandbox_is_created_and_its_ownership_recorded_as_the_reference_did() {
    let bench = Bench::new("owned");
    let output = bench.directory("owned");
    let mut engine = bench.engine();
    ran(&bench, &mut engine, "asan", None, &output).expect("a passing suite");
    let ownership = fs::read_to_string(output.join("ownership.json")).expect("its ownership");
    let wanted = format!(
        concat!(
            "{{\n  \"imageId\": \"{}\",\n  \"inputsSha256\": \"{}\",\n",
            "  \"runId\": \"{}\",\n  \"schemaVersion\": 1\n}}\n"
        ),
        IMAGE, bench.inputs, RUN_ID
    );
    assert_eq!(ownership, wanted);
    // A sandbox's identity is read as the reference read it: ASCII, with
    // the whitespace around it dropped, and then sixty-four small
    // hexadecimal digits.
    for (index, (printed, code)) in [
        (format!("\x0b {CONTAINER}\x1c\n"), None),
        (format!("{CONTAINER}\u{e9}\n"), Some(UNUSABLE)),
        (format!("{}\n", &CONTAINER[1..]), Some("container_identity")),
        (
            format!("{}\n", CONTAINER.to_uppercase()),
            Some("container_identity"),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let output = bench.directory(&format!("created-{index}"));
        let mut engine = bench.engine();
        engine.listed = format!("{CONTAINER}\n");
        engine.script = vec![on(&["create"], Act::Prints(printed.into_bytes()))];
        let created = ran(&bench, &mut engine, "asan", None, &output);
        match code {
            None => assert!(created.is_ok(), "{index}"),
            Some(code) => {
                assert_eq!(created.err(), Some(Fault(code)), "{index}");
                let mut wanted = started(&bench.inputs);
                wanted.pop();
                wanted.extend(cleaned());
                assert_eq!(engine.asked, wanted, "{index}");
            }
        }
    }
    // A phase's log that is there already fails as the reference's did.
    let output = bench.directory("logged");
    fs::write(output.join("native.stdout.log"), b"").expect("a log");
    let mut engine = bench.engine();
    let logged = ran(&bench, &mut engine, "asan", None, &output);
    assert_eq!(logged.err(), Some(Fault(UNUSABLE)));
    let mut wanted = started(&bench.inputs);
    wanted.extend(cleaned());
    assert_eq!(engine.asked, wanted);
    // A build is asked for a directory to be kept in.
    let mut engine = bench.engine();
    let unkept = hosted(&bench.root, &mut engine, Ok("amd64"), &budgeted, |host| {
        super::run::compile(host, &request("asan", None), &output)
    });
    assert_eq!(unkept.err(), Some(Fault("compiled_directory_required")));
    assert!(engine.asked.is_empty());
}

#[test]
fn a_failing_worker_step_is_the_runs_exit_status_one_with_its_code_logged() {
    let bench = Bench::new("failing");
    let cases = bench.cases();
    let first = format!("/work/corpus/{}/{}", cases[0].family, cases[0].identifier);
    let family = format!("/work/corpus/{}", cases[cases.len() - 1].family);
    let rpms = "/opt/check/builder-rpms.txt";
    let unit = "/work/worker/test/data/packet2.raw";
    let stated = |text: &str| Act::Prints(text.as_bytes().to_vec());
    let table: Vec<(&str, Cue, &str)> = vec![
        (
            "replay",
            on(&["-seed=1"], Act::Ends(77)),
            "worker_command_failed",
        ),
        (
            "replay",
            on(&["setup"], Act::Ends(1)),
            "worker_command_failed",
        ),
        (
            "asan",
            on(&["--invisibles"], Act::Ends(1)),
            "worker_command_failed",
        ),
        (
            "ubsan",
            on(&["--no-rebuild"], Act::Ends(2)),
            "worker_command_failed",
        ),
        (
            "replay",
            on(&["-seed=1"], Act::Lasts(secs(11))),
            "command_timed_out",
        ),
        (
            "asan",
            on(&["--invisibles"], Act::Lasts(secs(1201))),
            "command_timed_out",
        ),
        (
            "asan",
            on(&["setup"], Act::Lasts(secs(1201))),
            "command_timed_out",
        ),
        (
            "replay",
            on(&["-seed=1"], Act::Says(STEP_LOG + 1, 0)),
            "command_stdout_limit_exceeded",
        ),
        (
            "replay",
            on(&["-seed=1"], Act::Says(0, STEP_LOG + 1)),
            "command_stderr_limit_exceeded",
        ),
        ("asan", on(&["/usr/bin/env", "-0"], Act::Ends(1)), UNUSABLE),
        (
            "asan",
            on(&["/usr/bin/env", "-0"], Act::Prints(vec![0xff])),
            UNUSABLE,
        ),
        (
            "asan",
            on(&["-0"], Act::Stopped("command_stdout_limit_exceeded")),
            "command_stdout_limit_exceeded",
        ),
        (
            "asan",
            on(&["-0"], Act::Stopped("command_stderr_limit_exceeded")),
            "command_stderr_limit_exceeded",
        ),
        (
            "asan",
            on(&["/usr/bin/cp"], Act::Ends(1)),
            "worker_sandbox_required",
        ),
        (
            "asan",
            on(&["/work/worker/test/data"], Act::Ends(1)),
            UNUSABLE,
        ),
        ("asan", on(&[unit], Act::Ends(2)), UNUSABLE),
        ("replay", on(&["/work/corpus"], Act::Ends(1)), UNUSABLE),
        ("replay", on(&["-p", &family], Act::Ends(1)), UNUSABLE),
        ("replay", on(&[&first], Act::Ends(1)), UNUSABLE),
        ("asan", on(&[STAT, rpms], stated("a1ff 22 1.0\n")), UNUSABLE),
        (
            "asan",
            on(&[STAT, rpms], stated("41ed 22 1.0\n")),
            "input_not_regular",
        ),
        (
            "asan",
            on(&[STAT, rpms], stated("81a4 1048577 1.0\n")),
            "input_size_exceeded",
        ),
        ("asan", on(&[STAT, rpms], Act::Ends(1)), UNUSABLE),
        ("asan", on(&[STAT, rpms], stated("81a4 22\n")), UNUSABLE),
        ("asan", on(&[STAT, rpms], stated("81a4 x 1.0\n")), UNUSABLE),
        ("asan", on(&[STAT, rpms], stated("q1a4 22 1.0\n")), UNUSABLE),
        (
            "asan",
            on(&["/usr/bin/head", rpms], stated("short")),
            "input_changed",
        ),
        ("asan", on(&["/usr/bin/head", rpms], Act::Ends(1)), UNUSABLE),
    ];
    for (index, (mode, script, code)) in table.into_iter().enumerate() {
        let name = format!("{index} {mode} {script:?}");
        let output = bench.directory(&format!("failing-{index}"));
        let mut engine = bench.engine();
        engine.script = vec![script.clone()];
        let ran = ran(&bench, &mut engine, mode, None, &output);
        assert_eq!(ran.err(), Some(Fault("native_failed_exit_1")), "{name}");
        assert!(logged(&complaints(&output, "native"), code), "{name}");
        assert!(engine.script.is_empty(), "{name}");
        let start = cleanup(&engine);
        assert!(holds(&engine.asked[start - 1], &script.0), "{name}");
        assert!(output.join("ownership.json").is_file(), "{name}");
    }
}

#[test]
fn a_failing_build_step_is_the_builds_exit_status_one_and_publishes_nothing() {
    let bench = Bench::new("unbuilt");
    let stated = |text: &str| Act::Prints(text.as_bytes().to_vec());
    let mut narrow = executable(62, 20);
    narrow[4] = 1;
    let mut big = executable(62, 20);
    big[5] = 2;
    let (status, header) = (STAT, "/usr/bin/head");
    let summed = "/usr/bin/sha256sum";
    let table: Vec<(Vec<Cue>, u64, &str)> = vec![
        (
            vec![on(&["setup"], Act::Ends(1))],
            2,
            "worker_command_failed",
        ),
        (
            vec![on(&["setup"], Act::Ends(77))],
            2,
            "worker_command_failed",
        ),
        (
            vec![on(&["flatbuffers-generator"], Act::Ends(1))],
            2,
            "worker_command_failed",
        ),
        (
            vec![on(&["--no-rebuild"], Act::Ends(1))],
            2,
            "worker_command_failed",
        ),
        (
            vec![on(&["flatbuffers-generator"], Act::Says(STEP_LOG + 1, 0))],
            2,
            "command_stdout_limit_exceeded",
        ),
        (
            vec![on(&[status, BUILT], stated("41ed 96 1.0\n"))],
            2,
            "compiled_binary_not_regular",
        ),
        (
            vec![on(&[status, BUILT], stated("a1ff 96 1.0\n"))],
            2,
            UNUSABLE,
        ),
        (
            vec![on(&[status, BUILT], stated("81ed 63 1.0\n"))],
            2,
            "compiled_binary_size",
        ),
        (
            vec![on(&[status, BUILT], stated("81ed 536870913 1.0\n"))],
            2,
            "compiled_binary_size",
        ),
        (
            vec![on(&[status, BUILT], stated("81ed ninety 1.0\n"))],
            2,
            UNUSABLE,
        ),
        (vec![on(&[status, BUILT], stated("81ed 96\n"))], 2, UNUSABLE),
        (vec![on(&[status, BUILT], Act::Ends(1))], 2, UNUSABLE),
        (
            vec![on(&[header, BUILT], Act::Prints(executable(183, 20)))],
            2,
            "compiled_binary_architecture",
        ),
        (
            vec![on(&[header, BUILT], Act::Prints(narrow))],
            2,
            "compiled_binary_architecture",
        ),
        (
            vec![on(&[header, BUILT], Act::Prints(big))],
            2,
            "compiled_binary_architecture",
        ),
        (
            vec![on(
                &[header, BUILT],
                Act::Prints(executable(62, 20)[..19].to_vec()),
            )],
            2,
            "compiled_binary_architecture",
        ),
        (vec![on(&[header, BUILT], Act::Ends(1))], 2, UNUSABLE),
        (
            vec![on(&[summed, BUILT], stated("xyz  file\n"))],
            2,
            UNUSABLE,
        ),
        (
            vec![on(&[summed, BUILT], stated(&"A".repeat(64)))],
            2,
            UNUSABLE,
        ),
        (vec![on(&[summed, BUILT], Act::Ends(1))], 2, UNUSABLE),
        (
            vec![
                on(&[status, BUILT], stated("81ed 96 1.0\n")),
                on(&[status, BUILT], stated("81ed 96 2.0\n")),
            ],
            2,
            "compiled_binary_changed",
        ),
        (
            vec![
                on(&[status, BUILT], stated("81ed 96 1.0\n")),
                on(&[status, BUILT], stated("81ed 97 1.0\n")),
            ],
            2,
            "compiled_binary_changed",
        ),
        (vec![], 5, "codeql_build_jobs_range"),
        (vec![], 0, "codeql_insufficient_resources"),
    ];
    for (index, (script, workers, code)) in table.into_iter().enumerate() {
        let name = format!("{index} {workers} {script:?}");
        let output = bench.directory(&format!("unbuilt-{index}"));
        let directory = bench.scratch.0.join(format!("compiled-{index}"));
        let mut engine = bench.engine();
        engine.script.clone_from(&script);
        let budget = crate::budget::Budget { workers, ..BUDGET };
        let budget = move || Ok(budget);
        let compiled = hosted(&bench.root, &mut engine, Ok("amd64"), &budget, |host| {
            super::run::compile(host, &request("asan", Some(&directory)), &output)
        });
        let compiled = compiled.err();
        assert_eq!(
            compiled,
            Some(Fault("native_build_failed_exit_1")),
            "{name}"
        );
        assert!(logged(&complaints(&output, "compile"), code), "{name}");
        assert!(engine.script.is_empty(), "{name}");
        let start = cleanup(&engine);
        if let Some((words, _)) = script.last() {
            assert!(holds(&engine.asked[start - 1], words), "{name}");
        }
        assert!(!asked(&engine, "/usr/bin/cat"), "{name}");
        assert!(directory.symlink_metadata().is_err(), "{name}");
        assert!(
            begun(&bench.scratch.0, "native-compiled").is_empty(),
            "{name}"
        );
    }
}

#[test]
fn a_fault_of_the_hosts_own_in_a_phase_is_the_runs_and_logs_nothing() {
    let bench = Bench::new("host-fault");
    for (index, script, code) in [
        (
            0,
            on(&["setup"], Act::Stopped("tool_entry_unavailable")),
            "tool_entry_unavailable",
        ),
        (
            1,
            on(&["-seed=1"], Act::Stopped("native_log_unwritable")),
            "native_log_unwritable",
        ),
        (
            2,
            on(&["-0"], Act::Stopped("tool_entry_missing")),
            "tool_entry_missing",
        ),
        (3, on(&["-seed=1"], Act::Cancels), "command_lease_lost"),
        (4, on(&["/usr/bin/cp"], Act::Cancels), "command_lease_lost"),
    ] {
        let output = bench.directory(&format!("host-{index}"));
        let mut engine = bench.engine();
        engine.script = vec![script.clone()];
        let ran = ran(&bench, &mut engine, "replay", None, &output);
        assert_eq!(ran.err(), Some(Fault(code)), "{index}");
        assert_eq!(complaints(&output, "native"), b"", "{index}");
        let start = cleanup(&engine);
        assert!(holds(&engine.asked[start - 1], &script.0), "{index}");
        // The removal was answered, a cancelled run's too: no sandbox is left.
        assert_eq!(engine.listed, "", "{index}");
        let replays = engine
            .asked
            .iter()
            .filter(|command| command.contains(&"-seed=1".to_owned()));
        assert!(replays.count() <= 1, "{index}");
    }
}

#[test]
fn every_suite_runs_every_test_or_reviewed_input_once() {
    let bench = Bench::new("suites");
    let cases = bench.cases();
    for mode in super::MODES {
        let output = bench.directory(mode);
        let mut engine = bench.engine();
        let ran = ran(&bench, &mut engine, mode, None, &output).expect("a passing suite");
        let target = super::sandbox::target(mode).expect("a target");
        let built = format!("/work/build/{target}");
        let mut wanted = started(&bench.inputs);
        wanted.extend(sourced());
        let commands = super::sandbox::compile_commands(mode, 2).expect("a build");
        for (_, argv) in &commands {
            let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
            wanted.push(worked(&argv, None));
        }
        wanted.extend(identified(&built));
        let mut completed = Vec::new();
        if mode == "replay" {
            wanted.push(exec(&["/usr/bin/mkdir", "/work/corpus"]));
            for case in &cases {
                let directory = format!("/work/corpus/{}", case.family);
                wanted.push(exec(&["/usr/bin/mkdir", "-p", &directory]));
                wanted.push(writing(&format!("{directory}/{}", case.identifier)));
            }
            for case in &cases {
                let path = format!("/work/corpus/{}/{}", case.family, case.identifier);
                let argv = super::sandbox::replay_args(&built, &path);
                let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
                wanted.push(worked(&argv, Some(&case.family)));
                completed.push(Document::from(case.identifier.as_str()));
            }
        } else {
            wanted.push(worked(&[&built, "--invisibles"], None));
            completed.push(mode.into());
        }
        wanted.extend(catalogued());
        wanted.extend(cleaned());
        assert_eq!(engine.asked, wanted, "{mode}");
        let worker = ran.get("worker").expect("the worker's record");
        assert_eq!(worker.get("completed"), Some(&Document::List(completed)));
        assert_eq!(ran.get("compiledReused"), Some(&Document::Bool(false)));
        // A test runs within its own bound, and every step in its own room.
        let own = if mode == "replay" {
            secs(10)
        } else {
            secs(1200)
        };
        let room = Some((STEP_LOG, STEP_LOG));
        let commands = engine.asked.iter().zip(&engine.bounds);
        let worked = commands.filter(|(command, _)| command.contains(&"--workdir".to_owned()));
        for (index, (command, bound)) in worked.enumerate() {
            let own = if index < 4 { secs(1200) } else { own };
            assert_eq!(*bound, (own, room), "{command:?}");
        }
        // A step the reference's worker took in its own process has only
        // what is left of the run's bound.
        for (command, bound) in engine.asked.iter().zip(&engine.bounds) {
            if command[0] == "exec" && !command.contains(&"--workdir".to_owned()) {
                assert_eq!(bound.0, secs(2100), "{command:?}");
                assert!(bound.1.is_none() || bound.1 == room, "{command:?}");
            }
        }
        // Every reviewed byte reaches the sandbox: the unit fixtures with a
        // line end, which their reader leaves out.
        for case in &cases {
            let fixture = super::corpus::UNIT_FIXTURES
                .iter()
                .find(|(name, _)| *name == case.identifier);
            if let Some((_, fixture)) = fixture {
                let path = format!("/work/worker/test/data/{fixture}");
                let mut data = case.data.clone();
                data.push(b'\n');
                assert_eq!(engine.held(&path), Some(data), "{path}");
            }
            if mode == "replay" {
                let path = format!("/work/corpus/{}/{}", case.family, case.identifier);
                assert_eq!(engine.held(&path), Some(case.data.clone()), "{path}");
            }
        }
    }
}

#[test]
fn a_runs_record_holds_the_references_fields() {
    let bench = Bench::new("record");
    let output = bench.directory("record");
    let mut engine = bench.engine();
    let rpms = "/opt/check/builder-rpms.txt";
    engine.script = vec![
        on(&["setup"], Act::Lasts(Duration::from_micros(1_062_500))),
        on(&["--invisibles"], Act::Lasts(Duration::from_millis(1500))),
        on(&["/usr/bin/head", rpms], Act::Lasts(secs(5))),
    ];
    let answered = ran(&bench, &mut engine, "asan", None, &output).expect("a passing suite");
    let build = Document::record([
        ("status", "built".into()),
        ("mode", "asan".into()),
        ("workers", 2_u64.into()),
        (
            "phaseSeconds",
            Document::record([
                ("configure", timed(1.062)),
                ("generator", timed(0.0)),
                ("compile", timed(0.0)),
                ("install", timed(0.0)),
            ]),
        ),
        ("binaryBytes", 96_u64.into()),
        (
            "binarySha256",
            crate::digest(&executable(62, 96)).as_str().into(),
        ),
    ]);
    let worker = Document::record([
        ("status", "passed".into()),
        ("mode", "asan".into()),
        ("completed", Document::List(vec!["asan".into()])),
        ("elapsedSeconds", timed(2.562)),
        ("testSeconds", timed(1.5)),
        ("build", build),
        ("llvmVersion", "23.1.2".into()),
        ("opensslInstrumented", false.into()),
        ("builderRpmSha256", crate::digest(RPMS).as_str().into()),
    ]);
    let wanted = Document::record([
        ("schemaVersion", 1_u64.into()),
        ("status", "passed".into()),
        ("mode", "asan".into()),
        ("runId", RUN_ID.into()),
        ("imageId", IMAGE.into()),
        ("inputsSha256", bench.inputs.as_str().into()),
        ("architecture", "amd64".into()),
        ("compiledReused", false.into()),
        ("worker", worker),
        ("cleanup", "verified".into()),
    ]);
    assert_eq!(answered, wanted);
    // A list of packages of exactly the most bytes is read whole.
    let output = bench.directory("whole");
    let mut engine = bench.engine();
    let listed = vec![b'x'; usize::try_from(MIB).expect("a size")];
    engine.script = vec![
        on(&[STAT, rpms], Act::Prints(b"81a4 1048576 1.0\n".to_vec())),
        on(&["/usr/bin/head", rpms], Act::Prints(listed.clone())),
    ];
    let ran = ran(&bench, &mut engine, "ubsan", None, &output).expect("a passing suite");
    let worker = ran.get("worker").expect("the worker's record");
    let digest = worker.get("builderRpmSha256").and_then(Document::text);
    assert_eq!(digest, Some(crate::digest(&listed).as_str()));
}

#[test]
fn a_build_is_published_only_once_proved_and_its_sandbox_removed() {
    let bench = Bench::new("compile");
    let output = bench.directory("built");
    let parent = bench.directory("parent");
    let directory = parent.join("compiled");
    let mut engine = bench.engine();
    engine.watched = Some(parent.clone());
    let target = "mediasoup-worker-test-asan-address";
    engine.script = vec![
        on(&["setup"], Act::Lasts(Duration::from_micros(1_062_500))),
        on(&["flatbuffers-generator"], Act::Lasts(secs(2))),
        on(
            &["compile", target],
            Act::Lasts(Duration::from_millis(3500)),
        ),
    ];
    let built = compiling(&bench, &mut engine, "asan", &directory, &output);
    let summed = crate::digest(&executable(62, 96));
    let worker = Document::record([
        ("status", "built".into()),
        ("mode", "asan".into()),
        ("workers", 2_u64.into()),
        (
            "phaseSeconds",
            Document::record([
                ("configure", timed(1.062)),
                ("generator", timed(2.0)),
                ("compile", timed(3.5)),
                ("install", timed(0.0)),
            ]),
        ),
        ("binaryBytes", 96_u64.into()),
        ("binarySha256", summed.as_str().into()),
    ]);
    let mut receipt = Document::record([
        ("schemaVersion", 1_u64.into()),
        ("status", "built".into()),
        ("mode", "asan".into()),
        ("imageId", IMAGE.into()),
        ("inputsSha256", bench.inputs.as_str().into()),
        ("architecture", "amd64".into()),
        ("binaryBytes", 96_u64.into()),
        ("binarySha256", summed.as_str().into()),
        ("worker", worker),
    ]);
    let kept = fs::read_to_string(directory.join("receipt.json")).expect("a receipt");
    assert_eq!(
        kept,
        format!("{}\n", receipt.clone().sorted().pretty().trim_end())
    );
    receipt.put("cleanup", "verified".into());
    assert_eq!(built, Ok(receipt));
    // The build's commands, in their order: the export once the built
    // executable is proved, and the removal last.
    let mut wanted = started(&bench.inputs);
    wanted.extend(sourced());
    wanted.extend(phases());
    wanted.extend(identified(BUILT));
    wanted.push(exec(&["/usr/bin/cat", BUILT]));
    wanted.extend(cleaned());
    assert_eq!(engine.asked, wanted);
    let export = engine.bounds[engine.asked.len() - 4];
    assert_eq!(export, (secs(120), Some((512 * MIB, MIB))));
    let streams = engine.streams[engine.asked.len() - 4].clone();
    let (copy, dropped) = streams.expect("the export's logs");
    assert!(copy.starts_with(&parent) && copy.ends_with("artifact/binary"));
    assert_eq!(dropped, Path::new("/dev/null"));
    let logs = (
        output.join("compile.stdout.log"),
        output.join("compile.stderr.log"),
    );
    let phase = engine.streams[..engine.asked.len() - 4].iter().flatten();
    assert!(phase.clone().count() > 4);
    assert!(phase.into_iter().all(|streams| *streams == logs));
    let generator = engine
        .asked
        .iter()
        .position(|command| holds(command, &words(&["flatbuffers-generator"])));
    let generator = generator.expect("the generator");
    let left = secs(1200) - Duration::from_micros(1_062_500);
    assert_eq!(engine.bounds[generator - 1].0, secs(1200));
    assert_eq!(engine.bounds[generator].0, left);
    // As the sandbox was removed, the copy was already made executable and
    // nothing was published.
    let staged = engine.at_removal.first().map(|(path, _)| path.clone());
    let staged = staged.expect("a stage");
    assert!(staged.starts_with("native-compiled-"), "{staged}");
    let stage = format!("{staged}/artifact");
    let seen = [
        (staged.clone(), 0o700),
        (stage.clone(), 0o700),
        (format!("{stage}/binary"), 0o755),
    ];
    assert_eq!(engine.at_removal, seen);
    let mut names = begun(&directory, "");
    names.sort();
    assert_eq!(names, ["binary", "receipt.json"]);
    let mode = fs::metadata(directory.join("binary"))
        .expect("a binary")
        .permissions()
        .mode();
    assert_eq!(mode & 0o7777, 0o755);
    let checked =
        super::compiled::compiled_receipt(&directory, "asan", IMAGE, &bench.inputs, "amd64");
    assert!(checked.is_ok());
    assert_eq!(begun(&parent, ""), ["compiled"]);
    assert!(removed(&engine));
}

#[test]
fn the_toolchain_pins_both_linux_architectures_exactly() {
    let root = repository();
    let pin =
        Document::read(&fs::read(root.join("security/native/toolchain.json")).expect("a pin"))
            .expect("a pin");
    let platforms = pin
        .get("platforms")
        .and_then(Document::fields)
        .expect("platforms");
    let names: Vec<&str> = platforms.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(names.len(), 2);
    for (name, asset) in platforms {
        assert!(
            ["linux-x86_64", "linux-aarch64"].contains(&name.as_str()),
            "{name}"
        );
        assert!(super::hexadecimal(text(asset, "sha256"), 64), "{name}");
        let url = text(asset, "url");
        assert!(
            url.contains("/llvmorg-23.1.2/") && !url.contains("latest"),
            "{name}"
        );
    }
}

/// The image's recipe in the checkout.
fn recipe() -> String {
    let path = repository().join("build/native-security.Dockerfile");
    fs::read_to_string(path).expect("the recipe")
}

/// The recipe's instructions, each with the number of its stage: its
/// continued lines joined, and the script it reads from a document here
/// kept with it, line by line.
fn instructions(recipe: &str) -> Vec<(usize, String)> {
    let mut instructions = Vec::new();
    let mut stage = 0;
    let mut lines = recipe.lines();
    while let Some(line) = lines.next() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut instruction = line.to_owned();
        while let Some(continued) = instruction.strip_suffix('\\') {
            let next = lines.next().expect("a continued line");
            instruction = format!("{continued} {}", next.trim_start());
        }
        if let Some((_, marker)) = instruction.split_once("<<'") {
            let marker = marker
                .strip_suffix('\'')
                .expect("a quoted marker")
                .to_owned();
            for line in lines.by_ref().take_while(|line| *line != marker) {
                instruction.push('\n');
                instruction.push_str(line);
            }
        }
        stage += usize::from(instruction.starts_with("FROM "));
        instructions.push((stage, instruction));
    }
    instructions
}

/// The shell commands of `script` that run `program`, each whole.
fn invocations(script: &str, program: &str) -> Vec<String> {
    let joined = script.replace("\\\n", " ");
    let commands = joined.split(['\n', ';', '|', '&', '(', ')', '{', '}']);
    let lead = ["RUN", "if", "then", "else", "do", "!"];
    commands
        .filter(|command| {
            let mut words = command.split_whitespace();
            words.find(|word| !lead.contains(word)) == Some(program)
        })
        .map(|command| command.trim().to_owned())
        .collect()
}

/// The names of the build arguments that `instruction` declares.
fn declared(instruction: &str) -> Option<&str> {
    let name = instruction.strip_prefix("ARG ")?;
    Some(name.split_once('=').map_or(name, |(name, _)| name).trim())
}

#[test]
fn the_recipe_authenticates_the_pinned_toolchain_with_no_code_of_its_own() {
    let recipe = recipe();
    let instructions = instructions(&recipe);
    let stages = instructions
        .iter()
        .filter(|(_, held)| held.starts_with("FROM "));
    let stages: Vec<&str> = stages.map(|(_, held)| held.as_str()).collect();
    assert_eq!(stages.len(), 3);
    let base = stages[0].strip_prefix("FROM docker.io/library/fedora:44@sha256:");
    let base = base
        .and_then(|rest| rest.split_once(' '))
        .expect("a pinned base");
    assert!(super::hexadecimal(base.0, 64), "{}", base.0);
    assert_eq!(base.1, "AS native-base");
    assert_eq!(
        stages[1..],
        ["FROM native-base AS llvm-toolchain", "FROM native-base"]
    );
    // Every pin arrives as a build argument of the toolchain's stage, and
    // none may be missing or empty.
    let toolchain = instructions.iter().filter(|(stage, _)| *stage == 2);
    let names: Vec<&str> = toolchain.filter_map(|(_, held)| declared(held)).collect();
    assert_eq!(
        names,
        ["LLVM_URL", "LLVM_SHA256", "LLVM_BYTES", "LLVM_DIRECTORY"]
    );
    let required: Vec<String> = names
        .iter()
        .map(|name| format!("\"${{{name}:?}}\""))
        .collect();
    let required = format!(": {}\nset -C\n", required.join(" "));
    assert!(recipe.contains(&required), "{required}");
    // The archive is fetched over HTTPS alone, in at most its pinned bytes,
    // then held to exactly those bytes and their digest.
    let mut fetched = Vec::new();
    for (_, held) in &instructions {
        fetched.extend(invocations(held, "curl"));
    }
    assert_eq!(fetched.len(), 1, "{fetched:?}");
    for option in [
        "--fail",
        "--location",
        "--proto '=https'",
        "--proto-redir '=https'",
        "--connect-timeout 60",
        "--max-filesize \"$LLVM_BYTES\"",
        "-- \"$LLVM_URL\" > /tmp/llvm.tar.zst",
    ] {
        assert!(fetched[0].contains(option), "{option}");
    }
    for check in [
        "[ \"$(stat --format=%s /tmp/llvm.tar.zst)\" = \"$LLVM_BYTES\" ] || {",
        "  *[!0-9a-f]*) echo 'LLVM archive does not match pinned bytes' >&2; exit 1 ;;",
        concat!(
            "printf '%s  %s\\n' \"$LLVM_SHA256\" /tmp/llvm.tar.zst",
            " | sha256sum --check --strict --status || {"
        ),
        "    echo 'LLVM archive exceeds pinned size' >&2",
        "  echo 'LLVM archive member listing exceeds budget' >&2",
        "    *) echo 'Unexpected LLVM archive member layout' >&2; exit 1 ;;",
        "    */../*) echo 'Unexpected LLVM archive member layout' >&2; exit 1 ;;",
        "timeout 180 tar --use-compress-program='zstd --long=30' -tf /tmp/llvm.tar.zst \\",
        "--strip-components=1 --no-recursion --files-from=/tmp/llvm-members.txt\n",
        "CC=/opt/llvm/bin/clang CXX=/opt/llvm/bin/clang++",
    ] {
        assert!(recipe.contains(check), "{check}");
    }
    assert_eq!(
        recipe
            .matches("LLVM archive does not match pinned bytes")
            .count(),
        3
    );
    // Nothing floats, and no code of the repository's own is in the image:
    // every script the recipe holds is the shell's.
    assert!(!recipe.contains("latest"));
    assert!(!recipe.contains("python3 - ") && !recipe.contains("<<'PY'"));
    let scripts = instructions.iter().filter(|(_, held)| held.contains("<<"));
    let scripts: Vec<&str> = scripts.map(|(_, held)| held.as_str()).collect();
    assert_eq!(scripts.len(), 3);
    for script in scripts {
        assert!(script.starts_with("RUN /bin/sh -eu <<'SH'\n"), "{script}");
    }
    let inputs = super::inputs::source_files(&repository()).expect("the inputs");
    let inputs = relative(&inputs, &repository());
    let inputs: Vec<&str> = inputs
        .items()
        .expect("inputs")
        .iter()
        .filter_map(Document::text)
        .collect();
    let copies = instructions
        .iter()
        .filter(|(_, held)| held.starts_with("COPY "));
    let copies: Vec<&str> = copies.map(|(_, held)| held.as_str()).collect();
    assert_eq!(copies.len(), 4);
    for copy in copies {
        let words: Vec<&str> = copy.split_whitespace().collect();
        if words[1].starts_with("--from=") {
            assert_eq!(copy, "COPY --from=llvm-toolchain /opt/llvm /opt/llvm");
            continue;
        }
        for source in &words[1..words.len() - 1] {
            assert!(!source.ends_with(".py"), "{source}");
            let below = format!("{source}/");
            let bound = inputs
                .iter()
                .any(|input| input == source || input.starts_with(&below));
            assert!(bound, "{source} is no input of the image");
        }
    }
}

#[test]
fn the_recipe_declares_exactly_the_build_arguments_the_preparation_passes() {
    let bench = Bench::new("arguments");
    let output = bench.directory("prepared");
    let mut engine = bench.engine();
    let prepared = hosted(&bench.root, &mut engine, Ok("amd64"), &budgeted, |host| {
        super::run::prepare(host, &output)
    });
    prepared.expect("a prepared image");
    let build = &engine.asked[0];
    assert_eq!(build[0], "build");
    let passed = build.windows(2).filter(|pair| pair[0] == "--build-arg");
    let passed = passed.map(|pair| pair[1].split_once('=').expect("a value").0);
    let passed: std::collections::BTreeSet<&str> = passed.collect();
    let recipe = recipe();
    let instructions = instructions(&recipe);
    let declared = instructions.iter().filter_map(|(_, held)| declared(held));
    let declared: std::collections::BTreeSet<&str> = declared.collect();
    assert!(!passed.is_empty());
    assert_eq!(passed, declared);
}

/// The absolute paths of the system's programs that `text` names: every
/// run of a path's letters that begins below `/usr/bin/` or `/bin/`.
fn system_programs(text: &str) -> std::collections::BTreeSet<String> {
    let letter = |letter: char| letter.is_ascii_alphanumeric() || "/_.+-".contains(letter);
    let words = text.split(|held: char| !letter(held));
    let programs = words.filter(|word| word.starts_with("/usr/bin/") || word.starts_with("/bin/"));
    programs.map(str::to_owned).collect()
}

/// `text` with each of its runs of space as one space and each of its runs
/// of digits as one zero.
fn shaped(text: &str) -> String {
    let mut shaped = String::new();
    let mut last = None;
    for letter in text.chars() {
        let kept = if letter.is_whitespace() {
            ' '
        } else if letter.is_ascii_digit() {
            '0'
        } else {
            letter
        };
        if !(last == Some(kept) && (kept == ' ' || letter.is_ascii_digit())) {
            shaped.push(kept);
        }
        last = Some(kept);
    }
    shaped
}

/// The commands a build kept, that build run again, and a fresh replay
/// asked the engine, in order.
fn operated(bench: &Bench) -> Vec<Vec<String>> {
    let directory = bench.scratch.0.join("kept");
    let output = bench.directory("compiled");
    let mut engine = bench.engine();
    compiling(bench, &mut engine, "asan", &directory, &output).expect("a kept build");
    let mut commands = engine.asked;
    for (mode, compiled) in [("asan", Some(directory.as_path())), ("replay", None)] {
        let output = bench.directory(&format!("ran-{mode}"));
        let mut engine = bench.engine();
        ran(bench, &mut engine, mode, compiled, &output).expect("a passing suite");
        commands.extend(engine.asked);
    }
    commands
}

#[test]
fn the_image_is_refused_unless_every_program_the_host_runs_in_it_works() {
    let bench = Bench::new("programs");
    let commands = operated(&bench);
    let recipe = recipe();
    let instructions = instructions(&recipe);
    let last = instructions.iter().map(|(stage, _)| *stage).max();
    let checks = instructions.iter().filter(|(stage, held)| {
        Some(*stage) == last && held.starts_with("RUN ") && !system_programs(held).is_empty()
    });
    let checks: Vec<&str> = checks.map(|(_, held)| held.as_str()).collect();
    assert_eq!(checks.len(), 1, "{checks:?}");
    let check = checks[0];
    let mut wanted = std::collections::BTreeSet::new();
    for command in &commands {
        for word in command {
            wanted.extend(system_programs(word));
        }
    }
    assert!(
        wanted.contains("/usr/bin/sleep") && wanted.len() > 1,
        "{wanted:?}"
    );
    assert_eq!(system_programs(check), wanted);
    // Each is tried with the options the host gives it there: what comes
    // before the first path or variable it is handed.
    let tried = shaped(&check.replace("\\\n", " "));
    let mut forms = 0;
    for command in &commands {
        let at = command.iter().position(|word| word == CONTAINER);
        let Some(argv) = at.map(|at| &command[at + 1..]) else {
            continue;
        };
        if command[0] != "exec" || system_programs(&argv[0]).is_empty() {
            continue;
        }
        let given = argv[1..].iter().take_while(|word| {
            !word.starts_with('/') && (word.starts_with('-') || !word.contains('='))
        });
        let mut form = argv[0].clone();
        for word in given {
            let special = |letter: char| letter.is_whitespace() || "\"$&|;<>\\".contains(letter);
            assert!(!word.contains('\''), "{word}");
            if word.chars().any(special) {
                form.push_str(&format!(" '{word}'"));
            } else {
                form.push_str(&format!(" {word}"));
            }
        }
        assert!(tried.contains(&shaped(&form)), "{form}");
        forms += 1;
    }
    assert!(forms > 10, "{forms}");
    // The image alone does nothing but wait as long as the host's sandbox.
    let created = commands.iter().find(|command| command[0] == "create");
    let created = created.expect("a sandbox");
    let entrypoint = created
        .iter()
        .find_map(|word| word.strip_prefix("--entrypoint="));
    let entrypoint = entrypoint.expect("an entry point");
    let lasting = created.last().expect("a lifetime");
    let ending = instructions
        .iter()
        .rev()
        .take(2)
        .map(|(_, held)| held.as_str());
    let ending: Vec<&str> = ending.collect();
    assert_eq!(
        ending,
        [
            format!("CMD [\"{lasting}\"]"),
            format!("ENTRYPOINT [\"{entrypoint}\"]")
        ]
    );
}

#[test]
fn a_phase_past_its_bound_is_the_hosts_timeout_and_nothing_starts_after_it() {
    let bench = Bench::new("deadline");
    // A build's phase that outlasts what is left of the build's bound.
    let output = bench.directory("compile");
    let directory = bench.scratch.0.join("compiled");
    let mut engine = bench.engine();
    engine.script = vec![on(&["setup"], Act::Lasts(secs(1201)))];
    let compiled = compiling(&bench, &mut engine, "asan", &directory, &output);
    assert_eq!(compiled.err(), Some(Fault("command_timed_out")));
    assert_eq!(complaints(&output, "compile"), b"");
    let start = cleanup(&engine);
    assert!(holds(&engine.asked[start - 1], &words(&["setup"])));
    let room = Some((STEP_LOG, STEP_LOG));
    assert_eq!(engine.bounds[start - 1], (secs(1200), room));
    assert!(directory.symlink_metadata().is_err());
    // A test once the build has taken most of the run's bound.
    let output = bench.directory("late");
    let mut engine = bench.engine();
    engine.script = vec![
        on(&["setup"], Act::Lasts(secs(1000))),
        on(&["flatbuffers-generator"], Act::Lasts(secs(1000))),
        on(&["--invisibles"], Act::Lasts(secs(101))),
    ];
    let late = ran(&bench, &mut engine, "asan", None, &output);
    assert_eq!(late.err(), Some(Fault("command_timed_out")));
    assert_eq!(complaints(&output, "native"), b"");
    let start = cleanup(&engine);
    assert!(holds(&engine.asked[start - 1], &words(&["--invisibles"])));
    assert_eq!(engine.bounds[start - 1], (secs(100), room));
    // A step that ends as the run's bound ends leaves no time for another.
    let output = bench.directory("over");
    let mut engine = bench.engine();
    engine.script = vec![on(&["/usr/bin/cp"], Act::Lasts(secs(2100)))];
    let over = ran(&bench, &mut engine, "replay", None, &output);
    assert_eq!(over.err(), Some(Fault("command_timed_out")));
    let start = cleanup(&engine);
    assert!(holds(&engine.asked[start - 1], &words(&["/usr/bin/cp"])));
    assert_eq!(engine.bounds[start - 1], (secs(2100), room));
    assert_eq!(complaints(&output, "native"), b"");
    // The import of a kept build is bounded as a whole, and the run's own
    // bound begins once it is over.
    let directory = kept(&bench, "asan", "kept");
    let output = bench.directory("import");
    let mut engine = bench.engine();
    engine.script = vec![on(&["/bin/sh", RESTORED], Act::Lasts(secs(120)))];
    let imported = ran(&bench, &mut engine, "asan", Some(&directory), &output);
    assert_eq!(imported.err(), Some(Fault("command_timed_out")));
    let start = cleanup(&engine);
    assert_eq!(engine.asked[start - 1], writing(RESTORED));
    assert!(!output.join("native.stdout.log").exists());
    let output = bench.directory("imported");
    let mut engine = bench.engine();
    engine.script = vec![on(&["/bin/sh", RESTORED], Act::Lasts(secs(100)))];
    ran(&bench, &mut engine, "asan", Some(&directory), &output).expect("a reused build");
    let fed = Some((MIB, MIB));
    let import = [
        (secs(120), fed),
        (secs(20), fed),
        (secs(20), None),
        (secs(20), None),
        (secs(2100), None),
    ];
    assert_eq!(engine.bounds[3..8], import);
}

#[test]
fn a_step_has_half_the_phases_log_and_never_more_than_is_left() {
    let bench = Bench::new("rooms");
    let max = MAX_LOG;
    let generator = "flatbuffers-generator";
    let compiled = ["compile", "mediasoup-worker-test-asan-address"];
    let run = |name: &str, script: Vec<Cue>| {
        let output = bench.directory(name);
        let mut engine = bench.engine();
        engine.script = script;
        let ran = ran(&bench, &mut engine, "asan", None, &output);
        (engine, output, ran)
    };
    let rooms = |engine: &Sandboxed| {
        let start = cleanup(engine);
        let bounds = engine.bounds[start - 3..start].iter();
        bounds.map(|(_, room)| *room).collect::<Vec<_>>()
    };
    let half = Some((STEP_LOG, STEP_LOG));
    // What is left of the phase's log, once it is less than half of it, is
    // the host's to bound.
    let (engine, output, said) = run(
        "output",
        vec![
            on(&["setup"], Act::Says(STEP_LOG, 0)),
            on(&[generator], Act::Says(STEP_LOG - 10, 0)),
            on(&compiled, Act::Says(11, 0)),
        ],
    );
    assert_eq!(said.err(), Some(Fault("command_stdout_limit_exceeded")));
    let log = fs::read(output.join("native.stdout.log")).expect("a log");
    assert_eq!(log.len() as u64, max);
    assert_eq!(complaints(&output, "native"), b"");
    assert_eq!(rooms(&engine), [half, half, Some((10, STEP_LOG))]);
    let (engine, output, said) = run(
        "error",
        vec![
            on(&["setup"], Act::Says(0, STEP_LOG)),
            on(&[generator], Act::Says(0, STEP_LOG - 10)),
            on(&compiled, Act::Says(0, 11)),
        ],
    );
    assert_eq!(said.err(), Some(Fault("command_stderr_limit_exceeded")));
    let log = complaints(&output, "native");
    assert!(log.len() as u64 == max && log.iter().all(|byte| *byte == b'e'));
    assert_eq!(rooms(&engine), [half, half, Some((STEP_LOG, 10))]);
    // Half of what is left is still the step's own.
    let (engine, output, said) = run(
        "half",
        vec![
            on(&["setup"], Act::Says(STEP_LOG, 0)),
            on(&[generator], Act::Says(STEP_LOG + 1, 0)),
        ],
    );
    assert_eq!(said.err(), Some(Fault("native_failed_exit_1")));
    let code = "command_stdout_limit_exceeded";
    assert!(logged(&complaints(&output, "native"), code));
    let start = cleanup(&engine);
    assert_eq!(engine.bounds[start - 1].1, half);
    // The line of a failure is kept as far as the log has room for it.
    let (_, output, said) = run(
        "line",
        vec![
            on(&["setup"], Act::Says(0, STEP_LOG)),
            on(&[generator], Act::Says(0, STEP_LOG - 5)),
            on(&compiled, Act::Ends(1)),
        ],
    );
    assert_eq!(said.err(), Some(Fault("command_stderr_limit_exceeded")));
    let log = complaints(&output, "native");
    assert!(log.len() as u64 == max && log.ends_with(b"eeeeeNativ"));
    let (_, output, said) = run(
        "full",
        vec![
            on(&["setup"], Act::Says(0, STEP_LOG)),
            on(&[generator], Act::Says(0, STEP_LOG + 1)),
        ],
    );
    assert_eq!(said.err(), Some(Fault("command_stderr_limit_exceeded")));
    let log = complaints(&output, "native");
    assert!(log.len() as u64 == max && log.iter().all(|byte| *byte == b'e'));
}

#[test]
fn a_kept_build_is_imported_then_proved_then_run_in_the_references_order() {
    let bench = Bench::new("reuse");
    let directory = kept(&bench, "asan", "kept");
    let bytes = fs::read(directory.join("binary")).expect("a kept executable");
    let output = bench.directory("reused");
    let mut engine = bench.engine();
    let reused = ran(&bench, &mut engine, "asan", Some(&directory), &output);
    let reused = reused.expect("a reused build");
    let mut wanted = started(&bench.inputs);
    wanted.extend([
        writing(RESTORED),
        exec(&["/usr/bin/chmod", "700", RESTORED]),
        exec(&["/usr/bin/stat", "--format=%s", RESTORED]),
        exec(&["/usr/bin/sha256sum", RESTORED]),
    ]);
    wanted.extend(sourced());
    wanted.extend(identified(RESTORED));
    wanted.push(worked(&[RESTORED, "--invisibles"], None));
    wanted.extend(catalogued());
    wanted.extend(cleaned());
    assert_eq!(engine.asked, wanted);
    assert_eq!(engine.held(RESTORED), Some(bytes));
    // What the import's commands say is dropped, as the reference's host
    // dropped its receiver's; every later step's is the run's to keep.
    let dropped = Some((PathBuf::from("/dev/null"), PathBuf::from("/dev/null")));
    assert_eq!(engine.streams[3..5], [dropped.clone(), dropped]);
    let logs = (
        output.join("native.stdout.log"),
        output.join("native.stderr.log"),
    );
    let later = engine.streams[7..].iter().flatten();
    assert!(later.clone().count() > 4);
    assert!(later.into_iter().all(|streams| *streams == logs));
    let fed = Some((MIB, MIB));
    let import = [
        (secs(120), fed),
        (secs(120), fed),
        (secs(120), None),
        (secs(120), None),
    ];
    assert_eq!(engine.bounds[3..7], import);
    let worker = reused.get("worker").expect("the worker's record");
    assert_eq!(worker.get("build"), Some(&Document::Record(Vec::new())));
    let completed = Document::List(vec!["asan".into()]);
    assert_eq!(worker.get("completed"), Some(&completed));
    assert_eq!(reused.get("compiledReused"), Some(&Document::Bool(true)));
    // The restored program is held to a kept one's rules as it is proved.
    let output = bench.directory("directory");
    let mut engine = bench.engine();
    let stated = Act::Prints(b"41ed 96 1.0\n".to_vec());
    engine.script = vec![on(&[STAT, RESTORED], stated)];
    let refused = ran(&bench, &mut engine, "asan", Some(&directory), &output);
    assert_eq!(refused.err(), Some(Fault("native_failed_exit_1")));
    let code = "compiled_binary_not_regular";
    assert!(logged(&complaints(&output, "native"), code));
    assert!(!asked(&engine, "--invisibles"));
}

#[test]
fn a_kept_replay_build_runs_every_reviewed_input_without_a_build() {
    let bench = Bench::new("replayed");
    let cases = bench.cases();
    let directory = kept(&bench, "replay", "kept");
    let output = bench.directory("replayed");
    let mut engine = bench.engine();
    let replayed = ran(&bench, &mut engine, "replay", Some(&directory), &output);
    let replayed = replayed.expect("a reused build");
    assert!(!asked(&engine, MESON));
    let commands = engine.asked.iter().zip(&engine.bounds);
    let replays: Vec<_> = commands
        .filter(|(command, _)| command.contains(&"-seed=1".to_owned()))
        .collect();
    assert_eq!(replays.len(), cases.len());
    let mut completed = Vec::new();
    for ((command, bound), case) in replays.into_iter().zip(&cases) {
        let path = format!("/work/corpus/{}/{}", case.family, case.identifier);
        let argv = super::sandbox::replay_args(RESTORED, &path);
        let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
        assert_eq!(*command, worked(&argv, Some(&case.family)));
        assert_eq!(*bound, (secs(10), Some((STEP_LOG, STEP_LOG))));
        completed.push(Document::from(case.identifier.as_str()));
    }
    let worker = replayed.get("worker").expect("the worker's record");
    assert_eq!(worker.get("completed"), Some(&Document::List(completed)));
}

#[test]
fn a_kept_build_whose_bytes_change_on_the_host_is_never_sent() {
    let bench = Bench::new("transfer");
    let directory = kept(&bench, "asan", "kept");
    let output = bench.directory("changed");
    let mut engine = bench.engine();
    engine.tampered = Some(("start", directory.join("binary"), executable(62, 97)));
    let changed = ran(&bench, &mut engine, "asan", Some(&directory), &output);
    assert_eq!(changed.err(), Some(Fault("compiled_transfer_changed")));
    let mut wanted = started(&bench.inputs);
    wanted.extend(cleaned());
    assert_eq!(engine.asked, wanted);
    assert!(output.join("ownership.json").is_file());
}

#[test]
fn a_kept_build_that_is_not_imported_whole_is_the_hosts_import_failure() {
    let bench = Bench::new("import");
    let directory = kept(&bench, "asan", "kept");
    let sum = |summed: &str| Act::Prints(format!("{summed}  {RESTORED}\n").into_bytes());
    let (size, summed) = ("--format=%s", "/usr/bin/sha256sum");
    let failed = "compiled_import_failed";
    let table = [
        (on(&["/bin/sh", RESTORED], Act::Ends(1)), failed),
        (on(&["/usr/bin/chmod", "700"], Act::Ends(1)), failed),
        (on(&[size, RESTORED], Act::Ends(1)), failed),
        (on(&[size, RESTORED], Act::Prints(b"95\n".to_vec())), failed),
        (on(&[size, RESTORED], Act::Prints(b"97\n".to_vec())), failed),
        (
            on(&[size, RESTORED], Act::Prints(b"ninety-six\n".to_vec())),
            failed,
        ),
        (on(&[summed, RESTORED], sum(&"0".repeat(64))), failed),
        (on(&[summed, RESTORED], Act::Ends(1)), failed),
        (on(&[summed, RESTORED], Act::Prints(vec![0xff])), failed),
        (
            on(
                &["/bin/sh", RESTORED],
                Act::Stopped("command_stdout_limit_exceeded"),
            ),
            "command_stdout_limit_exceeded",
        ),
        (
            on(
                &["/usr/bin/chmod", "700"],
                Act::Stopped("command_timed_out"),
            ),
            "command_timed_out",
        ),
        (
            on(&[summed, RESTORED], Act::Stopped("command_timed_out")),
            "command_timed_out",
        ),
        (
            on(
                &[size, RESTORED],
                Act::Stopped("command_stderr_limit_exceeded"),
            ),
            "command_stderr_limit_exceeded",
        ),
        (
            on(&["/usr/bin/chmod", "700"], Act::Cancels),
            "command_lease_lost",
        ),
    ];
    for (index, (script, code)) in table.into_iter().enumerate() {
        let output = bench.directory(&format!("import-{index}"));
        let mut engine = bench.engine();
        engine.script = vec![script.clone()];
        let imported = ran(&bench, &mut engine, "asan", Some(&directory), &output);
        assert_eq!(imported.err(), Some(Fault(code)), "{index}");
        assert!(!asked(&engine, "-0"), "{index}");
        assert!(!output.join("native.stdout.log").exists(), "{index}");
        let start = cleanup(&engine);
        assert!(holds(&engine.asked[start - 1], &script.0), "{index}");
        assert_eq!(engine.listed, "", "{index}");
    }
}

#[test]
fn a_kept_build_that_is_not_its_own_in_the_sandbox_runs_nothing() {
    let bench = Bench::new("mismatch");
    let directory = kept(&bench, "asan", "kept");
    let summed = crate::digest(&fs::read(directory.join("binary")).expect("a kept executable"));
    let sum = |summed: &str| Act::Prints(format!("{summed}  {RESTORED}\n").into_bytes());
    let output = bench.directory("mismatch");
    let mut engine = bench.engine();
    engine.script = vec![
        on(&["/usr/bin/sha256sum", RESTORED], sum(&summed)),
        on(&["/usr/bin/sha256sum", RESTORED], sum(&"f".repeat(64))),
    ];
    let mismatched = ran(&bench, &mut engine, "asan", Some(&directory), &output);
    assert_eq!(mismatched.err(), Some(Fault("native_failed_exit_1")));
    let line = failure("compiled_runtime_mismatch");
    assert_eq!(complaints(&output, "native"), line.as_bytes());
    assert!(!asked(&engine, "--invisibles"));
    let start = cleanup(&engine);
    assert_eq!(
        engine.asked[start - 1],
        exec(&["/usr/bin/stat", STAT, RESTORED])
    );
}

#[test]
fn a_kept_build_that_is_not_the_runs_asks_the_engine_nothing_more() {
    let bench = Bench::new("other");
    let directory = kept(&bench, "asan", "kept");
    let output = bench.directory("other");
    let mut engine = bench.engine();
    let other = ran(&bench, &mut engine, "ubsan", Some(&directory), &output);
    assert_eq!(other.err(), Some(Fault("compiled_receipt_mismatch")));
    assert_eq!(engine.asked, [inspected()]);
    let mut changed = executable(62, 96);
    changed[95] = 1;
    fs::write(directory.join("binary"), changed).expect("other bytes");
    let mut engine = bench.engine();
    let other = ran(&bench, &mut engine, "asan", Some(&directory), &output);
    assert_eq!(other.err(), Some(Fault("compiled_binary_mismatch")));
    assert_eq!(engine.asked, [inspected()]);
    for (path, code) in [
        (PathBuf::from("kept"), "compiled_path"),
        (directory.join("..").join("kept"), "compiled_path"),
        (bench.scratch.0.join("absent"), UNUSABLE),
    ] {
        let mut engine = bench.engine();
        let other = ran(&bench, &mut engine, "asan", Some(&path), &output);
        assert_eq!(other.err(), Some(Fault(code)), "{path:?}");
        assert_eq!(engine.asked, [inspected()], "{path:?}");
    }
    assert!(!output.join("ownership.json").exists());
}

#[test]
fn a_build_whose_copy_is_not_what_was_built_is_not_published() {
    let bench = Bench::new("copied");
    let mut other = executable(62, 96);
    other[95] = 1;
    let two = format!("{CONTAINER}\n{CONTAINER}\n").into_bytes();
    // A copy of other bytes is one the host proves itself, after the export.
    type Copied = Option<Vec<u8>>;
    let table: Vec<(Copied, Vec<Cue>, &str)> = vec![
        (Some(executable(62, 97)), vec![], "compiled_copy_mismatch"),
        (Some(other), vec![], "compiled_copy_mismatch"),
        (
            Some(executable(183, 96)),
            vec![],
            "compiled_binary_architecture",
        ),
        (Some(vec![0; 63]), vec![], "compiled_binary_size"),
        // The sizes at the bounds of an executable's pass in the sandbox.
        (
            None,
            stated_twice("81ed 64 1.0\n"),
            "compiled_copy_mismatch",
        ),
        (
            None,
            stated_twice("81ed 536870912 1.0\n"),
            "compiled_copy_mismatch",
        ),
        (
            None,
            vec![on(&["/usr/bin/cat", BUILT], Act::Ends(1))],
            "compiled_export_failed",
        ),
        (
            None,
            vec![on(
                &["/usr/bin/cat", BUILT],
                Act::Stopped("command_timed_out"),
            )],
            "command_timed_out",
        ),
        (
            None,
            vec![on(&["ps"], Act::Prints(two.clone()))],
            "unexpected_owned_containers",
        ),
        (
            None,
            vec![on(&["setup"], Act::Ends(1)), on(&["ps"], Act::Prints(two))],
            "unexpected_owned_containers",
        ),
    ];
    for (index, (exported, script, code)) in table.into_iter().enumerate() {
        let executable = exported.is_some();
        let parent = bench.directory(&format!("copied-{index}"));
        let output = bench.directory(&format!("copied-{index}-evidence"));
        let directory = parent.join("compiled");
        let mut engine = bench.engine();
        engine.exported = exported;
        engine.script = script;
        engine.watched = Some(parent.clone());
        let compiled = compiling(&bench, &mut engine, "asan", &directory, &output);
        assert_eq!(compiled.err(), Some(Fault(code)), "{index}");
        assert!(begun(&parent, "").is_empty(), "{index}");
        if executable {
            // The copy was made executable and proved before the removal.
            assert_eq!(complaints(&output, "compile"), b"", "{index}");
            let binary = engine
                .at_removal
                .iter()
                .find(|(path, _)| path.ends_with("/binary"));
            assert_eq!(binary.map(|(_, mode)| *mode), Some(0o755), "{index}");
            assert!(removed(&engine), "{index}");
        }
    }
}

#[test]
fn a_kept_builds_directory_must_be_named_as_it_resolves() {
    let scratch = Scratch::new("resolved");
    let root = scratch.0.join("tree");
    fs::create_dir_all(root.join("real/sub")).expect("a tree");
    symlink(root.join("real"), root.join("link")).expect("a link");
    symlink(root.join("missing"), root.join("dangling")).expect("a link");
    symlink("loop", root.join("loop")).expect("a loop");
    fs::write(root.join("file"), "x").expect("a file");
    symlink("../real", root.join("real/up")).expect("a link");
    symlink(root.join("aloop"), root.join("aloop")).expect("a loop");
    symlink(root.join("real"), root.join("real/self")).expect("a link");
    let tree = root.to_string_lossy().into_owned();
    // What the reference answered for each on this tree.
    for (path, canonical) in [
        (format!("{tree}/real/compiled"), true),
        (format!("{tree}/real"), true),
        (format!("{tree}/link/compiled"), false),
        (format!("{tree}/link"), false),
        (format!("{tree}/real/sub/.."), false),
        (format!("{tree}/real/./compiled"), true),
        (format!("{tree}/real//compiled"), true),
        (format!("{tree}/real/compiled/"), true),
        (format!("{tree}/missing/compiled"), true),
        (format!("{tree}/missing/../real"), false),
        (format!("{tree}/dangling"), false),
        (format!("{tree}/dangling/compiled"), false),
        (format!("{tree}/loop"), true),
        (format!("{tree}/loop/compiled"), true),
        (format!("{tree}/file/compiled"), true),
        ("relative/compiled".to_owned(), false),
        (format!("/{tree}/real/compiled"), false),
        (format!("//{tree}/real/compiled"), true),
        ("/".to_owned(), true),
        (format!("{tree}/real/.."), false),
        (format!("{tree}/link/../real"), false),
        (format!("{tree}/real/up"), false),
        (format!("{tree}/real/up/compiled"), false),
        (format!("{tree}/aloop"), true),
        (format!("{tree}/aloop/compiled"), true),
        (format!("{tree}/real/self"), false),
        (format!("{tree}/real/self/compiled"), false),
        (String::new(), false),
        (".".to_owned(), false),
    ] {
        let found = crate::paths::canonical(Path::new(&path));
        assert_eq!(found, canonical, "{path}");
    }
    // A build is refused a directory named otherwise, or one that is there,
    // before the engine is asked anything.
    let bench = Bench::new("named");
    let output = bench.directory("named");
    for (path, code) in [
        (PathBuf::from("compiled"), "compiled_path"),
        (root.join("real/sub/.."), "compiled_path"),
        (root.join("link/compiled"), "compiled_path"),
        (root.join("link"), "compiled_path"),
        (root.join("dangling"), "compiled_path"),
        (root.join("real"), "compiled_already_exists"),
        (root.join("real/sub"), "compiled_already_exists"),
    ] {
        let mut engine = bench.engine();
        let compiled = compiling(&bench, &mut engine, "asan", &path, &output);
        assert_eq!(compiled.err(), Some(Fault(code)), "{path:?}");
        assert!(engine.asked.is_empty(), "{path:?}");
    }
    // One below a directory that is not there is named as it resolves, and
    // fails as the reference did, as its temporary directory is made.
    let mut engine = bench.engine();
    let missing = root.join("missing/compiled");
    let compiled = compiling(&bench, &mut engine, "asan", &missing, &output);
    assert_eq!(compiled.err(), Some(Fault(UNUSABLE)));
    assert_eq!(engine.asked, [inspected()]);
    assert!(!output.join("ownership.json").exists());
    // A link to itself is named as it resolves and is not there, so the
    // build is made, and its publication fails as the reference's did.
    let output = bench.directory("looped");
    let mut engine = bench.engine();
    let compiled = compiling(&bench, &mut engine, "asan", &root.join("loop"), &output);
    assert_eq!(compiled.err(), Some(Fault(UNUSABLE)));
    assert!(removed(&engine));
    assert!(begun(&root, "native-compiled").is_empty());
}

#[test]
fn an_unsupported_machine_is_refused_where_the_reference_first_asked_it() {
    let bench = Bench::new("machine");
    let unsupported = Err(Fault("native_host_architecture_unsupported"));
    let output = bench.directory("machine");
    let absent = bench.scratch.0.join("absent");
    let mut engine = bench.engine();
    let prepared = hosted(&bench.root, &mut engine, unsupported, &budgeted, |host| {
        super::run::prepare(host, &output)
    });
    let suite = hosted(&bench.root, &mut engine, unsupported, &budgeted, |host| {
        super::run::run(host, &request("asan", None), &output)
    });
    let compiled = hosted(&bench.root, &mut engine, unsupported, &budgeted, |host| {
        super::run::compile(host, &request("asan", Some(&absent)), &output)
    });
    for refused in [prepared, suite, compiled] {
        assert_eq!(refused.err(), unsupported.err());
    }
    assert!(engine.asked.is_empty());
    assert!(begun(&output, "").is_empty());
    // A kept build's directory is judged before the machine is.
    let existing = hosted(&bench.root, &mut engine, unsupported, &budgeted, |host| {
        super::run::compile(host, &request("asan", Some(&output)), &output)
    });
    assert_eq!(existing.err(), Some(Fault("compiled_already_exists")));
}

#[test]
fn a_builds_budget_is_asked_as_its_sandbox_is_made() {
    let bench = Bench::new("budget");
    let refused =
        || -> crate::Result<crate::budget::Budget> { Err(Fault("codeql_insufficient_resources")) };
    let output = bench.directory("prepared");
    let mut engine = bench.engine();
    let prepared = hosted(&bench.root, &mut engine, Ok("amd64"), &refused, |host| {
        super::run::prepare(host, &output)
    });
    assert!(prepared.is_ok());
    let output = bench.directory("ran");
    let mut engine = bench.engine();
    let suite = hosted(&bench.root, &mut engine, Ok("amd64"), &refused, |host| {
        super::run::run(host, &request("asan", None), &output)
    });
    assert_eq!(suite.err(), Some(Fault("codeql_insufficient_resources")));
    assert_eq!(engine.asked, [inspected(), cleaned()[0].clone()]);
    assert!(output.join("ownership.json").is_file());
}

#[test]
fn the_local_engine_refuses_new_commands_once_cancelled_but_still_removes() {
    use super::engine::Engine as _;
    let scratch = Scratch::new("local");
    let logs = |name: &str| {
        let output = scratch.0.join(format!("{name}.stdout.log"));
        let error = scratch.0.join(format!("{name}.stderr.log"));
        fs::write(&output, b"").expect("a log");
        fs::write(&error, b"").expect("a log");
        (output, error)
    };
    let cancelled = || true;
    let mut engine = super::local::Local {
        prefix: vec!["/bin/echo".into()],
        cancelled: &cancelled,
    };
    let lost = Err(Fault("command_lease_lost"));
    assert_eq!(engine.ask(&words(&["ps"]), secs(5)), lost);
    let (output, error) = logs("cancelled");
    let refused = engine.run(&words(&["x"]), &[], secs(5), (&output, &error), (10, 10));
    assert_eq!(refused, lost.map(|_: Vec<u8>| 0));
    assert_eq!(fs::read(&output).expect("a log"), b"");
    let removal = engine.removing(&words(&["rm", "--force", "x"]), secs(5));
    assert_eq!(removal, Ok(b"rm --force x\n".to_vec()));
    // Each stream of a command is kept within its own room.
    let running = || false;
    let script = "printf 0123456789; printf abcdef >&2";
    let prefix = ["/bin/sh", "-c", script, "sh"].map(std::ffi::OsString::from);
    let mut engine = super::local::Local {
        prefix: prefix.to_vec(),
        cancelled: &running,
    };
    for (name, room, outcome, printed, complained) in [
        (
            "output",
            (4, 100),
            Err(Fault("command_stdout_limit_exceeded")),
            Some("0123"),
            None,
        ),
        (
            "error",
            (100, 3),
            Err(Fault("command_stderr_limit_exceeded")),
            None,
            Some("abc"),
        ),
        (
            "both",
            (100, 100),
            Ok(0),
            Some("0123456789"),
            Some("abcdef"),
        ),
    ] {
        let (output, error) = logs(name);
        let ran = engine.run(&[], &[], secs(10), (&output, &error), room);
        assert_eq!(ran, outcome, "{name}");
        if let Some(printed) = printed {
            assert_eq!(
                fs::read_to_string(&output).expect("a log"),
                printed,
                "{name}"
            );
        }
        if let Some(complained) = complained {
            assert_eq!(
                fs::read_to_string(&error).expect("a log"),
                complained,
                "{name}"
            );
        }
    }
    // A question that ends with another status is refused, even a removal.
    let prefix = ["/bin/sh", "-c", "exit 3", "sh"].map(std::ffi::OsString::from);
    let mut engine = super::local::Local {
        prefix: prefix.to_vec(),
        cancelled: &running,
    };
    let failed = Err(Fault("command_failed"));
    assert_eq!(engine.ask(&words(&["ps"]), secs(10)), failed);
    assert_eq!(engine.removing(&words(&["ps"]), secs(10)), failed);
}

#[test]
fn the_suites_are_no_job_of_the_hosted_security_workflow() {
    let workflow = repository().join(".github/workflows/security.yml");
    let workflow = fs::read(workflow).expect("the workflow");
    let workflow: serde_json::Value = serde_saphyr::from_slice(&workflow).expect("a workflow");
    let jobs = workflow.get("jobs").and_then(serde_json::Value::as_object);
    let jobs = jobs.expect("its jobs");
    assert!(jobs.contains_key("security-fast"));
    for job in ["native-security", "security-deep"] {
        assert!(!jobs.contains_key(job), "{job}");
    }
    assert_eq!(super::MODES, ["asan", "ubsan", "replay"]);
}

/// What the command made of `arguments` below `root` with only `variables`
/// in its environment, `ready` saying whether its operation may begin, and
/// `engine` as the engine it selects: its exit status, what it printed and
/// told, and the engine it selected, if it did.
fn commanded<'e>(
    root: &Path,
    arguments: &[&str],
    variables: &[(&str, &str)],
    engine: Option<&'e mut Sandboxed>,
    ready: crate::Result<()>,
) -> (u8, String, String, Option<crate::engine::Name>) {
    let clock = engine.as_ref().map_or_else(
        || Rc::new(Cell::new(Duration::ZERO)),
        |engine| Rc::clone(&engine.clock),
    );
    let now = move || clock.get();
    let variable = |name: &str| {
        let found = variables.iter().find(|(held, _)| *held == name);
        found.map(|(_, value)| std::ffi::OsString::from(value))
    };
    let selected = Cell::new(None);
    let mut slot = engine;
    let mut select =
        |name: crate::engine::Name| -> crate::Result<Box<dyn super::engine::Engine + 'e>> {
            selected.set(Some(name));
            match slot.take() {
                Some(engine) => Ok(Box::new(engine)),
                None => Err(Fault("engine_missing")),
            }
        };
    let run_id = || -> crate::Result<String> { Ok(RUN_ID.to_owned()) };
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let mut around = super::cli::Around {
        variable: &variable,
        engine: &mut select,
        machine: "x86_64",
        budget: &budgeted,
        clock: &now,
        run_id: &run_id,
        out: &mut out,
        err: &mut err,
    };
    let mut ready = move || ready;
    let status = super::cli::command(root, &words(arguments), &mut around, &mut ready);
    let text = |bytes: Vec<u8>| String::from_utf8(bytes).expect("text");
    (status, text(out), text(err), selected.get())
}

/// A command line as the reference's parser read it: the help, a refusal,
/// or the operation, the engine and each option given.
fn spoken(line: super::cli::Line) -> String {
    match line {
        super::cli::Line::Help => "help".to_owned(),
        super::cli::Line::Refused => "refused".to_owned(),
        super::cli::Line::Asked(asked) => {
            let path = |path: &Option<PathBuf>| {
                let path = path.as_deref();
                path.map(|path| path.to_string_lossy().into_owned())
            };
            format!(
                "{} {} {:?} {:?} {:?} {:?}",
                asked.operation,
                asked.engine.program(),
                path(&asked.output),
                asked.mode,
                asked.image,
                path(&asked.compiled)
            )
        }
    }
}

/// Command lines and what the reference's parser made of each, recorded by
/// running it: the help where it ended with 0, a refusal where it ended with
/// 2. A path it read from nothing is spelled `.` there.
const COMMAND_LINES: &[(&[&str], &str)] = &[
    (&[], "refused"),
    (&["verify"], "verify docker None None None None"),
    (&["verify", "x"], "refused"),
    (&["verify", "--x"], "refused"),
    (&["verify", "-h"], "help"),
    (&["verify", "--"], "refused"),
    (&["verify", "--he"], "help"),
    (&["-h"], "help"),
    (&["--help"], "help"),
    (&["--he"], "help"),
    (&["--h"], "help"),
    (&["--help=x"], "refused"),
    (&["-hx"], "help"),
    (&["-h-"], "refused"),
    (&["-h", "verify"], "help"),
    (&["-x", "verify"], "refused"),
    (&["--", "verify"], "verify docker None None None None"),
    (&["--", "--", "verify"], "refused"),
    (&["-x", "verify", "-h"], "help"),
    (&["bogus"], "refused"),
    (&["ver"], "refused"),
    (&["-1"], "refused"),
    (&["", "verify"], "refused"),
    (&["prepare"], "refused"),
    (
        &["prepare", "--output", "/x"],
        "prepare docker Some(\"/x\") None None None",
    ),
    (
        &["prepare", "--output", "/x", "--output", "/y"],
        "prepare docker Some(\"/y\") None None None",
    ),
    (
        &["prepare", "--output=/x"],
        "prepare docker Some(\"/x\") None None None",
    ),
    (
        &["prepare", "--out", "/x"],
        "prepare docker Some(\"/x\") None None None",
    ),
    (
        &["prepare", "--o", "/x"],
        "prepare docker Some(\"/x\") None None None",
    ),
    (
        &["prepare", "--o=/x"],
        "prepare docker Some(\"/x\") None None None",
    ),
    (
        &["prepare", "--output", ""],
        "prepare docker Some(\"\") None None None",
    ),
    (
        &["prepare", "--output="],
        "prepare docker Some(\"\") None None None",
    ),
    (&["prepare", "--output", "-x"], "refused"),
    (
        &["prepare", "--output", "-1"],
        "prepare docker Some(\"-1\") None None None",
    ),
    (
        &["prepare", "--output", "-.5"],
        "prepare docker Some(\"-.5\") None None None",
    ),
    (
        &["prepare", "--output", "-"],
        "prepare docker Some(\"-\") None None None",
    ),
    (
        &["prepare", "--output", "-a b"],
        "prepare docker Some(\"-a b\") None None None",
    ),
    (&["prepare", "--output", "--engine"], "refused"),
    (&["prepare", "--output"], "refused"),
    (&["prepare", "--output", "--"], "refused"),
    (
        &["prepare", "--output=--"],
        "prepare docker Some(\"--\") None None None",
    ),
    (&["prepare", "--output", "/x", "--", "/y"], "refused"),
    (&["prepare", "--", "--output", "/x"], "refused"),
    (
        &["prepare", "--engine", "podman", "--output", "/x"],
        "prepare podman Some(\"/x\") None None None",
    ),
    (
        &["prepare", "--engine", "docker", "--output", "/x"],
        "prepare docker Some(\"/x\") None None None",
    ),
    (
        &["prepare", "--engine", "nerdctl", "--output", "/x"],
        "refused",
    ),
    (&["prepare", "--engine", "pod", "--output", "/x"], "refused"),
    (
        &["prepare", "--engine=podman", "--output", "/x"],
        "prepare podman Some(\"/x\") None None None",
    ),
    (
        &["prepare", "--e", "podman", "--output", "/x"],
        "prepare podman Some(\"/x\") None None None",
    ),
    (
        &[
            "prepare", "--engine", "podman", "--engine", "docker", "--output", "/x",
        ],
        "prepare docker Some(\"/x\") None None None",
    ),
    (&["prepare", "--mode", "asan", "--output", "/x"], "refused"),
    (&["prepare", "--image", "i", "--output", "/x"], "refused"),
    (&["prepare", "--output", "/x", "extra"], "refused"),
    (&["prepare", "extra", "--output", "/x"], "refused"),
    (&["prepare", "-h"], "help"),
    (&["prepare", "--output", "/x", "-h"], "help"),
    (&["prepare", "extra", "-h"], "help"),
    (&["prepare", "--bogus", "-h"], "help"),
    (&["prepare", "--engine", "x", "-h"], "refused"),
    (&["prepare", "--output", "-h"], "refused"),
    (&["prepare", "-h", "--engine", "x"], "help"),
    (&["prepare", "--help=x"], "refused"),
    (&["prepare", "--he=x"], "refused"),
    (&["prepare", "-h="], "refused"),
    (&["prepare", "-hx"], "help"),
    (&["prepare", "-h-"], "refused"),
    (&["prepare", "-=x", "-h"], "refused"),
    (&["prepare", "--=x", "-h"], "refused"),
    (&["prepare", "-x", "-h"], "help"),
    (&["prepare", "-o", "/x"], "refused"),
    (&["prepare", "--outputs", "/x"], "refused"),
    (&["prepare", "--output", "/x", "--engine"], "refused"),
    (&["prepare", "--output", "/x", "-"], "refused"),
    (&["prepare", "--output", "/x", "-e", "docker"], "refused"),
    (&["prepare", "--output", "/x", "--x=y"], "refused"),
    (
        &["run", "--mode", "asan", "--image", "i", "--output", "/x"],
        "run docker Some(\"/x\") Some(\"asan\") Some(\"i\") None",
    ),
    (
        &["run", "--mode", "ubsan", "--image", "i", "--output", "/x"],
        "run docker Some(\"/x\") Some(\"ubsan\") Some(\"i\") None",
    ),
    (
        &["run", "--mode", "replay", "--image", "i", "--output", "/x"],
        "run docker Some(\"/x\") Some(\"replay\") Some(\"i\") None",
    ),
    (
        &[
            "run",
            "--mode",
            "asan",
            "--image",
            "i",
            "--output",
            "/x",
            "--compiled-directory",
            "/c",
        ],
        "run docker Some(\"/x\") Some(\"asan\") Some(\"i\") Some(\"/c\")",
    ),
    (
        &[
            "run",
            "--mode",
            "asan",
            "--image",
            "i",
            "--output",
            "/x",
            "--compiled",
            "/c",
        ],
        "run docker Some(\"/x\") Some(\"asan\") Some(\"i\") Some(\"/c\")",
    ),
    (
        &[
            "run", "--mode", "asan", "--image", "i", "--output", "/x", "--c", "/c",
        ],
        "run docker Some(\"/x\") Some(\"asan\") Some(\"i\") Some(\"/c\")",
    ),
    (
        &[
            "run", "--m", "asan", "--i", "i", "--o", "/x", "--e", "podman",
        ],
        "run podman Some(\"/x\") Some(\"asan\") Some(\"i\") None",
    ),
    (
        &[
            "run",
            "--mode=asan",
            "--image=i",
            "--output=/x",
            "--engine=podman",
            "--compiled-directory=/c",
        ],
        "run podman Some(\"/x\") Some(\"asan\") Some(\"i\") Some(\"/c\")",
    ),
    (
        &["run", "--mode", "bogus", "--image", "i", "--output", "/x"],
        "refused",
    ),
    (
        &["run", "--mode", "ASAN", "--image", "i", "--output", "/x"],
        "refused",
    ),
    (&["run", "--mode", "asan", "--output", "/x"], "refused"),
    (&["run", "--image", "i", "--output", "/x"], "refused"),
    (&["run", "--mode", "asan", "--image", "i"], "refused"),
    (
        &["run", "--mode", "asan", "--image", "", "--output", "/x"],
        "run docker Some(\"/x\") Some(\"asan\") Some(\"\") None",
    ),
    (
        &[
            "run",
            "--mode",
            "asan",
            "--image",
            "i",
            "--output",
            "/x",
            "--compiled-directory",
            "",
        ],
        "run docker Some(\"/x\") Some(\"asan\") Some(\"i\") Some(\"\")",
    ),
    (
        &[
            "run", "--mode", "asan", "--mode", "ubsan", "--image", "i", "--output", "/x",
        ],
        "run docker Some(\"/x\") Some(\"ubsan\") Some(\"i\") None",
    ),
    (
        &["run", "--image", "-x", "--mode", "asan", "--output", "/x"],
        "refused",
    ),
    (
        &["run", "--image", "-x y", "--mode", "asan", "--output", "/x"],
        "run docker Some(\"/x\") Some(\"asan\") Some(\"-x y\") None",
    ),
    (
        &[
            "run",
            "--mode",
            "asan",
            "--image",
            "i",
            "--output",
            "/x",
            "--compiled-directory",
        ],
        "refused",
    ),
    (
        &[
            "run", "--mode", "asan", "--image", "i", "--output", "/x", "x",
        ],
        "refused",
    ),
    (&["run", "--mode", "bogus", "-h"], "refused"),
    (&["run", "-h", "--mode", "bogus"], "help"),
    (
        &[
            "compile", "--mode", "asan", "--image", "i", "--output", "/x",
        ],
        "refused",
    ),
    (
        &[
            "compile",
            "--mode",
            "asan",
            "--image",
            "i",
            "--output",
            "/x",
            "--compiled-directory",
            "/c",
        ],
        "compile docker Some(\"/x\") Some(\"asan\") Some(\"i\") Some(\"/c\")",
    ),
    (
        &[
            "compile",
            "--mode",
            "replay",
            "--image",
            "i",
            "--output",
            "/x",
            "--compiled-directory",
            "/c",
            "--engine",
            "podman",
        ],
        "compile podman Some(\"/x\") Some(\"replay\") Some(\"i\") Some(\"/c\")",
    ),
    (
        &[
            "compile",
            "--mode",
            "asan",
            "--image",
            "i",
            "--compiled-directory",
            "/c",
        ],
        "refused",
    ),
    (
        &[
            "compile",
            "--mode",
            "asan",
            "--output",
            "/x",
            "--compiled-directory",
            "/c",
        ],
        "refused",
    ),
    (
        &[
            "compile",
            "--image",
            "i",
            "--output",
            "/x",
            "--compiled-directory",
            "/c",
        ],
        "refused",
    ),
    (
        &[
            "compile",
            "--mode",
            "asan",
            "--image",
            "i",
            "--output",
            "/x",
            "--compiled-directory",
            "/c",
            "--compiled-directory",
            "/d",
        ],
        "compile docker Some(\"/x\") Some(\"asan\") Some(\"i\") Some(\"/d\")",
    ),
    (
        &[
            "compile",
            "--mode",
            "asan",
            "--image",
            "i",
            "--output",
            "/x",
            "--compiled-directory",
            "/c",
            "--engine",
            "x",
        ],
        "refused",
    ),
    (
        &[
            "compile",
            "--mode",
            "asan",
            "--image",
            "i",
            "--output",
            "/x",
            "--compiled-directory",
            "/c",
            "-h",
        ],
        "help",
    ),
];

#[test]
fn every_command_line_is_read_as_the_references_parser_read_it() {
    for (arguments, wanted) in COMMAND_LINES {
        let line = super::cli::parsed(&words(arguments));
        assert_eq!(spoken(line), *wanted, "{arguments:?}");
    }
    // The reference's own operations for its worker in the sandbox are none
    // here: nothing of this repository's runs in the image.
    for arguments in [
        &["_hold"][..],
        &[
            "_receive",
            "--compiled-sha256",
            "x",
            "--compiled-bytes",
            "1",
        ],
        &["_worker", "--mode", "asan"],
        &["_compile", "--mode", "asan"],
    ] {
        let line = super::cli::parsed(&words(arguments));
        assert_eq!(line, super::cli::Line::Refused, "{arguments:?}");
    }
}

#[test]
fn the_command_refuses_an_automated_run_before_its_engine_or_its_output() {
    let bench = Bench::new("automated");
    for marker in ["CI", "GITHUB_ACTIONS", "ACT"] {
        let output = bench.scratch.0.join(format!("automated-{marker}"));
        let path = output.to_string_lossy().into_owned();
        let mut engine = bench.engine();
        let arguments = ["prepare", "--output", path.as_str()];
        let variables = [(marker, "")];
        let commanded = commanded(
            &bench.root,
            &arguments,
            &variables,
            Some(&mut engine),
            Ok(()),
        );
        let (status, out, err, selected) = commanded;
        assert_eq!((status, out.as_str(), selected), (1, "", None), "{marker}");
        assert_eq!(err, failure("vendor_source_scan_local_only"), "{marker}");
        assert!(output.symlink_metadata().is_err(), "{marker}");
        assert!(engine.asked.is_empty(), "{marker}");
    }
}

#[test]
fn an_existing_evidence_directory_is_refused_with_nothing_written_into_it() {
    let bench = Bench::new("existing");
    let output = bench.directory("existing");
    let path = output.to_string_lossy().into_owned();
    let mut engine = bench.engine();
    let arguments = ["prepare", "--output", path.as_str()];
    let commanded_here = commanded(&bench.root, &arguments, &[], Some(&mut engine), Ok(()));
    let (status, out, err, selected) = commanded_here;
    assert_eq!((status, out.as_str(), selected), (1, "", None));
    assert_eq!(err, failure(UNUSABLE));
    assert!(begun(&output, "").is_empty());
    // A directory not named whole is never made.
    let relative = format!("native-suites-relative-{}", std::process::id());
    let arguments = ["prepare", "--output", relative.as_str()];
    let (status, _, err, selected) = commanded(&bench.root, &arguments, &[], None, Ok(()));
    assert_eq!((status, selected), (1, None));
    assert_eq!(err, failure("absolute_output_required"));
    assert!(Path::new(&relative).symlink_metadata().is_err());
    // An operation that may not begin makes nothing either.
    let output = bench.scratch.0.join("unready");
    let path = output.to_string_lossy().into_owned();
    let arguments = ["prepare", "--output", path.as_str()];
    let unready = Err(Fault("image_signal_unavailable"));
    let (status, _, err, selected) = commanded(&bench.root, &arguments, &[], None, unready);
    assert_eq!((status, selected), (1, None));
    assert_eq!(err, failure("image_signal_unavailable"));
    assert!(output.symlink_metadata().is_err());
}

#[test]
fn a_commands_answer_is_kept_in_its_evidence_and_printed_on_one_line() {
    let bench = Bench::new("answered");
    let output = bench.scratch.0.join("answered");
    let path = output.to_string_lossy().into_owned();
    let mut engine = bench.engine();
    let arguments = ["prepare", "--engine", "podman", "--output", path.as_str()];
    let variables = [("CIRCLE", "x"), ("PATH", "/nowhere")];
    let commanded = commanded(
        &bench.root,
        &arguments,
        &variables,
        Some(&mut engine),
        Ok(()),
    );
    let (status, out, err, selected) = commanded;
    let podman = Some(crate::engine::Name::Podman);
    assert_eq!((status, err.as_str(), selected), (0, "", podman));
    let printed = format!(
        concat!(
            "{{\"architecture\": \"amd64\", \"imageId\": \"{}\", ",
            "\"inputsSha256\": \"{}\", \"schemaVersion\": 1, \"status\": \"passed\"}}\n"
        ),
        IMAGE, bench.inputs
    );
    assert_eq!(out, printed);
    let report = fs::read_to_string(output.join("report.json")).expect("a report");
    let kept = format!(
        concat!(
            "{{\n  \"architecture\": \"amd64\",\n  \"imageId\": \"{}\",\n",
            "  \"inputsSha256\": \"{}\",\n  \"schemaVersion\": 1,\n",
            "  \"status\": \"passed\"\n}}\n"
        ),
        IMAGE, bench.inputs
    );
    assert_eq!(report, kept);
    let mode = fs::metadata(&output)
        .expect("evidence")
        .permissions()
        .mode();
    assert_eq!(mode & 0o077, 0);
    assert!(!engine.asked.is_empty());
}

#[test]
fn a_failure_is_reported_where_this_run_made_its_evidence_and_never_over_a_report() {
    let bench = Bench::new("reported");
    let output = bench.scratch.0.join("unselected");
    let path = output.to_string_lossy().into_owned();
    let arguments = ["prepare", "--output", path.as_str()];
    let (status, out, err, selected) = commanded(&bench.root, &arguments, &[], None, Ok(()));
    let docker = Some(crate::engine::Name::Docker);
    assert_eq!((status, out.as_str(), selected), (1, "", docker));
    assert_eq!(err, failure("engine_missing"));
    let report = fs::read_to_string(output.join("report.json")).expect("a report");
    let failed = concat!(
        "{\n  \"error\": \"engine_missing\",\n  \"schemaVersion\": 1,\n",
        "  \"status\": \"failed\"\n}\n"
    );
    assert_eq!(report, failed);
    assert_eq!(begun(&output, ""), ["report.json"]);
    // A report that is there already, which the operation could not write
    // over either, is kept as it is.
    let output = bench.scratch.0.join("written");
    let path = output.to_string_lossy().into_owned();
    let arguments = ["prepare", "--output", path.as_str()];
    let mut engine = bench.engine();
    engine.tampered = Some(("build", output.join("report.json"), b"{}\n".to_vec()));
    let commanded = commanded(&bench.root, &arguments, &[], Some(&mut engine), Ok(()));
    let (status, out, err, _) = commanded;
    assert_eq!((status, out.as_str()), (1, ""));
    assert_eq!(err, failure(UNUSABLE));
    let report = fs::read(output.join("report.json")).expect("a report");
    assert_eq!(report, b"{}\n");
}

#[test]
fn usage_help_and_verification_answer_without_an_engine() {
    let bench = Bench::new("usage");
    let never = || Err(Fault("never_asked"));
    let (status, out, err, selected) = commanded(&bench.root, &["--help"], &[], None, never());
    assert_eq!((status, err.as_str(), selected), (0, "", None));
    assert!(out.starts_with("usage: simplestchat-security native-suites verify\n"));
    let (status, out, err, _) = commanded(&bench.root, &["prepare"], &[], None, never());
    assert_eq!((status, out.as_str()), (2, ""));
    assert!(err.starts_with("usage: simplestchat-security native-suites verify\n"));
    let variables = [("CI", "true")];
    let (status, out, err, selected) =
        commanded(&bench.root, &["verify"], &variables, None, never());
    assert_eq!((status, err.as_str(), selected), (0, "", None));
    let verified = concat!(
        "{\"status\": \"passed\", \"corpusCases\": 17, \"families\": ",
        "[\"stun\", \"dtls\", \"sctp\", \"rtp\", \"rtcp\", \"codecs\", \"utils\"]}\n"
    );
    assert_eq!(out, verified);
    let unreviewed = bench
        .root
        .join("security/native/corpus/stun/unreviewed.hex");
    fs::write(unreviewed, "00\n").expect("an input");
    let (status, out, err, _) = commanded(&bench.root, &["verify"], &[], None, never());
    assert_eq!((status, out.as_str()), (1, ""));
    assert_eq!(err, failure("corpus_unlisted_file"));
}

#[test]
fn every_operation_is_asked_of_the_engine_with_the_options_given() {
    let bench = Bench::new("operations");
    let evidence = |name: &str| {
        let output = bench.scratch.0.join(name);
        (output.to_string_lossy().into_owned(), output)
    };
    let report = |output: &Path| {
        let report = fs::read(output.join("report.json")).expect("a report");
        Document::read(&report).expect("a report")
    };
    // A run of a fresh build.
    let (path, output) = evidence("ran");
    let mut engine = bench.engine();
    let arguments = [
        "run", "--mode", "ubsan", "--image", IMAGE, "--output", &path,
    ];
    let commanded_here = commanded(&bench.root, &arguments, &[], Some(&mut engine), Ok(()));
    let (status, out, err, selected) = commanded_here;
    let docker = Some(crate::engine::Name::Docker);
    assert_eq!((status, err.as_str(), selected), (0, "", docker));
    let ran = report(&output);
    assert_eq!(out, format!("{}\n", ran.clone().sorted().spaced()));
    assert_eq!(text(&ran, "mode"), "ubsan");
    assert_eq!(text(&ran, "runId"), RUN_ID);
    assert_eq!(ran.get("compiledReused"), Some(&Document::Bool(false)));
    assert!(asked(&engine, "mediasoup-worker-test-asan-undefined"));
    // A build kept, and a run of it.
    let (path, output) = evidence("compiled");
    let (kept, directory) = evidence("kept");
    let mut engine = bench.engine();
    let arguments = [
        "compile",
        "--mode",
        "replay",
        "--image",
        IMAGE,
        "--output",
        &path,
        "--compiled-directory",
        &kept,
    ];
    let commanded_here = commanded(&bench.root, &arguments, &[], Some(&mut engine), Ok(()));
    assert_eq!(commanded_here.0, 0, "{}", commanded_here.2);
    assert_eq!(text(&report(&output), "mode"), "replay");
    assert!(directory.join("receipt.json").is_file());
    let (path, output) = evidence("reused");
    let mut engine = bench.engine();
    let arguments = [
        "run",
        "--mode",
        "replay",
        "--image",
        IMAGE,
        "--output",
        &path,
        "--compiled-directory",
        &kept,
    ];
    let commanded_here = commanded(&bench.root, &arguments, &[], Some(&mut engine), Ok(()));
    assert_eq!(commanded_here.0, 0, "{}", commanded_here.2);
    let reused = report(&output);
    assert_eq!(reused.get("compiledReused"), Some(&Document::Bool(true)));
    assert!(!asked(&engine, MESON));
    // An image named otherwise than by its identity.
    let (path, output) = evidence("named");
    let mut engine = bench.engine();
    let arguments = [
        "run",
        "--mode",
        "asan",
        "--image",
        "builder:latest",
        "--output",
        &path,
    ];
    let commanded_here = commanded(&bench.root, &arguments, &[], Some(&mut engine), Ok(()));
    assert_eq!(commanded_here.0, 1);
    assert_eq!(commanded_here.2, failure("immutable_image_required"));
    assert_eq!(text(&report(&output), "error"), "immutable_image_required");
}

#[test]
fn the_engine_is_selected_and_asked_as_the_reference_asked_it() {
    use std::ffi::OsString;
    // The selection's own questions: two mebibytes of either stream, and a
    // status of 0.
    for (script, outcome) in [
        ("head -c 2097152 /dev/zero", Ok(2 << 20)),
        (
            "head -c 2097153 /dev/zero",
            Err(Fault("command_stdout_limit_exceeded")),
        ),
        ("head -c 2097152 /dev/zero >&2", Ok(0)),
        (
            "head -c 2097153 /dev/zero >&2",
            Err(Fault("command_stderr_limit_exceeded")),
        ),
        ("exit 3", Err(Fault("command_failed"))),
    ] {
        let arguments = ["/bin/sh", "-c", script].map(OsString::from);
        let asked = super::cli::selecting(&arguments).map(|printed| printed.len());
        assert_eq!(asked, outcome, "{script}");
    }
    assert_eq!(super::cli::selecting(&[]), Err(Fault("engine_missing")));
    // Docker found on the search path, and reached through the host's own
    // socket or else the caller's.
    let scratch = Scratch::new("selected");
    let programs = scratch.0.join("bin");
    fs::create_dir(&programs).expect("a directory");
    symlink("/bin/echo", programs.join("docker")).expect("a program");
    let home = scratch.0.join("home");
    fs::create_dir_all(home.join(".docker/run")).expect("a home");
    fs::write(home.join(".docker/run/docker.sock"), b"").expect("a socket");
    let running = || false;
    let search = Some(programs.as_os_str());
    let docker = crate::engine::Name::Docker;
    let engine = super::cli::selected(docker, search, Some(home.clone()), &running);
    let mut engine = engine.expect("an engine");
    let printed = engine.ask(&words(&["ps"]), secs(5)).expect("an answer");
    let host = Path::new("/var/run/docker.sock");
    let socket = if host.exists() {
        host.to_path_buf()
    } else {
        home.join(".docker/run/docker.sock")
    };
    let wanted = format!("--host unix://{} ps\n", socket.display());
    assert_eq!(String::from_utf8(printed).expect("text"), wanted);
    // A program that may not be run is none.
    let unrunnable = scratch.0.join("unrunnable");
    fs::create_dir(&unrunnable).expect("a directory");
    fs::write(unrunnable.join("docker"), b"").expect("a file");
    let private = fs::Permissions::from_mode(0o644);
    fs::set_permissions(unrunnable.join("docker"), private).expect("a mode");
    let search = Some(unrunnable.as_os_str());
    let missing = super::cli::selected(docker, search, Some(home), &running);
    assert_eq!(missing.err(), Some(Fault("engine_missing")));
    // A run's identity: thirty-two small hexadecimal digits, new each time.
    let one = super::cli::run_identity().expect("an identity");
    let two = super::cli::run_identity().expect("an identity");
    assert!(super::hexadecimal(&one, 32) && super::hexadecimal(&two, 32) && one != two);
}

#[test]
fn the_local_engine_bounds_each_command_and_keeps_only_its_own_logs() {
    use super::engine::Engine as _;
    use std::ffi::OsString;
    let running = || false;
    let shell = |script: &str| super::local::Local {
        prefix: ["/bin/sh", "-c", script, "sh"].map(OsString::from).to_vec(),
        cancelled: &running,
    };
    // A question's answer in at most sixteen mebibytes, and its complaints
    // in one, whether or not it removes.
    for (script, outcome) in [
        ("head -c 16777216 /dev/zero", Ok(16 << 20)),
        (
            "head -c 16777217 /dev/zero",
            Err(Fault("command_stdout_limit_exceeded")),
        ),
        ("head -c 1048576 /dev/zero >&2", Ok(0)),
        (
            "head -c 1048577 /dev/zero >&2",
            Err(Fault("command_stderr_limit_exceeded")),
        ),
        ("sleep 5", Err(Fault("command_timed_out"))),
    ] {
        let within = Duration::from_millis(if script == "sleep 5" { 300 } else { 30_000 });
        let asked = shell(script).ask(&[], within).map(|printed| printed.len());
        assert_eq!(asked, outcome, "{script}");
        let removed = shell(script)
            .removing(&[], within)
            .map(|printed| printed.len());
        assert_eq!(removed, outcome, "{script}");
    }
    let mut engine = super::local::Local {
        prefix: Vec::new(),
        cancelled: &running,
    };
    assert_eq!(engine.ask(&[], secs(5)), Err(Fault("engine_missing")));
    // A command reads what it is given within its bound.
    let scratch = Scratch::new("logs");
    let logs = |name: &str| {
        let output = scratch.0.join(format!("{name}.stdout.log"));
        let error = scratch.0.join(format!("{name}.stderr.log"));
        fs::write(&output, b"").expect("a log");
        fs::write(&error, b"").expect("a log");
        (output, error)
    };
    let (output, error) = logs("given");
    let ran = shell("cat").run(&[], b"given", secs(10), (&output, &error), (10, 10));
    assert_eq!(ran, Ok(0));
    assert_eq!(fs::read(&output).expect("a log"), b"given");
    // A later command's streams follow an earlier one's in the same logs.
    let ran =
        shell("printf more; printf less >&2").run(&[], &[], secs(10), (&output, &error), (10, 10));
    assert_eq!(ran, Ok(0));
    assert_eq!(fs::read(&output).expect("a log"), b"givenmore");
    let ran = shell("printf again >&2").run(&[], &[], secs(10), (&output, &error), (10, 10));
    assert_eq!(ran, Ok(0));
    assert_eq!(fs::read(&error).expect("a log"), b"lessagain");
    let (output, error) = logs("slow");
    let slow = Duration::from_millis(300);
    let ran = shell("sleep 5").run(&[], &[], slow, (&output, &error), (10, 10));
    assert_eq!(ran, Err(Fault("command_timed_out")));
    // Only a log that is there, and not through a link, is kept.
    let (real, other) = logs("real");
    let link = scratch.0.join("link.log");
    symlink(&real, &link).expect("a link");
    let absent = scratch.0.join("absent.log");
    for (index, kept) in [
        (&absent, &other),
        (&other, &absent),
        (&link, &other),
        (&other, &link),
    ]
    .into_iter()
    .enumerate()
    {
        let kept = (kept.0.as_path(), kept.1.as_path());
        let ran = shell("printf x").run(&[], &[], secs(5), kept, (10, 10));
        assert_eq!(ran, Err(Fault("native_log_unwritable")), "{index}");
    }
    assert_eq!(fs::read(&real).expect("a log"), b"");
    // A command is stopped once the run is cancelled while it runs.
    let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let cancelled = || flag.load(std::sync::atomic::Ordering::SeqCst);
    let mut engine = super::local::Local {
        prefix: ["/bin/sleep", "20"].map(OsString::from).to_vec(),
        cancelled: &cancelled,
    };
    let setter = std::sync::Arc::clone(&flag);
    let cancelling = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        setter.store(true, std::sync::atomic::Ordering::SeqCst);
    });
    let started = std::time::Instant::now();
    let ran = engine.run(&[], &[], secs(20), (&real, &other), (10, 10));
    cancelling.join().expect("a cancellation");
    assert_eq!(ran, Err(Fault("command_lease_lost")));
    assert!(started.elapsed() < secs(15));
}

/// Whether `attempt` at the file `path` is refused as `wanted` within five
/// seconds of tries while another thread keeps changing the file with
/// `change`. Every other try must pass.
fn raced(
    path: &Path,
    mut change: impl FnMut(&fs::File) + Send + 'static,
    attempt: impl Fn(&Path) -> crate::Result<()>,
    wanted: &str,
) -> bool {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    let stop = Arc::new(AtomicBool::new(false));
    let file = fs::OpenOptions::new()
        .write(true)
        .open(path)
        .expect("a file");
    let stopping = Arc::clone(&stop);
    let writer = std::thread::spawn(move || {
        while !stopping.load(Ordering::SeqCst) {
            change(&file);
        }
    });
    let started = std::time::Instant::now();
    let mut refused = false;
    while !refused && started.elapsed() < secs(5) {
        match attempt(path) {
            Ok(()) => {}
            Err(Fault(code)) => {
                assert_eq!(code, wanted);
                refused = true;
            }
        }
    }
    stop.store(true, Ordering::SeqCst);
    writer.join().expect("a writer");
    refused
}

#[test]
fn a_file_that_changes_while_it_is_read_is_refused_as_the_reference_refused_it() {
    use std::os::unix::fs::FileExt as _;
    let scratch = Scratch::new("changing");
    // An input whose size changes while it is read.
    let growing = scratch.0.join("growing");
    fs::write(&growing, vec![b'x'; 1 << 20]).expect("a file");
    let mut wider = false;
    let resize = move |file: &fs::File| {
        wider = !wider;
        let size = (1 << 20) + if wider { 4096 } else { 0 };
        file.set_len(size).expect("a size");
    };
    let read = |path: &Path| super::read_regular(path, 16 << 20).map(drop);
    assert!(raced(&growing, resize, read, "input_changed"));
    // An executable that changes while it is hashed, its size kept.
    let binary = scratch.0.join("binary");
    fs::write(&binary, executable(62, 4 << 20)).expect("a binary");
    let touch = |file: &fs::File| {
        file.write_at(b"z", 1000).expect("a change");
    };
    let identified = |path: &Path| super::compiled::binary_identity(path, "amd64").map(drop);
    assert!(raced(&binary, touch, identified, "compiled_binary_changed"));
}
