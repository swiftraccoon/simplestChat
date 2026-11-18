//! Replays of `recorded_sources.json`: what the reference made of
//! committed files, of authenticated archives, and of the native
//! sources of fixture checkouts with private caches, read through a `git
//! show` that serves the recorded commit and logs what it was asked.

use super::{NativeSources, archive_files, tracked_bytes};
use crate::{
    codeql::{
        Sourced,
        tests::{Committed, Tally, items, planted, recording, refusal, text},
    },
    digest,
    document::Document,
    scratch::{Scratch, base64},
    vendor::{Bounds, manifest::Source},
};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

const RECORDED: &[u8] = include_bytes!("recorded_sources.json");

/// The vendor's bounds with the reference's constants that `case` changed.
fn bounds(case: &Document) -> Bounds {
    let mut bounds = Bounds::REVIEWED;
    let given = case
        .get("bounds")
        .and_then(Document::fields)
        .unwrap_or_default();
    for (name, value) in given {
        let value = value.whole().expect("a bound");
        let size = usize::try_from(value).expect("a size");
        let bytes = u64::try_from(value).expect("a size");
        match name.as_str() {
            "MAX_DOWNLOAD" => bounds.source = size,
            "MAX_MEMBER" => bounds.member = bytes,
            "MAX_CONTENT" => bounds.content = bytes,
            "MAX_FILES" => bounds.entries = size,
            "MAX_MANIFEST" => bounds.record = size,
            other => panic!("unexpected bound {other}"),
        }
    }
    bounds
}

/// What came of a step here, as the tally takes it.
fn came<T>(
    place: &Path,
    outcome: Sourced<T>,
    held: impl FnOnce(T) -> Document,
) -> Result<Document, String> {
    outcome.map(held).map_err(|stop| refusal(&stop, place))
}

/// The refusal here of what the reference's interpreter raised, by its
/// class and words, where the case's own name does not say otherwise.
fn raised(class: &str, words: &str) -> String {
    match class {
        "ProcessError" => return words.to_owned(),
        "FileNotFoundError" => "tool_entry_missing",
        "OSError" => "tool_entry_unavailable",
        "IsADirectoryError" => "nonregular_tool_file",
        "KeyError" => "codeql_wrap_directory_missing",
        "UnicodeDecodeError"
        | "DuplicateOptionError"
        | "DuplicateSectionError"
        | "MissingSectionHeaderError"
        | "ParsingError" => "codeql_wrap_unread",
        "ValueError" => return format!("integrity: {words}"),
        "BadGzipFile" | "EOFError" => "integrity: Unreadable gzip stream",
        "ReadError" => "integrity: Unreadable tar archive",
        other => panic!("unexpected {other}"),
    }
    .to_owned()
}

/// The cases whose interpreter's failure is the vendor verifier's words
/// here, with those words: what reads the cache and the vendored tree says
/// what is wrong where the reference's interpreter failed to open it.
const WORDED: [(&str, &str); 4] = [
    (
        "no cache",
        "integrity: Directory must be owned and private (0700): <place>/cache",
    ),
    (
        "a local overlay whose directory is missing",
        "integrity: Not a source directory: <place>/vendor/mediasoup-sys-0.19.0/subprojects/packagefiles/fixture",
    ),
    (
        "a cache entry that is symlink",
        "integrity: Unreadable file: <place>/cache/6bf2393b25a689b2ceb209128ba39a21c3a08ecbb3a0183873a809bb9534d8f3",
    ),
    (
        "a cache entry that is directory",
        "integrity: Not a bounded regular file: <place>/cache/6bf2393b25a689b2ceb209128ba39a21c3a08ecbb3a0183873a809bb9534d8f3",
    ),
];

/// The cases the vendor verifier words otherwise than the reference did:
/// its quoting escapes every character outside printable ASCII, where the
/// reference's kept the printable ones.
const REWORDED: [(&str, &str); 2] = [
    (
        "a member outside ASCII",
        "Unsafe or noncanonical path: 'a/\\xe9'",
    ),
    (
        "a name outside ASCII",
        "Unsafe or noncanonical path: 'a/\\xe9.rs'",
    ),
];

