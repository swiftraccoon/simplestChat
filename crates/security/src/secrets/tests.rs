//! Replays of the recorded projections and neutral snapshots.
//!
//! `recorded_secrets.json` holds what `build/security_secret_projection.py`
//! and `build/security_secrets.py` answered for files projected one at a time
//! and for whole snapshots with their manifests and gitleaks configurations.
//! Where the reference failed with an exception of its interpreter, `named`
//! gives the code here. A configuration the reference's own configuration
//! check refuses is refused here with that check's code, whatever the
//! reference's translation made of it; `DIFFERENT` names the rest of what is
//! answered differently on purpose.

use super::{
    FORMAT, MANIFEST, RESERVE, free, identity, neutral_snapshot, prefix, project, unchanged,
};
use crate::{
    Fault,
    context::Limits,
    digest,
    record::pretty,
    scratch::{Scratch, base64},
};
use serde_json::{Map, Value, json};
use std::{
    io::Write as _,
    os::unix::fs::PermissionsExt,
    path::Path,
    process::Command,
    sync::atomic::{AtomicBool, Ordering},
};

const RECORDED: &str = include_str!("recorded_secrets.json");

/// A fixture's path is its file's literal path between its anchors, and a
/// manifest names canonical paths: the reference searched with any pattern
/// and joined any name to the snapshot.
const DIFFERENT: [(&str, &str); 15] = [
    (
        "a manifest with a name that is no canonical path",
        "security_source_path",
    ),
    ("a path without its end anchor", "gitleaks_fixture_path"),
    ("a path without anchors", "gitleaks_fixture_path"),
    ("a path with an unescaped point", "gitleaks_fixture_path"),
    (
        "a path with an unescaped point that finds another file",
        "gitleaks_fixture_path",
    ),
    (
        "a path that is a wildcard over one file",
        "gitleaks_fixture_path",
    ),
    (
        "a path that is a wildcard over two files",
        "gitleaks_fixture_path",
    ),
    ("a path that is an alternation", "gitleaks_fixture_path"),
    ("a path without its start anchor", "gitleaks_fixture_path"),
    (
        "a path with a class after a backslash",
        "gitleaks_fixture_path",
    ),
    ("a path that names nothing", "gitleaks_fixture_path"),
    ("a path that is no pattern", "gitleaks_fixture_path"),
    ("a path that is empty", "gitleaks_fixture_path"),
    ("a path that is a number", "gitleaks_broad_allowlist"),
    ("a target that is a list", "gitleaks_broad_allowlist"),
];

/// The translation is written in the order of the reviewed file's keys: the
/// reference kept whatever order the configuration had.
const REORDERED: &str = "an allowlist with its keys in another order";

fn recorded() -> Value {
    serde_json::from_str(RECORDED).expect("the recording is JSON")
}

fn text(value: &Value) -> &str {
    value.as_str().expect("a recorded string")
}

fn list(value: &Value) -> &[Value] {
    value.as_array().expect("a recorded list")
}

fn mode(path: &Path) -> u32 {
    let metadata = path.symlink_metadata().expect("an entry");
    metadata.permissions().mode() & 0o777
}

/// The bytes of a recorded file: text, base64, or a unit repeated.
fn content(spec: &Value) -> Vec<u8> {
    if let Some(text) = spec.as_str() {
        return text.as_bytes().to_vec();
    }
    if let Some(encoded) = spec.get("base64") {
        return base64(text(encoded));
    }
    let times = usize::try_from(spec["times"].as_u64().expect("a count"));
    base64(text(&spec["unit"])).repeat(times.expect("a small count"))
}

fn named<'a>(code: &'a str, stage: &str) -> &'a str {
    match (code, stage) {
        ("FileNotFoundError", _) => "tool_entry_missing",
        ("IsADirectoryError", _) => "secret_projection_source_kind",
        ("FileExistsError" | "OSError", _) => "tool_entry_unavailable",
        ("JSONDecodeError" | "UnicodeDecodeError", "manifest") => "invalid_tool_json",
        ("TOMLDecodeError" | "UnicodeDecodeError", _) => "invalid_gitleaks_configuration",
        (code, _) => code,
    }
}

