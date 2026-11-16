//! Replays of the engine's commands of `recorded_sandbox.json`: what the
//! reference's runner in Python answered, refused and kept of real
//! commands; and how the runner names, numbers and branches its log.

use super::{Bounds, Runner};
use crate::{
    Fault,
    document::Document,
    image::{
        sandbox::{
            Engine,
            tests::{cases, pairs, texts, timeless, unpacked, whole},
        },
        secrets::tests::text,
    },
    scratch::Scratch,
};
use std::{
    ffi::OsString,
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    time::Duration,
};

/// The checkout, where the reference ran its commands.
fn checkout() -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    root.canonicalize().expect("the checkout")
}

/// The fixed code that refuses here what the reference's runner raised,
/// read from the words it said.
fn code(raised: &str, said: &str) -> String {
    if raised == "BuildError" {
        if let Some(stopped) = said.strip_prefix("Command stopped: ") {
            return stopped.split_once(": ").expect("a code").1.to_owned();
        }
        for (words, code) in [
            ("Command timed out: ", "command_timed_out"),
            ("Command failed (", "image_engine_command_failed"),
            (
                "Command inspection output too large: ",
                "image_engine_output_too_large",
            ),
        ] {
            if said.starts_with(words) {
                return code.to_owned();
            }
        }
    }
    let code = match raised {
        "UnicodeDecodeError" => "image_engine_output_not_text",
        "FileNotFoundError" => "tool_entry_missing",
        other => panic!("no code for {other}: {said}"),
    };
    code.to_owned()
}

/// What an answer was recorded as: its text, or the length and digest of
/// a long one.
fn answered(status: i32, said: &str, root: &Path) -> Document {
    let said = said.replace(root.to_str().expect("text"), "<root>");
    let text = if said.len() > 200 {
        Document::record([
            ("bytes", said.len().into()),
            ("sha256", crate::digest(said.as_bytes()).into()),
        ])
    } else {
        said.into()
    };
    Document::List(vec![Document::Number(i64::from(status).into()), text])
}

/// What a command left in `logs`, as the recorder showed it: each record
/// with its leader's number and its time replaced, and each log's bytes
/// with the checkout at `root` named as the recorder named it.
fn kept(logs: &Path, root: &Path) -> Vec<(String, Document, u32, Option<Vec<u8>>)> {
    let mut found = Vec::new();
    for entry in fs::read_dir(logs).expect("logs") {
        let path = entry.expect("an entry").path();
        let name = path
            .file_name()
            .expect("a name")
            .to_str()
            .expect("text")
            .to_owned();
        let mode = path
            .symlink_metadata()
            .expect("a file")
            .permissions()
            .mode()
            & 0o7777;
        let bytes = fs::read(&path).expect("bytes");
        if name.ends_with(".json") {
            let mut record = Document::read(&bytes).expect("a record");
            if record.get("pid").and_then(Document::whole).is_some() {
                record.put("pid", "<pid>".into());
            }
            found.push((name, timeless(record), mode, None));
        } else {
            let shown = String::from_utf8_lossy(&bytes)
                .replace(root.to_str().expect("text"), "<root>")
                .into_bytes();
            let bytes = if bytes.is_ascii() { shown } else { bytes };
            found.push((name, Document::Null, mode, Some(bytes)));
        }
    }
    found.sort_by(|left, right| left.0.cmp(&right.0));
    found
}

