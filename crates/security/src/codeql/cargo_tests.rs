//! Replays of `recorded_cargo.json`: what the reference
//! made of fixture checkouts listed by a fake `git`, with fake compiler
//! drivers whose text the recording holds, and of restored build
//! directories. Every key is of a fixed source path, so that the recorded
//! keys are this host's as well.

use super::{
    CODEQL_NATIVE_FILES, CODEQL_RUST_MANIFESTS, Host, cache_key, dependency_input, payload_bytes,
    prepare, private_directory, stabilize_dependencies, validate,
};
use crate::{
    codeql::tests::{Tally, items, planted, recording, scrubbed, text, unlocked, whole},
    document::Document,
    scratch::Scratch,
};
use std::{
    ffi::OsString,
    fs,
    os::unix::fs::{MetadataExt as _, PermissionsExt as _},
    path::{Path, PathBuf},
};

const RECORDED: &[u8] = include_bytes!("recorded_cargo.json");

/// Files of the native analysis that were deleted after the recording was
/// made: the recording lists them among the files every CodeQL key binds
/// and judged each an input, and none is one any longer.
const REMOVED_SINCE_RECORDING: [&str; 5] = [
    "build/ci_verified.py",
    "build/security_findings.py",
    "build/security_policy.py",
    "build/security_source_scope.py",
    "build/security_vendor.py",
];

/// The code here of what the reference's interpreter raised.
fn raised(class: &str, words: &str) -> String {
    match class {
        "UnicodeDecodeError" | "ValueError" | "JSONDecodeError" | "DuplicateJsonError" => {
            "codeql_cargo_unread".to_owned()
        }
        "ProcessError" => words.to_owned(),
        "FileNotFoundError" => "tool_entry_missing".to_owned(),
        "OSError" | "PermissionError" => "tool_entry_unavailable".to_owned(),
        "IsADirectoryError" => "nonregular_tool_file".to_owned(),
        other => panic!("unexpected {other}"),
    }
}

/// A fresh place holding the tree of `case`.
fn placed(case: &Document) -> Scratch {
    let scratch = Scratch::new("codeql-cargo");
    planted(&scratch.0, case.get("tree").expect("a tree"));
    scratch
}

/// The recorded path `name` of `case` with its place written in.
fn path(case: &Document, name: &str, place: &Path) -> PathBuf {
    PathBuf::from(text(case, name).replace("<place>", &place.to_string_lossy()))
}

/// The caller's environment of a recorded case: what it records, its fake
/// programs' directory as the search path, and a home of its own.
fn environment(case: &Document, place: &Path) -> impl Fn(&str) -> Option<OsString> {
    let recorded = case.get("environment").cloned().expect("an environment");
    let (search, home) = (place.join("bin"), place.join("caller"));
    move |name: &str| match name {
        "PATH" => Some(search.clone().into_os_string()),
        "HOME" => Some(home.clone().into_os_string()),
        _ => recorded
            .get(name)
            .and_then(Document::text)
            .map(OsString::from),
    }
}

/// What the fake programs of a case logged.
fn logged(place: &Path) -> Document {
    let fields = ["cargo", "git", "rustc"].map(|name| {
        let log = fs::read_to_string(place.join(format!("fake/{name}.log"))).unwrap_or_default();
        (name.to_owned(), Document::from(scrubbed(&log, place)))
    });
    Document::Record(fields.to_vec())
}

#[test]
fn every_recorded_name_is_judged_an_input_alike() {
    let recorded = recording(RECORDED);
    let mut judged = [0; 2];
    for case in items(&recorded, "inputs") {
        let name = text(case, "name");
        let held = case.get("held") == Some(&Document::Bool(true));
        // A file removed since the recording was an input then and is none now.
        let removed = REMOVED_SINCE_RECORDING.contains(&name);
        assert!(!removed || held, "{name}");
        assert_eq!(dependency_input(name), held != removed, "{name}");
        judged[usize::from(removed)] += 1;
    }
    assert_eq!(judged, [46, 5]);
    let listed = |name: &str| -> Vec<String> {
        let words = items(&recorded, name)
            .iter()
            .map(|word| word.text().expect("a name"));
        words.map(str::to_owned).collect()
    };
    let mut native: Vec<String> = CODEQL_NATIVE_FILES
        .iter()
        .map(|name| (*name).to_owned())
        .collect();
    native.sort();
    let mut kept = listed("nativeFiles");
    kept.retain(|name| !REMOVED_SINCE_RECORDING.contains(&name.as_str()));
    assert_eq!(
        kept.len() + REMOVED_SINCE_RECORDING.len(),
        listed("nativeFiles").len()
    );
    assert_eq!(native, kept);
    let mut manifests: Vec<String> = CODEQL_RUST_MANIFESTS.map(str::to_owned).to_vec();
    manifests.sort();
    assert_eq!(manifests, listed("manifests"));
}