/// Counts of the cases answered alike, refused alike and answered differently
/// on purpose.
#[derive(Debug, Default, PartialEq, Eq)]
struct Tally(usize, usize, usize);

#[test]
fn the_prefix_and_the_reserve_are_the_reference_ones() {
    let recording = recorded();
    assert_eq!(json!(prefix().len()), recording["prefixBytes"]);
    assert_eq!(json!(digest(&prefix())), recording["prefixSha256"]);
    assert_eq!(json!(RESERVE), recording["reserve"]);
}

#[test]
fn every_recorded_projection_is_made_alike() {
    let scratch = Scratch::new("sp");
    let mut tally = Tally::default();
    for (index, case) in list(&recorded()["projections"]).iter().enumerate() {
        let name = text(&case["name"]);
        let base = scratch.0.join(index.to_string());
        std::fs::create_dir(&base).expect("a directory");
        let source = base.join("source");
        match text(&case["kind"]) {
            "file" => std::fs::write(&source, content(&case["content"])).expect("a file"),
            "link" => {
                std::fs::write(base.join("real"), content(&case["content"])).expect("a file");
                std::os::unix::fs::symlink("real", &source).expect("a link");
            }
            "dangling" => std::os::unix::fs::symlink("nowhere", &source).expect("a link"),
            "directory" => std::fs::create_dir(&source).expect("a directory"),
            "fifo" => {
                let made = Command::new("mkfifo").arg(&source).status();
                assert!(made.expect("mkfifo ran").success());
            }
            _ => {}
        }
        let mut target = base.join("target");
        match text(&case["destination"]) {
            "exists" => std::fs::write(&target, "old").expect("an old file"),
            "link" => std::os::unix::fs::symlink("elsewhere", &target).expect("a link"),
            "orphan" => target = base.join("missing").join("target"),
            _ => {}
        }
        let limit = case["limit"].as_u64().expect("a limit");
        let made = project(&source, &target, limit);
        let reference = &case["result"];
        if let Some(code) = reference.get("code") {
            let refused = made.err().map(|fault| fault.0);
            assert_eq!(refused, Some(named(text(code), "")), "{name}");
            if case["destination"] == "new" {
                assert_eq!(target.exists(), !case["left"].is_null(), "{name}");
            }
            tally.1 += 1;
            continue;
        }
        let made = made.expect(name);
        let written = std::fs::read(&target).expect("a projection");
        let tail = &written[prefix().len()..];
        let tail = String::from_utf8(tail[..tail.len().min(96)].to_vec());
        let answered = json!({
            "sourceSha256": made.source_sha256,
            "sourceBytes": made.source_bytes,
            "projectionSha256": made.projection_sha256,
            "projectionBytes": made.projection_bytes,
            "format": FORMAT,
            "writtenSha256": digest(&written),
            "tail": tail.expect("a projection that is ASCII"),
            "mode": mode(&target),
        });
        assert_eq!(&answered, reference, "{name}");
        assert_eq!(made.projection_sha256, digest(&written), "{name}");
        tally.0 += 1;
    }
    assert_eq!(tally, Tally(18, 13, 0));
}

#[test]
fn a_file_is_what_it_was_only_with_every_byte_and_one_identity() {
    let scratch = Scratch::new("su");
    let (one, other) = (scratch.0.join("one"), scratch.0.join("other"));
    std::fs::write(&one, "four").expect("a file");
    std::fs::write(&other, "four").expect("another file");
    let before = one.metadata().expect("metadata");
    let apart = other.metadata().expect("metadata");
    assert!(unchanged(4, &before, &before, &before));
    assert!(!unchanged(3, &before, &before, &before));
    assert!(!unchanged(5, &before, &before, &before));
    assert!(!unchanged(4, &before, &apart, &before));
    assert!(!unchanged(4, &before, &before, &apart));
    let mut longer = std::fs::OpenOptions::new();
    let mut longer = longer.append(true).open(&one).expect("the file again");
    longer.write_all(b"!").expect("one more byte");
    let after = one.metadata().expect("metadata");
    assert!(!unchanged(4, &before, &after, &before));
    assert!(!unchanged(4, &before, &before, &after));
}