/// A fresh place holding the tree of `case`.
fn placed(case: &Document) -> Scratch {
    let scratch = Scratch::new("codeql-sources");
    planted(&scratch.0, case.get("tree").expect("a tree"));
    scratch
}

#[test]
fn every_recorded_committed_file_is_read_alike() {
    let recorded = recording(RECORDED);
    let mut tally = Tally::default();
    for case in items(&recorded, "tracked") {
        let scratch = placed(case);
        let place = &scratch.0;
        let root = PathBuf::from(text(case, "root").replace("<place>", &place.to_string_lossy()));
        let git = Committed::new(&root, text(&recorded, "revision"), case, place);
        let limit = case.get("limit").and_then(Document::whole);
        let limit = limit.map_or(super::MAX_SOURCE, |limit| {
            u64::try_from(limit).expect("a bound")
        });
        let read = tracked_bytes(
            &root,
            text(case, "revision"),
            text(case, "path"),
            limit,
            &git,
        );
        let read = came(place, read, |data| {
            Document::record([
                ("sha256", digest(&data).into()),
                ("size", data.len().into()),
            ])
        });
        tally.judge(&reworded(case), read, &raised, &[]);
        assert_eq!(
            git.taken(),
            *case.get("asked").expect("asked"),
            "{}",
            text(case, "name")
        );
    }
    let expected = Tally {
        held: 4,
        refused: 30,
        raised: 7,
        stricter: 0,
    };
    assert_eq!(tally, expected);
}

/// `case` with the vendor verifier's own words where `REWORDED` names it.
fn reworded(case: &Document) -> Document {
    let mut case = case.clone();
    let name = text(&case, "name").to_owned();
    if let Some((_, words)) = REWORDED.iter().find(|(held, _)| *held == name) {
        let recorded = case
            .get("result")
            .and_then(|result| result.get("integrity"));
        assert!(recorded.is_some(), "{name} was refused there in words");
        case.put("result", Document::record([("integrity", (*words).into())]));
    }
    case
}

/// Files by their names and the SHA-256 of each, as the recording writes
/// them.
fn digests(files: &BTreeMap<String, Vec<u8>>) -> Document {
    let fields = files
        .iter()
        .map(|(name, data)| (name.clone(), Document::from(digest(data))));
    Document::Record(fields.collect())
}

#[test]
fn every_recorded_archive_is_read_alike() {
    let recorded = recording(RECORDED);
    let mut tally = Tally::default();
    for case in items(&recorded, "archives") {
        let source = Source {
            identifier: "fixture".to_owned(),
            url: "https://example.invalid/source".to_owned(),
            sha256: text(case, "sha256").to_owned(),
            format: text(case, "format").to_owned(),
        };
        let data = base64(text(case, "base64"));
        let read = archive_files(&source, &data, &bounds(case));
        let read = came(Path::new("/nowhere"), read, |files| digests(&files));
        let short = |class: &str, words: &str| match text(case, "name") {
            // The reference's reader failed inside the member before its
            // own length check could; here the check is what refuses it.
            "a tar cut inside a member" => "codeql_archive_member_size".to_owned(),
            // Its library failed to read a pax sparse map as numbers: in the
            // member's own bytes, and a second time after two headers for
            // the whole archive. Here the member is no file.
            "a sparse member of pax 1.0 whose map is no number"
            | "two global headers before a member" => "codeql_archive_kind".to_owned(),
            _ => raised(class, words),
        };
        tally.judge(&reworded(case), read, &short, &[]);
    }
    let expected = Tally {
        held: 29,
        refused: 40,
        raised: 8,
        stricter: 0,
    };
    assert_eq!(tally, expected);
}