#[test]
fn every_recorded_key_is_reckoned_alike() {
    let recorded = recording(RECORDED);
    let mut tally = Tally::default();
    for case in items(&recorded, "keys") {
        let scratch = placed(case);
        let place = &scratch.0;
        let variable = environment(case, place);
        let search = place.join("bin").into_os_string();
        let host = Host {
            variable: &variable,
            system: text(case, "system"),
            machine: text(case, "machine"),
            search: Some(&search),
        };
        let key = cache_key(&place.join("root"), &path(case, "source", place), &host);
        let came = key
            .map(Document::from)
            .map_err(|failure| failure.to_string());
        tally.judge(case, came, &raised, &[]);
        assert_eq!(
            logged(place),
            *case.get("logs").expect("logs"),
            "{}",
            text(case, "name")
        );
    }
    let expected = Tally {
        held: 45,
        refused: 48,
        raised: 11,
        stricter: 0,
    };
    assert_eq!(tally, expected);
}

#[test]
fn every_recorded_directory_is_judged_private_alike() {
    let recorded = recording(RECORDED);
    let mut tally = Tally::default();
    for case in items(&recorded, "private") {
        let scratch = placed(case);
        let judged = private_directory(&path(case, "path", &scratch.0));
        let came = judged
            .map(|()| Document::Null)
            .map_err(|failure| failure.to_string());
        tally.judge(case, came, &raised, &[]);
    }
    let expected = Tally {
        held: 3,
        refused: 7,
        raised: 1,
        stricter: 0,
    };
    assert_eq!(tally, expected);
}

#[test]
fn every_recorded_restored_build_is_validated_alike() {
    let recorded = recording(RECORDED);
    let mut tally = Tally::default();
    for case in items(&recorded, "validates") {
        let scratch = placed(case);
        let place = &scratch.0;
        let source = PathBuf::from(text(case, "source"));
        let target = validate(&source, &path(case, "directory", place), text(case, "key"));
        let came = target
            .map(|target| Document::from(scrubbed(&target.to_string_lossy(), place)))
            .map_err(|failure| failure.to_string());
        tally.judge(case, came, &raised, &[]);
    }
    let expected = Tally {
        held: 7,
        refused: 19,
        raised: 6,
        stricter: 0,
    };
    assert_eq!(tally, expected);
}

#[test]
fn every_recorded_build_is_weighed_alike() {
    let recorded = recording(RECORDED);
    let mut tally = Tally::default();
    for case in items(&recorded, "payloads") {
        let scratch = placed(case);
        let weighed = payload_bytes(&scratch.0.join("target"));
        unlocked(&scratch.0);
        let came = weighed
            .map(Document::from)
            .map_err(|failure| failure.to_string());
        tally.judge(case, came, &raised, &[]);
    }
    let expected = Tally {
        held: 12,
        refused: 9,
        raised: 0,
        stricter: 0,
    };
    assert_eq!(tally, expected);
}

/// The time each entry below the source of `place` was last changed, and
/// a file's last read, as the recording writes them: the reviewed
/// fixture's time, the fixed time, or another.
fn times(place: &Path) -> Document {
    let root = place.join("source");
    let mut found = Vec::new();
    let mut pending = vec![root.clone()];
    while let Some(directory) = pending.pop() {
        let Ok(entries) = fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let entry_path = entry.path();
            let metadata = entry_path.symlink_metadata().expect("metadata");
            if metadata.is_dir() {
                pending.push(entry_path.clone());
            }
            let shown = |time: i64| {
                if time == 1_234_567_890 || time == super::DEPENDENCY_MTIME {
                    Document::Number(time.into())
                } else {
                    "other".into()
                }
            };
            let relative = entry_path.strip_prefix(&root).expect("below");
            let relative = relative.to_string_lossy().into_owned();
            // A file is never read once it is stamped, so its access time is
            // the stamp's.
            if metadata.is_file() {
                found.push((format!("{relative} accessed"), shown(metadata.atime())));
            }
            found.push((relative, shown(metadata.mtime())));
        }
    }
    Document::Record(found).sorted()
}

#[test]
fn every_recorded_source_tree_is_stamped_alike() {
    let recorded = recording(RECORDED);
    let mut tally = Tally::default();
    for case in items(&recorded, "stabilized") {
        let scratch = placed(case);
        let place = &scratch.0;
        let stamped = stabilize_dependencies(&path(case, "source", place));
        let found = times(place);
        unlocked(place);
        let came = stamped
            .map(|()| Document::Null)
            .map_err(|failure| failure.to_string());
        tally.judge(case, came, &raised, &[]);
        let expected = case.get("mtimes").cloned().expect("times").sorted();
        assert_eq!(found, expected, "{}", text(case, "name"));
    }
    let expected = Tally {
        held: 7,
        refused: 10,
        raised: 0,
        stricter: 0,
    };
    assert_eq!(tally, expected);
}