#[test]
fn a_file_that_grows_while_it_is_read_is_refused() {
    let scratch = Scratch::new("sg");
    let source = scratch.0.join("source");
    std::fs::write(&source, vec![b'x'; 4 * 1024 * 1024]).expect("a file");
    let (stop, started) = (AtomicBool::new(false), AtomicBool::new(false));
    let mut refusals = Vec::new();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            let mut options = std::fs::OpenOptions::new();
            let mut growing = options.append(true).open(&source).expect("the file");
            while !stop.load(Ordering::SeqCst) {
                growing.write_all(b"y").expect("one more byte");
                started.store(true, Ordering::SeqCst);
            }
        });
        while !started.load(Ordering::SeqCst) {
            std::thread::yield_now();
        }
        for attempt in 0..20 {
            let target = scratch.0.join(format!("target{attempt}"));
            refusals.push(project(&source, &target, u64::MAX / 2).err());
        }
        stop.store(true, Ordering::SeqCst);
    });
    let changed = Some(Fault("secret_projection_source_changed"));
    assert!(refusals.contains(&changed), "{refusals:?}");
    assert!(
        refusals
            .iter()
            .all(|refusal| refusal.is_none() || *refusal == changed)
    );
}

#[test]
fn the_identity_of_a_file_is_its_place_its_size_and_both_times() {
    let scratch = Scratch::new("si");
    let file = scratch.0.join("file");
    std::fs::write(&file, "content").expect("a file");
    let epoch = std::time::SystemTime::UNIX_EPOCH;
    let earlier = std::fs::FileTimes::new()
        .set_modified(epoch + std::time::Duration::new(1_000_000, 7))
        .set_accessed(epoch + std::time::Duration::new(2_000_000, 9));
    let opened = std::fs::File::options()
        .write(true)
        .open(&file)
        .expect("the file");
    opened.set_times(earlier).expect("earlier times");
    let seen = rustix::fs::lstat(&file).expect("the file's state");
    let whole = |value: i128| value;
    let times = [
        whole(seen.st_mtime.into()),
        whole(seen.st_mtime_nsec.into()),
        whole(seen.st_ctime.into()),
        whole(seen.st_ctime_nsec.into()),
    ];
    let (device, node, size, held) = identity(&file.metadata().expect("metadata"));
    assert_eq!(whole(device.into()), whole(seen.st_dev.into()));
    assert_eq!(whole(node.into()), whole(seen.st_ino.into()));
    assert_eq!((size, whole(seen.st_size.into())), (7, 7));
    assert_eq!(held.map(i128::from), times);
    assert_eq!((times[0], times[1]), (1_000_000, 7));
    assert_ne!((times[2], times[3]), (times[0], times[1]));
}

#[test]
fn the_room_left_is_that_of_the_filesystem() {
    let scratch = Scratch::new("sf");
    let room = free(&scratch.0).expect("room");
    let asked = Command::new("df").arg("-Pk").arg(&scratch.0).output();
    let answer = String::from_utf8(asked.expect("df ran").stdout).expect("text");
    let row = answer.lines().nth(1).expect("the row of the filesystem");
    let available: u64 = row
        .split_whitespace()
        .nth(3)
        .and_then(|blocks| blocks.parse().ok())
        .expect("the available blocks");
    let listed = available * 1024;
    assert!(
        room > listed / 2 && room / 2 < listed,
        "{room} beside {listed}"
    );
    let missing = free(&scratch.0.join("missing"));
    assert_eq!(missing, Err(Fault("tool_entry_missing")));
}

