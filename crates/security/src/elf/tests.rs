//! Replays of the recorded ELF audits.
//!
//! `recorded_elf.json` holds what the reference in Python made of ELF bytes
//! that hold metadata and no program: what it parsed, the hardening it
//! found, the dependency metadata it read, the paths it resolved inside
//! image trees, whole audits and its command line. Bytes are recorded as
//! their length and the runs that are not zero. Where the reference raised
//! an exception of its interpreter, `named` says how the audit ends here.

use super::{
    Elf, Refusal, Stop, audit, checked, dependency_diagnostics, dependency_metadata, hardening,
    image_path, parse, platform,
};
use crate::{
    Fault, digest,
    document::Document,
    scratch::{Scratch, base64},
};
use std::{fs::Permissions, os::unix::fs::PermissionsExt, path::Path, process::Command};

const RECORDING: &[u8] = include_bytes!("recorded_elf.json");

/// Metadata the reference read and that is no document here: it is read
/// as UTF-8 without a mark.
const STRICTER: [&str; 4] = [
    "UTF-8 with a mark",
    "UTF-16",
    "UTF-16 without a mark",
    "UTF-32 without a mark",
];

/// Metadata both refuse, with the code it has here: a number that is not
/// one, half of an escaped pair and a document nested deeper than 128
/// levels are no JSON here, and a minus zero is no whole number.
const OTHERWISE: [(&str, &str); 4] = [
    (
        "a not-a-number among the dependencies",
        "auditable_metadata_invalid",
    ),
    ("a dependency of minus zero", "auditable_dependency_index"),
    (
        "an escaped half of a pair in a name",
        "auditable_metadata_invalid",
    ),
    ("a document nested very deep", "auditable_metadata_invalid"),
];

/// Objects whose dynamic table does not lie where the loader reads it. The
/// reference read the table where the header says it lies in the file: it
/// parsed seven of these, one with a search path that only the loader
/// would have seen, and refused the other four by a later rule. Here they
/// are refused first.
const UNMAPPED: [&str; 11] = [
    "a string table in two loaded segments",
    "a string table in a segment that is not loaded",
    "a string table in a segment of another kind and no loaded segment",
    "a dynamic table that the loader reads elsewhere, with a search path there",
    "a dynamic table at an address outside every loaded segment",
    "a dynamic table at an address below every loaded segment",
    "a dynamic table mapped two entries off",
    "a dynamic table mapped a byte off",
    "a dynamic table in a second loaded segment at another offset",
    "a dynamic table that ends past the file of its loaded segment",
    "a dynamic table whose address wraps a machine word",
];

/// Images the reference passed and that are refused here, with the code: a
/// library that undoes what the executable is held to, or whose dynamic
/// table the loader reads elsewhere.
const UNPROTECTED: [(&str, &str); 10] = [
    ("a library without its protections", "elf_library_stack"),
    ("a library with an executable stack", "elf_library_stack"),
    ("a library without a stack header", "elf_library_stack"),
    ("a library with two stack headers", "elf_library_stack"),
    (
        "a library with a writable and executable segment",
        "elf_library_writable_executable",
    ),
    (
        "a library with text relocations",
        "elf_library_text_relocations",
    ),
    (
        "a library with text relocations by the flags",
        "elf_library_text_relocations",
    ),
    ("a loader with an executable stack", "elf_library_stack"),
    (
        "a loader with text relocations",
        "elf_library_text_relocations",
    ),
    (
        "a library with a dynamic table that the loader reads elsewhere",
        "elf_dynamic_table_mapping",
    ),
];

/// Command lines the reference's reader took and that are refused here:
/// an option given twice, cut short or joined to its value, and a value of
/// nothing.
const LENIENT: [&str; 6] = [
    "an option given twice",
    "the report named twice",
    "the platform named twice",
    "an image of no name",
    "an option cut short",
    "an option joined to its value",
];