/// The hosted environment every prepared build's key is reckoned in.
fn hosted(place: &Path) -> impl Fn(&str) -> Option<OsString> {
    let (search, home) = (place.join("bin"), place.join("caller"));
    move |name: &str| match name {
        "PATH" => Some(search.clone().into_os_string()),
        "HOME" => Some(home.clone().into_os_string()),
        "GITHUB_REF" => Some("refs/heads/main".into()),
        "GITHUB_EVENT_NAME" => Some("push".into()),
        "ImageOS" => Some("ubuntu24".into()),
        "ImageVersion" => Some("20261101.1".into()),
        _ => None,
    }
}

/// What the directory `cache` holds as the recording lists it, with the
/// key `key` written as the recording writes it.
fn holding(cache: &Path, key: &str, place: &Path) -> Document {
    let mut found = Vec::new();
    let mut pending = vec![cache.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let Ok(entries) = fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let entry_path = entry.path();
            let relative = entry_path.strip_prefix(cache).expect("below");
            let relative = relative.to_string_lossy().into_owned();
            let metadata = entry_path.symlink_metadata().expect("metadata");
            if metadata.file_type().is_symlink() {
                found.push((relative, Document::record([("kind", "link".into())])));
                continue;
            }
            let mode = u64::from(metadata.permissions().mode() & 0o777);
            let mut fields = vec![("mode".to_owned(), Document::from(mode))];
            if metadata.is_dir() {
                pending.push(entry_path);
            } else if metadata.len() <= 2048 {
                let held = fs::read_to_string(&entry_path).expect("text");
                fields.push((
                    "text".to_owned(),
                    scrubbed(&held.replace(key, "<key>"), place).into(),
                ));
            }
            found.push((relative, Document::Record(fields)));
        }
    }
    Document::Record(found).sorted()
}

#[test]
fn every_recorded_build_is_prepared_alike() {
    let recorded = recording(RECORDED);
    let mut steps_taken = 0;
    for case in items(&recorded, "prepares") {
        let scratch = placed(case);
        let place = &scratch.0;
        let variable = hosted(place);
        let search = place.join("bin").into_os_string();
        let host = Host {
            variable: &variable,
            system: "Linux",
            machine: "x86_64",
            search: Some(&search),
        };
        let (root, source) = (place.join("root"), place.join("source"));
        let directory = path(case, "directory", place);
        let name = text(case, "name");
        let mut first = None;
        for (step, expected) in items(case, "steps").iter().zip(items(case, "results")) {
            if let Some(said) = step.get("rustc").and_then(Document::text) {
                fs::write(place.join("fake/rustc.out"), said).expect("a compiler's text");
            }
            if let Some(written) = step.get("write") {
                planted(place, written);
            }
            let key = cache_key(&root, &source, &host).unwrap_or_else(|_| "<none>".to_owned());
            let key = first.get_or_insert(key).clone();
            let prepared = prepare(&root, &source, &directory, &host);
            let came = match prepared {
                Ok(target) => {
                    Document::record([("held", scrubbed(&target.to_string_lossy(), place).into())])
                }
                Err(failure) => Document::record([("refused", failure.to_string().into())]),
            };
            assert_eq!(came, *expected.get("result").expect("a result"), "{name}");
            let cache = holding(&place.join("cache"), &key, place);
            assert_eq!(
                cache,
                expected.get("cache").cloned().expect("a cache").sorted(),
                "{name}"
            );
            // A parent made on the way has the mode of a directory made
            // without one, whatever the mask of this process is.
            let probe = place.join(format!("probe-{steps_taken}"));
            fs::create_dir(&probe).expect("a probe");
            let mode = |path: &Path| path.symlink_metadata().map(|held| held.mode() & 0o777).ok();
            let made: Vec<(String, Document)> = ["deep", "deep/er"]
                .into_iter()
                .filter(|parent| place.join(parent).is_dir())
                .map(|parent| {
                    (
                        parent.to_owned(),
                        (mode(&place.join(parent)) == mode(&probe)).into(),
                    )
                })
                .collect();
            fs::remove_dir(&probe).expect("a probe removed");
            let parents = expected.get("parents").cloned().expect("parents");
            assert_eq!(Document::Record(made).sorted(), parents.sorted(), "{name}");
            steps_taken += 1;
        }
    }
    assert_eq!(steps_taken, 23);
}

/// The commands the reference let its interpreter end with a traceback,
/// which end here with the command's own failure line and status.
const TOLD: [&str; 1] = ["a key whose compiler says too much"];