#[test]
fn commands_match_the_recording() {
    let root = checkout();
    let mut kinds = [0; 2];
    for case in cases("runners") {
        let name = text(&case, "name").to_owned();
        let scratch = Scratch::new("runner");
        let logs = scratch.0.join("logs");
        fs::create_dir(&logs).expect("logs");
        let commands = case
            .get("commands")
            .and_then(Document::items)
            .expect("commands");
        let environment = commands[0].get("env").map_or_else(
            || vec![("PATH".to_owned(), OsString::from("/usr/bin:/bin"))],
            |_| {
                let given = pairs(&commands[0], "env");
                given
                    .into_iter()
                    .map(|(key, value)| (key, value.into()))
                    .collect()
            },
        );
        // A short grace: the reference's fifteen seconds only make the
        // command that ignores the request to stop wait longer.
        let bounds = Bounds {
            grace: Duration::from_secs(2),
            ..Bounds::REVIEWED
        };
        let mut runner = Runner::new(Vec::new(), environment, &root, &logs, bounds);
        let results = case
            .get("results")
            .and_then(Document::items)
            .expect("results");
        assert_eq!(commands.len(), results.len(), "{name}");
        for (command, result) in commands.iter().zip(results) {
            let argv: Vec<OsString> = texts(command, "argv")
                .into_iter()
                .map(OsString::from)
                .collect();
            let seconds = command
                .get("timeout")
                .map_or(30, |_| whole(command, "timeout"));
            let failing = command.get("allow") == Some(&Document::Bool(true));
            let lease = command
                .get("healthy")
                .map(|held| *held == Document::Bool(true));
            let mut healthy = || lease.unwrap_or(true);
            match runner.command(&argv, seconds, failing, &mut healthy) {
                Ok((status, said)) => {
                    assert_eq!(
                        Some(&answered(status, &said, &root)),
                        result.get("held"),
                        "{name}"
                    );
                    kinds[0] += 1;
                }
                Err(Fault(refused)) => {
                    let raised = text(result, "raised");
                    assert_eq!(refused, code(raised, text(result, "said")), "{name}");
                    kinds[1] += 1;
                }
            }
        }
        let files = case.get("files").and_then(Document::fields).expect("files");
        let left = kept(&logs, &root);
        let names: Vec<&str> = left.iter().map(|(name, ..)| name.as_str()).collect();
        let recorded: Vec<&str> = files.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names, recorded, "{name}");
        for ((file, record, mode, bytes), (_, entry)) in left.iter().zip(files) {
            assert_eq!(u64::from(*mode), whole(entry, "mode"), "{name} {file}");
            match bytes {
                Some(bytes) => assert_eq!(*bytes, unpacked(entry), "{name} {file}"),
                None => assert_eq!(Some(record), entry.get("record"), "{name} {file}"),
            }
        }
    }
    assert_eq!(kinds, [12, 11]);
}

#[test]
fn the_reviewed_bounds_are_the_reference_runners() {
    // Two mebibytes a stream and of an answer, and fifteen seconds to end.
    let reviewed = Bounds::REVIEWED;
    assert_eq!(reviewed.stream, 2 * 1024 * 1024);
    assert_eq!(reviewed.inspected, 2 * 1024 * 1024);
    assert_eq!(reviewed.grace, Duration::from_secs(15));
}

#[test]
fn the_log_is_named_after_the_engine_and_numbered_from_each_branch() {
    let scratch = Scratch::new("runner-log");
    let logs = scratch.0.join("logs");
    fs::create_dir(&logs).expect("logs");
    let prefix = vec![OsString::from("/bin/sh"), "-c".into()];
    let environment = vec![("PATH".to_owned(), OsString::from("/usr/bin:/bin"))];
    let mut runner = Runner::new(prefix, environment, &checkout(), &logs, Bounds::REVIEWED);
    assert_eq!(runner.name(), "sh");
    assert_eq!(runner.log(), Some(logs.join("01-sh.log")));
    let first = runner.command(&["printf first".into()], 30, false, &mut || true);
    assert_eq!(first, Ok((0, "first".to_owned())));
    assert_eq!(runner.log(), Some(logs.join("02-sh.log")));
    let branched = scratch.0.join("branch");
    fs::create_dir(&branched).expect("logs");
    let mut branch = runner.branch(&branched);
    assert_eq!(branch.name(), "sh");
    assert_eq!(branch.log(), Some(branched.join("01-sh.log")));
    let said = branch.command(&["printf \"$PATH\"".into()], 30, false, &mut || true);
    assert_eq!(said, Ok((0, "/usr/bin:/bin".to_owned())));
    assert!(branched.join("01-sh.outcome.json").is_file());
    assert_eq!(runner.log(), Some(logs.join("02-sh.log")));
    let engines = Runner::new(
        vec!["/opt/podman/bin/podman".into(), "--remote=false".into()],
        Vec::new(),
        &checkout(),
        &logs,
        Bounds::REVIEWED,
    );
    assert_eq!(engines.name(), "podman");
    let empty = Runner::new(Vec::new(), Vec::new(), &checkout(), &logs, Bounds::REVIEWED);
    assert_eq!(empty.name(), "");
    assert_eq!(empty.log(), Some(logs.join("01-.log")));
}