/// Cases that end otherwise for the administrator, who may read and search
/// everything.
const UNPRIVILEGED: [&str; 2] = [
    "a directory that may not be searched",
    "an executable that nobody may read",
];

fn recording() -> Document {
    Document::read(RECORDING).expect("the recording is a document")
}

fn field<'a>(document: &'a Document, name: &str) -> &'a Document {
    document.get(name).unwrap_or(&Document::Null)
}

fn text<'a>(document: &'a Document, name: &str) -> &'a str {
    field(document, name)
        .text()
        .unwrap_or_else(|| panic!("no text {name}"))
}

fn list<'a>(document: &'a Document, name: &str) -> &'a [Document] {
    field(document, name)
        .items()
        .unwrap_or_else(|| panic!("no list {name}"))
}

fn number(document: &Document) -> u64 {
    u64::try_from(document.whole().expect("a number")).expect("a count")
}

/// The bytes that hexadecimal digits spell.
fn bytes_of(digits: &str) -> Vec<u8> {
    let pairs = digits.as_bytes().as_chunks::<2>().0.iter();
    let byte = |pair: &[u8; 2]| {
        let pair = std::str::from_utf8(pair).expect("digits");
        u8::from_str_radix(pair, 16).expect("a byte")
    };
    pairs.map(byte).collect()
}

/// The bytes that a recording holds as their length and their runs.
fn restored(spec: &Document) -> Vec<u8> {
    let mut data = vec![0; usize::try_from(number(field(spec, "length"))).expect("a length")];
    for part in list(spec, "parts") {
        let part = part.items().expect("a run");
        let at = usize::try_from(number(&part[0])).expect("a place");
        let digits = part[1].text().expect("digits");
        for (index, byte) in bytes_of(digits).into_iter().enumerate() {
            data[at + index] = byte;
        }
    }
    data
}

/// What was parsed, in the recording's shape.
fn facts(elf: &Elf) -> Document {
    let segment = |segment: &super::Segment| {
        let sizes = [
            segment.offset,
            segment.address,
            segment.file_size,
            segment.memory_size,
        ];
        let mut fields: Vec<Document> = vec![
            u64::from(segment.kind).into(),
            u64::from(segment.flags).into(),
        ];
        fields.extend(sizes.map(Document::from));
        Document::List(fields)
    };
    let tag = |(tag, values): &(i64, Vec<u64>)| {
        let values: Vec<Document> = values.iter().map(|value| (*value).into()).collect();
        Document::List(vec![Document::Number((*tag).into()), values.into()])
    };
    let section = |section: &Vec<u8>| {
        Document::record([
            ("bytes", section.len().into()),
            ("sha256", digest(section).into()),
        ])
    };
    Document::record([
        (
            "segments",
            elf.segments.iter().map(segment).collect::<Vec<_>>().into(),
        ),
        (
            "dynamic",
            elf.dynamic.iter().map(tag).collect::<Vec<_>>().into(),
        ),
        (
            "needed",
            elf.needed
                .iter()
                .map(|name| name.as_str().into())
                .collect::<Vec<Document>>()
                .into(),
        ),
        ("interpreter", elf.interpreter.as_deref().into()),
        ("audit", elf.audit.as_ref().map(section).into()),
    ])
}

/// A written document against what the recording holds of it: its text,
/// or the length and the digest of a long one.
fn same_text(written: &Document, recorded: &Document, name: &str) {
    let written = written.pretty();
    let written = written.strip_suffix('\n').expect("a line end");
    if let Some(large) = recorded.get("large") {
        assert_eq!(
            written.len() as u64,
            number(field(large, "bytes")),
            "{name}"
        );
        assert_eq!(digest(written.as_bytes()), text(large, "sha256"), "{name}");
    } else {
        assert_eq!(written, text(recorded, "text"), "{name}");
    }
}

/// How the audit ends here where the reference raised.
fn named(class: &str) -> Stop {
    match class {
        "FileNotFoundError" => Stop::Missing,
        "PermissionError" | "OSError" => Stop::Unreadable,
        other => panic!("{other} is not named"),
    }
}