#[test]
fn a_manifest_has_at_most_four_mebibytes() {
    let scratch = Scratch::new("sm");
    let room = free(&scratch.0).expect("room");
    for (over, refusal) in [(0, None), (1, Some(Fault("tool_file_size")))] {
        let base = scratch.0.join(over.to_string());
        let (source, output) = (base.join("source"), base.join("output"));
        std::fs::create_dir_all(&source).expect("a source directory");
        std::fs::create_dir(&output).expect("an output directory");
        std::fs::write(source.join("a"), "data").expect("a file");
        let mut manifest = format!("{{\"a\": \"{}\"}}", digest(b"data")).into_bytes();
        manifest.resize(usize::try_from(MANIFEST).expect("a size") + over, b' ');
        std::fs::write(output.join("source-manifest.json"), manifest).expect("a manifest");
        let policy = base.join("gitleaks.toml");
        let plain = "title = 'Fixture'\nallowlists = []\n[extend]\nuseDefault = true\n";
        std::fs::write(&policy, plain).expect("a configuration");
        let made = neutral_snapshot(&output, &source, &policy, false, &Limits::REVIEWED, room);
        assert_eq!(made.err(), refusal, "{over}");
    }
    assert_eq!(MANIFEST, 4 * 1024 * 1024);
}

/// Build a recorded snapshot below `base` and answer its three paths.
fn built(base: &Path, case: &Value) -> [std::path::PathBuf; 3] {
    let (source, output) = (base.join("source"), base.join("output"));
    std::fs::create_dir_all(&source).expect("a source directory");
    std::fs::create_dir(&output).expect("an output directory");
    let mut identities = Map::new();
    for (name, spec) in case["files"].as_object().expect("files") {
        let target = source.join(name);
        std::fs::create_dir_all(target.parent().expect("a parent")).expect("directories");
        std::fs::write(&target, content(spec)).expect("a file");
        identities.insert(name.clone(), digest(&content(spec)).into());
    }
    let manifest = &case["manifest"];
    let written = if let Some(raw) = manifest.get("text") {
        (!raw.is_null()).then(|| content(raw))
    } else {
        let patch = manifest.get("patch").and_then(Value::as_object);
        for (name, identity) in patch.into_iter().flatten() {
            identities.insert(name.clone(), identity.clone());
        }
        let dropped = manifest.get("drop").and_then(Value::as_array);
        for name in dropped.into_iter().flatten() {
            identities.remove(text(name));
        }
        Some(pretty(&Value::Object(identities)).into_bytes())
    };
    if let Some(bytes) = written {
        std::fs::write(output.join("source-manifest.json"), bytes).expect("a manifest");
    }
    let policy = base.join("gitleaks.toml");
    if !case["configuration"].is_null() {
        std::fs::write(&policy, content(&case["configuration"])).expect("a configuration");
    }
    for operation in list(&case["tamper"]) {
        let target = source.join(text(&operation[1]));
        if text(&operation[0]) != "write" {
            std::fs::remove_file(&target).expect("a removed file");
        }
        match text(&operation[0]) {
            "write" => std::fs::write(&target, text(&operation[2])).expect("a changed file"),
            "link" => std::os::unix::fs::symlink(text(&operation[2]), &target).expect("a link"),
            "directory" => std::fs::create_dir(&target).expect("a directory"),
            _ => {}
        }
    }
    for existing in list(&case["before"]).iter().map(text) {
        if existing.contains('.') {
            std::fs::write(output.join(existing), "").expect("an existing file");
        } else {
            std::fs::create_dir(output.join(existing)).expect("an existing directory");
        }
    }
    [source, output, policy]
}