#[test]
fn every_recorded_native_source_is_bound_alike() {
    let recorded = recording(RECORDED);
    let mut tally = Tally::default();
    let mut cases = 0;
    for case in items(&recorded, "native") {
        let scratch = placed(case);
        let place = &scratch.0;
        let name = text(case, "name");
        let git = Committed::new(place, text(&recorded, "revision"), case, place);
        let cache = PathBuf::from(text(case, "cache").replace("<place>", &place.to_string_lossy()));
        let steps = items(case, "steps");
        let worded = |class: &str, words: &str| match WORDED.iter().find(|(held, _)| *held == name)
        {
            Some((_, words)) => (*words).to_owned(),
            None => raised(class, words),
        };
        let made = NativeSources::new(place, text(case, "revision"), &cache, &git, bounds(case));
        let step = |index: usize, title: &str| {
            let mut step = steps[index].clone();
            step.put("name", format!("{name}: {title}").into());
            step
        };
        let mut resolver = match made {
            Ok(resolver) => {
                tally.judge(&step(0, "made"), Ok(Document::Null), &worded, &[]);
                resolver
            }
            Err(stop) => {
                let refusal = Err(refusal(&stop, place));
                tally.judge(&step(0, "made"), refusal, &worded, &[]);
                assert_eq!(steps.len(), 1, "{name}");
                cases += 1;
                continue;
            }
        };
        assert_eq!(
            git.taken(),
            *steps[0].get("asked").expect("asked"),
            "{name}"
        );
        for (index, recorded_step) in steps.iter().enumerate().skip(1) {
            let member = text(recorded_step, "name");
            let identity = resolver.identity(member);
            let identity = came(place, identity, Document::from);
            tally.judge(&step(index, member), identity, &worded, &[]);
            assert_eq!(
                git.taken(),
                *recorded_step.get("asked").expect("asked"),
                "{name}: {member}"
            );
        }
        cases += 1;
    }
    assert_eq!(cases, 85);
    let expected = Tally {
        held: 87,
        refused: 52,
        raised: 14,
        stricter: 0,
    };
    assert_eq!(tally, expected);
}

#[test]
fn the_callers_git_shows_exactly_what_was_committed_within_its_bound() {
    let scratch = Scratch::new("codeql-git");
    let root = &scratch.0;
    let git = crate::scratch::host_git();
    let run = |arguments: &[&str]| {
        let done = std::process::Command::new(&git)
            .args([
                "-c",
                "user.name=fixture",
                "-c",
                "user.email=fixture@example.invalid",
            ])
            .args(arguments)
            .current_dir(root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .output()
            .expect("git runs");
        assert!(done.status.success(), "git {arguments:?}");
        String::from_utf8(done.stdout)
            .expect("text")
            .trim()
            .to_owned()
    };
    run(&["init", "-q"]);
    std::fs::create_dir(root.join("a")).expect("a directory");
    std::fs::write(root.join("a/b.rs"), "fn b() {}\n").expect("a source");
    std::fs::write(root.join("a/long.rs"), "123456").expect("a source");
    run(&["add", "-A"]);
    run(&["commit", "-q", "-m", "fixture"]);
    let revision = run(&["rev-parse", "HEAD"]);
    let read = |name: &str, limit: u64| tracked_bytes(root, &revision, name, limit, &super::Git);
    assert_eq!(
        read("a/b.rs", super::MAX_SOURCE),
        Ok(b"fn b() {}\n".to_vec())
    );
    std::fs::write(root.join("a/c.rs"), "fn c() {}\n").expect("an untracked source");
    let mismatch = Err(crate::codeql::Stop::Code("codeql_source_revision_mismatch"));
    assert_eq!(read("a/c.rs", super::MAX_SOURCE), mismatch);
    std::fs::write(root.join("a/b.rs"), "fn edited() {}\n").expect("an edit");
    assert_eq!(read("a/b.rs", super::MAX_SOURCE), mismatch);
    std::fs::write(root.join("a/long.rs"), "12345").expect("a shorter copy");
    let over = Err(crate::codeql::Stop::Code("command_stdout_limit_exceeded"));
    assert_eq!(read("a/long.rs", 5), over);
    assert_eq!(read("a/long.rs", 6), mismatch);
}