/// How a recorded result that is no answer ends here.
fn stopped(recorded: &Document) -> Stop {
    if let Some(class) = recorded.get("raised") {
        return named(class.text().expect("a class"));
    }
    Stop::Refused(Refusal {
        reason: text(recorded, "reason").to_owned(),
        dependencies: recorded.get("dependencies").cloned(),
    })
}

#[test]
fn every_recorded_elf_is_read_alike() {
    let recording = recording();
    // Parsed and refused; hardened and not; with metadata and without; and
    // refused here alone.
    let mut tally = (0, 0, 0, 0, 0, 0, 0);
    for case in list(&recording, "parses") {
        let name = text(case, "name");
        let data = restored(field(case, "data"));
        let platform_name = text(case, "platform");
        let recorded = field(case, "parse");
        let parsed = parse(&data, platform_name);
        if UNMAPPED.contains(&name) {
            assert_eq!(parsed, Err(Fault("elf_dynamic_table_mapping")), "{name}");
            tally.6 += 1;
            continue;
        }
        let Some(expected) = recorded.get("facts") else {
            assert_eq!(
                parsed,
                Err(Fault(Box::leak(
                    text(recorded, "reason").to_owned().into_boxed_str()
                ))),
                "{name}"
            );
            tally.1 += 1;
            continue;
        };
        let elf = parsed.unwrap_or_else(|fault| panic!("{name}: {fault}"));
        assert_eq!(&facts(&elf), expected, "{name}");
        tally.0 += 1;
        let recorded = field(case, "hardening");
        match (hardening(&elf, platform_name), recorded.get("checks")) {
            (Ok(checks), Some(expected)) => {
                assert_eq!(&checks, expected, "{name}");
                tally.2 += 1;
            }
            (Err(refusal), None) => {
                assert_eq!(Stop::Refused(refusal), stopped(recorded), "{name}");
                tally.3 += 1;
            }
            (found, _) => panic!("{name}: {found:?}"),
        }
        let recorded = field(case, "metadata");
        match dependency_metadata(elf.audit.as_deref()) {
            Ok(metadata) => {
                same_text(&metadata, recorded, name);
                tally.4 += 1;
            }
            Err(fault) => {
                assert_eq!(fault.0, text(recorded, "reason"), "{name}");
                tally.5 += 1;
            }
        }
    }
    assert_eq!(tally, (62, 102, 30, 32, 57, 5, 11));
}

#[test]
fn every_recorded_section_of_metadata_is_read_alike() {
    let recording = recording();
    // Read alike, refused alike, refused here alone, and refused by both
    // under another code.
    let mut tally = (0, 0, 0, 0);
    for case in list(&recording, "metadata") {
        let name = text(case, "name");
        let section = field(case, "section").text().map(base64);
        let recorded = field(case, "result");
        let read = dependency_metadata(section.as_deref());
        if STRICTER.contains(&name) {
            assert_eq!(read, Err(Fault("auditable_metadata_invalid")), "{name}");
            assert!(recorded.get("reason").is_none(), "{name}");
            tally.2 += 1;
            continue;
        }
        if let Some((_, code)) = OTHERWISE.iter().find(|(other, _)| *other == name) {
            assert_eq!(read, Err(Fault(code)), "{name}");
            assert_ne!(
                recorded.get("reason").and_then(Document::text),
                Some(*code),
                "{name}"
            );
            assert!(recorded.get("reason").is_some(), "{name}");
            tally.3 += 1;
            continue;
        }
        match read {
            Ok(metadata) => {
                same_text(&metadata, recorded, name);
                tally.0 += 1;
            }
            Err(fault) => {
                assert_eq!(fault.0, text(recorded, "reason"), "{name}");
                tally.1 += 1;
            }
        }
    }
    assert_eq!(tally, (29, 89, 4, 4));
}