/// What a neutral snapshot left in `output`, as the recording describes it.
fn described(output: &Path, inputs: &Path, translated: &Path) -> Value {
    let mut projections = Map::new();
    for entry in std::fs::read_dir(inputs).expect("the inputs") {
        let path = entry.expect("an entry").path();
        let name = path.file_name().expect("a name").to_str().expect("text");
        let written = std::fs::read(&path).expect("a projection");
        let noted = json!({"mode": mode(&path), "sha256": digest(&written)});
        projections.insert(name.to_owned(), noted);
    }
    let read = |name: &str| std::fs::read_to_string(output.join(name)).expect("evidence");
    let names = [
        "secret-paths.json",
        "secret-coverage.json",
        "secret-configuration.toml",
    ];
    json!({
        "directory": inputs.file_name().expect("a name").to_str(),
        "directoryMode": mode(inputs),
        "inputs": projections,
        "translated": translated.file_name().expect("a name").to_str(),
        "configuration": std::fs::read_to_string(translated).expect("a configuration"),
        "paths": read("secret-paths.json"),
        "coverage": read("secret-coverage.json"),
        "modes": names.map(|name| mode(&output.join(name))),
    })
}

#[test]
fn every_recorded_neutral_snapshot_is_made_alike() {
    let scratch = Scratch::new("sn");
    let (mut tally, mut checked) = (Tally::default(), 0);
    for (index, case) in list(&recorded()["neutrals"]).iter().enumerate() {
        let name = text(&case["name"]);
        let base = scratch.0.join(index.to_string());
        let [source, output, policy] = built(&base, case);
        let mut limits = Limits::REVIEWED;
        if let Some(small) = case["limits"].as_object() {
            limits.source = small["source"].as_u64().expect("a source limit");
            limits.tree = small["tree"].as_u64().expect("a tree limit");
            let files = usize::try_from(small["files"].as_u64().expect("a file limit"));
            limits.files = files.expect("a small file limit");
        }
        let room = case["free"].as_u64();
        let room = room.unwrap_or_else(|| free(&output).expect("room"));
        let vendored = case["includeVendor"] == true;
        let made = neutral_snapshot(&output, &source, &policy, vendored, &limits, room);
        let refused = made.as_ref().err().map(|fault| fault.0);
        let reference = &case["result"];
        let stage = if list(&case["left"])
            .iter()
            .any(|left| left == "secret-coverage.json")
        {
            "configuration"
        } else {
            "manifest"
        };
        let reached = reference.get("code").is_none() || stage == "configuration";
        let shape = case["shape"].get("code").map(text);
        if let Some((_, code)) = DIFFERENT.iter().find(|(case, _)| *case == name) {
            assert_eq!(refused, Some(*code), "{name}");
            tally.2 += 1;
        } else if let Some(code) = shape.filter(|code| *code != "unreviewed_gitleaks_allowlist") {
            // The configuration check comes first in the gate and refuses this.
            if reached {
                assert_eq!(refused, Some(named(code, "configuration")), "{name}");
                checked += 1;
                continue;
            }
            let code = text(&reference["code"]);
            assert_eq!(refused, Some(named(code, stage)), "{name}");
            tally.1 += 1;
        } else if let Some(code) = reference.get("code") {
            assert_eq!(refused, Some(named(text(code), stage)), "{name}");
            let mut left: Vec<String> = std::fs::read_dir(&output)
                .expect("the output")
                .map(|entry| {
                    entry
                        .expect("an entry")
                        .file_name()
                        .into_string()
                        .expect("text")
                })
                .filter(|name| name != "source-manifest.json")
                .collect();
            left.sort_unstable();
            assert_eq!(json!(left), case["left"], "{name}");
            tally.1 += 1;
        } else {
            let made = made.expect(name);
            let mut described = described(&output, &made.inputs, &made.configuration);
            let mut reference = reference.clone();
            if name == REORDERED {
                let parsed = |side: &mut Value| {
                    let written = side["configuration"].take();
                    toml::from_str::<Value>(text(&written)).expect("a configuration")
                };
                assert_ne!(described["configuration"], reference["configuration"]);
                assert_eq!(parsed(&mut described), parsed(&mut reference), "{name}");
            }
            assert_eq!(described, reference, "{name}");
            tally.0 += 1;
        }
    }
    assert_eq!((tally, checked), (Tally(33, 27, 15), 16));
}