#[test]
fn every_recorded_cargo_key_command_answers_alike() {
    let recorded = recording(RECORDED);
    let mut answered = 0;
    for case in items(&recorded, "mains") {
        let scratch = placed(case);
        let place = &scratch.0;
        let name = text(case, "name");
        let variable = environment(case, place);
        let search = place.join("bin").into_os_string();
        if let Some(receipt) = case
            .get("receipt")
            .filter(|receipt| **receipt != Document::Bool(false))
        {
            let host = Host {
                variable: &variable,
                system: "Linux",
                machine: "x86_64",
                search: Some(&search),
            };
            let source = Path::new(text(&recorded, "source"));
            let key = cache_key(&place.join("root"), source, &host).expect("a key");
            let written = Document::record([
                ("schemaVersion", 1_u64.into()),
                ("key", key.as_str().into()),
                ("sourceRoot", source.to_string_lossy().as_ref().into()),
            ]);
            let mut tree = vec![
                (
                    "check".to_owned(),
                    Document::record([("directory", true.into()), ("mode", 0o700_u64.into())]),
                ),
                (
                    "check/target".to_owned(),
                    Document::record([("directory", true.into()), ("mode", 0o700_u64.into())]),
                ),
                (
                    "check/receipt.json".to_owned(),
                    Document::record([
                        ("text", format!("{}\n", written.spaced()).into()),
                        ("mode", 0o600_u64.into()),
                    ]),
                ),
            ];
            if let Some(fields) = receipt.fields() {
                tree.extend(fields.iter().cloned());
            }
            planted(place, &Document::Record(tree));
        }
        let mut arguments = vec!["cargo-key".to_owned()];
        let words = items(case, "argv")
            .iter()
            .map(|word| word.text().expect("a word"));
        arguments.extend(words.map(|word| word.replace("<place>", &place.to_string_lossy())));
        let budget =
            |_: Option<&str>| -> crate::Result<crate::budget::Budget> { panic!("no budget") };
        let private = || panic!("not the analyzer's setup");
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let mut around = crate::codeql::cli::Around {
            variable: &variable,
            search: Some(&search),
            system: "Linux",
            machine: "x86_64",
            bundle: "darwin",
            budget: &budget,
            private: &private,
            out: &mut out,
            err: &mut err,
        };
        let status = crate::codeql::cli::command(&place.join("root"), &arguments, &mut around);
        let (out, err) = (
            String::from_utf8(out).expect("text"),
            String::from_utf8(err).expect("text"),
        );
        if TOLD.contains(&name) {
            assert!(
                case.get("status")
                    .and_then(Document::text)
                    .is_some_and(|said| said.starts_with("raised "))
            );
            assert_eq!(
                (status, out.as_str(), err.as_str()),
                (1, "", "Rust extraction Cargo cache validation failed.\n")
            );
        } else {
            assert_eq!(i128::from(status), whole(case, "status"), "{name}");
            assert_eq!(out, text(case, "stdout"), "{name}");
            assert_eq!(err, text(case, "stderr"), "{name}");
        }
        unlocked(place);
        answered += 1;
    }
    assert_eq!(answered, 10);
}

#[test]
fn the_workflow_reuses_dependencies_only_for_a_fresh_rust_extraction() {
    // From `test_workflow_reuses_dependencies_only_for_fresh_rust_extraction`:
    // no cached dependency build skips extraction, policy or the bounded
    // publication check.
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let text = fs::read_to_string(root.join(".github/workflows/codeql.yml")).expect("the workflow");
    let workflow: serde_json::Value = serde_saphyr::from_str(&text).expect("a workflow");
    let steps = workflow["jobs"]["source-analysis"]["steps"]
        .as_array()
        .expect("steps");
    let step = |field: &str, value: &str| {
        let found = steps.iter().position(|step| step[field] == value);
        found.unwrap_or_else(|| panic!("no step {value}"))
    };
    let said = |index: usize, field: &str| steps[index][field].as_str().unwrap_or_default();
    let restore = step("id", "rust-cargo");
    assert!(said(restore, "if").contains("steps.rust-database.outputs.cache-hit != 'true'"));
    let key = steps[restore]["with"]["key"].as_str().expect("a key");
    assert!(!key.contains("restore-keys"));
    let runner = step("id", "source-analysis");
    let cache = "--rust-cargo-cache \"$RUNNER_TEMP/codeql-rust-cargo\"";
    assert!(said(runner, "run").contains(cache));
    let size = step("id", "rust-cargo-size");
    assert!(said(size, "run").starts_with("set -euo pipefail\n"));
    let ready = "steps.source-analysis.outputs.database-cache-ready == 'true'";
    assert!(said(size, "if").contains(ready));
    let save = step("name", "Save bounded Rust extraction build output");
    assert!(said(save, "if").contains("steps.rust-cargo-size.outputs.cacheable == 'true'"));
    assert!(size < save);
    assert!(steps[runner].get("continue-on-error").is_none());
}