#[test]
fn documents_are_written_as_the_reference_wrote_them() {
    let recording = recording();
    let cases = list(&recording, "documents");
    for case in cases {
        let source = text(case, "source");
        let read = Document::read(source.as_bytes()).expect("a document");
        assert_eq!(
            read.pretty(),
            format!("{}\n", text(case, "text")),
            "{source}"
        );
    }
    assert_eq!(cases.len(), 16);
    use crate::document::Unread;
    assert_eq!(Document::read(b"1 2"), Err(Unread::Invalid));
    assert_eq!(Document::read(b""), Err(Unread::Invalid));
    assert_eq!(
        Document::read(b"{\"a\": 1, \"a\": 2}"),
        Err(Unread::Duplicate)
    );
    assert_eq!(
        Document::read(b"[{\"a\": 1, \"a\": 2}, }"),
        Err(Unread::Duplicate)
    );
    assert_eq!(
        Document::read(b"[{\"a\": 1, \"a\": 2"),
        Err(Unread::Invalid)
    );
    assert_eq!(
        Document::read(b"[{\"a\": {\"b\": 1}, \"b\": 2}]").map(|_| ()),
        Ok(())
    );
}

#[test]
fn what_is_said_of_needed_libraries_is_said_alike() {
    let recording = recording();
    let cases = list(&recording, "diagnostics");
    for case in cases {
        let needed: Vec<String> = list(case, "needed")
            .iter()
            .map(|name| name.text().expect("a name").to_owned())
            .collect();
        let (_, loader) = platform(text(case, "platform")).expect("a platform");
        assert_eq!(
            &dependency_diagnostics(&needed, loader),
            field(case, "result"),
            "{needed:?}"
        );
    }
    assert_eq!(cases.len(), 11);
}

/// Plant a recorded tree below `root`: files of text or of a named fixture,
/// links, directories and named pipes, each with its mode when it has one.
fn planted(root: &Path, tree: &Document, fixtures: &Document) {
    for (path, spec) in tree.fields().expect("a tree") {
        let target = root.join(path);
        std::fs::create_dir_all(target.parent().expect("a parent")).expect("directories");
        if let Some(link) = spec.get("link") {
            std::os::unix::fs::symlink(link.text().expect("a target"), &target).expect("a link");
        } else if spec.get("directory").is_some() {
            std::fs::create_dir_all(&target).expect("a directory");
        } else if let Some(length) = spec.get("sparse") {
            let file = std::fs::File::create(&target).expect("a file");
            file.set_len(number(length)).expect("a length");
        } else if spec.get("fifo").is_some() {
            let made = Command::new("mkfifo")
                .arg(&target)
                .status()
                .expect("mkfifo runs");
            assert!(made.success());
        } else {
            let mut content = match spec.get("fixture") {
                Some(fixture) => restored(field(fixtures, fixture.text().expect("a name"))),
                None => spec
                    .get("text")
                    .and_then(Document::text)
                    .unwrap_or_default()
                    .as_bytes()
                    .to_vec(),
            };
            let more = spec
                .get("append")
                .and_then(Document::text)
                .unwrap_or_default();
            content.extend(bytes_of(more));
            std::fs::write(&target, content).expect("a file");
        }
        if let Some(mode) = spec.get("mode") {
            let mode = Permissions::from_mode(u32::try_from(number(mode)).expect("a mode"));
            std::fs::set_permissions(&target, mode).expect("a mode");
        }
    }
}

/// Give back what a case took away, so that its tree can be removed.
fn restore(root: &Path, tree: &Document) {
    for (path, spec) in tree.fields().expect("a tree") {
        if spec.get("mode").is_some() {
            let _ = std::fs::set_permissions(root.join(path), Permissions::from_mode(0o700));
        }
    }
}

fn administrator() -> bool {
    rustix::process::geteuid().is_root()
}

#[test]
fn every_recorded_path_is_resolved_alike() {
    let recording = recording();
    let scratch = Scratch::new("elf-paths");
    // Resolved, refused, and stopped by the files.
    let mut tally = (0, 0, 0);
    for (index, case) in list(&recording, "paths").iter().enumerate() {
        let name = text(case, "name");
        if administrator() && UNPRIVILEGED.contains(&name) {
            continue;
        }
        let root = scratch.0.join(index.to_string());
        std::fs::create_dir_all(&root).expect("a root");
        planted(&root, field(case, "tree"), &Document::Null);
        let resolved = image_path(&root, text(case, "asked"));
        restore(&root, field(case, "tree"));
        let recorded = field(case, "result");
        match resolved {
            Ok(path) => {
                let below = path.strip_prefix(&root).expect("below the root");
                assert_eq!(
                    below.to_str(),
                    recorded.get("path").and_then(Document::text),
                    "{name}"
                );
                tally.0 += 1;
            }
            Err(stop) => {
                assert_eq!(stop, stopped(recorded), "{name}");
                tally.1 += usize::from(matches!(stop, Stop::Refused(_)));
                tally.2 += usize::from(!matches!(stop, Stop::Refused(_)));
            }
        }
    }
    if !administrator() {
        assert_eq!(tally, (15, 19, 6));
    }
}

#[test]
fn every_recorded_image_is_audited_alike() {
    let recording = recording();
    let scratch = Scratch::new("elf-audits");
    let fixtures = field(&recording, "fixtures");
    // Passed, refused, stopped by the files, and refused here alone.
    let mut tally = (0, 0, 0, 0);
    for (index, case) in list(&recording, "audits").iter().enumerate() {
        let name = text(case, "name");
        if administrator() && UNPRIVILEGED.contains(&name) {
            continue;
        }
        let place = scratch.0.join(index.to_string());
        std::fs::create_dir_all(place.join("root")).expect("a root");
        planted(&place.join("root"), field(case, "tree"), fixtures);
        match field(case, "prepared").text() {
            Some("linked_root") => {
                std::os::unix::fs::symlink("root", place.join("linked")).expect("a link")
            }
            Some("file_root") => std::fs::write(place.join("file"), "x").expect("a file"),
            Some(other) => panic!("{other} is not prepared"),
            None => {}
        }
        let root = place.join(text(case, "root"));
        let platform_name = text(case, "platform");
        let audited = audit(&root, platform_name);
        let recorded = field(case, "result");
        if let Some((_, code)) = UNPROTECTED.iter().find(|(other, _)| *other == name) {
            assert_eq!(
                audited,
                Err(stopped(&Document::record([("reason", (*code).into())]))),
                "{name}"
            );
            assert!(recorded.get("text").is_some(), "{name}");
            let output = place.join("report.json");
            let words = [
                "--rootfs",
                root.to_str().expect("a path"),
                "--output",
                output.to_str().expect("a path"),
            ];
            assert_eq!(
                checked(&words.map(str::to_owned)),
                Some(Ok(false)),
                "{name}"
            );
            let written = std::fs::read_to_string(&output).expect("a report");
            assert_eq!(
                written,
                format!("{{\n  \"passed\": false,\n  \"reason\": \"{code}\"\n}}\n"),
                "{name}"
            );
            tally.3 += 1;
            continue;
        }
        match &audited {
            Ok(report) => {
                same_text(report, recorded, name);
                tally.0 += 1;
            }
            Err(stop) => {
                assert_eq!(stop, &stopped(recorded), "{name}");
                tally.1 += usize::from(matches!(stop, Stop::Refused(_)));
                tally.2 += usize::from(!matches!(stop, Stop::Refused(_)));
            }
        }
        // The command writes the same report, or says that the files could
        // not be read, and ends with whether the audit passed.
        let output = place.join("report.json");
        let words = [
            "--rootfs",
            root.to_str().expect("a path"),
            "--platform",
            platform_name,
            "--output",
            output.to_str().expect("a path"),
        ];
        let ended = checked(&words.map(str::to_owned));
        restore(&place.join("root"), field(case, "tree"));
        let command = field(case, "command");
        let Some(status) = field(command, "status").whole() else {
            assert_eq!((ended, output.exists()), (None, false), "{name}");
            assert_eq!(
                field(command, "status")
                    .get("exit")
                    .and_then(Document::whole),
                Some(2),
                "{name}"
            );
            continue;
        };
        assert_eq!(ended, Some(Ok(status == 0)), "{name}");
        assert_eq!(status == 0, audited.is_ok(), "{name}");
        let written = std::fs::read_to_string(&output).expect("a report");
        assert_eq!(written, text(command, "text"), "{name}");
        let mode = output.metadata().expect("a report").permissions().mode() & 0o7777;
        assert_eq!(u64::from(mode), number(field(command, "mode")), "{name}");
    }
    if !administrator() {
        assert_eq!(tally, (19, 40, 3, 10));
    }
}

#[test]
fn every_recorded_command_line_ends_alike() {
    let recording = recording();
    let scratch = Scratch::new("elf-commands");
    let fixtures = field(&recording, "fixtures");
    let usual = field(&list(&recording, "audits")[0], "tree");
    // Audited, no command line for either, refused here alone, and a
    // report that cannot be written.
    let mut tally = (0, 0, 0, 0);
    for (index, case) in list(&recording, "commands").iter().enumerate() {
        let name = text(case, "name");
        let place = scratch.0.join(index.to_string());
        std::fs::create_dir_all(place.join("root")).expect("a root");
        planted(&place.join("root"), usual, fixtures);
        let output = place.join("report.json");
        match field(case, "existing").text() {
            Some("file") => std::fs::write(&output, "there\n").expect("a file"),
            Some("link") => std::os::unix::fs::symlink("elsewhere.json", &output).expect("a link"),
            Some("directory") => std::fs::create_dir(&output).expect("a directory"),
            Some(other) => panic!("{other} is not prepared"),
            None => {}
        }
        let spelled = place.display().to_string();
        let words: Vec<String> = list(case, "arguments")
            .iter()
            .map(|word| word.text().expect("a word").replace("<place>", &spelled))
            .collect();
        let ended = checked(&words);
        let mut left: Vec<(String, Document)> = Vec::new();
        let mut entries: Vec<_> = std::fs::read_dir(&place)
            .expect("the place")
            .map(|entry| entry.expect("an entry").path())
            .collect();
        entries.sort();
        for entry in entries.iter().filter(|entry| !entry.ends_with("root")) {
            let plain = entry.symlink_metadata().expect("an entry").is_file();
            let content = plain.then(|| std::fs::read_to_string(entry).expect("text"));
            left.push((
                entry
                    .file_name()
                    .expect("a name")
                    .to_string_lossy()
                    .into_owned(),
                content.into(),
            ));
        }
        let status = field(case, "status");
        if LENIENT.contains(&name) {
            assert_eq!((ended, left.is_empty()), (None, true), "{name}");
            assert!(status.whole().is_some(), "{name}");
            tally.2 += 1;
        } else if let Some(passed) = status.whole() {
            assert_eq!(ended, Some(Ok(passed == 0)), "{name}");
            assert_eq!(&Document::Record(left), field(case, "left"), "{name}");
            let mode = output.metadata().expect("a report").permissions().mode() & 0o7777;
            assert_eq!(u64::from(mode), number(field(case, "mode")), "{name}");
            tally.0 += 1;
        } else if status.get("exit").is_some() {
            assert_eq!((ended, left.is_empty()), (None, true), "{name}");
            tally.1 += 1;
        } else {
            let code = match text(status, "raised") {
                "FileExistsError" => "tool_entry_unavailable",
                "FileNotFoundError" => "tool_entry_missing",
                other => panic!("{other} is not named"),
            };
            assert_eq!(ended, Some(Err(Fault(code))), "{name}");
            assert_eq!(&Document::Record(left), field(case, "left"), "{name}");
            tally.3 += 1;
        }
    }
    assert_eq!(tally, (5, 9, 6, 4));
}
