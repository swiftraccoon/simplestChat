//! Replays of `recorded_verify.json`: whole verifications by the reference
//! in Python over small vendored directories, with the sources in the
//! cache. Each case is built again here and must end the same way and
//! leave the same evidence, byte for byte. Where the reference failed in
//! the words of its interpreter, the tables give the words here.

use super::{Network, Verification, asked, source_bytes, sources, verify};
use crate::{
    document::Document,
    scratch::{Scratch, base64},
    tools::{Answer, Transport},
    vendor::{Bounds, Problem, manifest::Source},
};
use std::{
    fs::Permissions,
    os::unix::fs::{DirBuilderExt, PermissionsExt, symlink},
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

const RECORDING: &[u8] = include_bytes!("recorded_verify.json");

/// Cases that both refuse in other words: the reference in those of its
/// interpreter's system errors and readers.
const REWORDED: [(&str, &str); 14] = [
    (
        "a source that is a link in the cache",
        "Unreadable file: <root>/cache/c0c56958ef8be5c1979366896b7e0c7206949a5aa2b23f51429c7f56b10990d3",
    ),
    (
        "a source that is a dangling link in the cache",
        "Unreadable file: <root>/cache/c0c56958ef8be5c1979366896b7e0c7206949a5aa2b23f51429c7f56b10990d3",
    ),
    (
        "a link to a file",
        "Unreadable file: <root>/vendor/pkg/link",
    ),
    ("a dangling link", "Unreadable file: <root>/vendor/pkg/link"),
    (
        "a file that cannot be read",
        "Unreadable file: <root>/vendor/pkg/same",
    ),
    (
        "a cache below a directory that is not there",
        "Directory cannot be made: <root>/absent/cache",
    ),
    (
        "an output that is there",
        "Directory cannot be made: <root>/evidence",
    ),
    (
        "an output that is a file",
        "Directory cannot be made: <root>/evidence",
    ),
    (
        "an output that is a dangling link",
        "Directory cannot be made: <root>/evidence",
    ),
    (
        "an output below a directory that is not there",
        "Directory cannot be made: <root>/absent/evidence",
    ),
    ("no record", "Missing vendor file: vendor/integrity.json"),
    ("a record that is no JSON", "Invalid JSON"),
    (
        "a tree that is said to be a zip and is not",
        "Unreadable zip archive",
    ),
    (
        "a file with a name that is no ASCII",
        "Unsafe or noncanonical path: 'pkg/caf\\xe9'",
    ),
];

/// Cases that are refused here before anything is compared, where the
/// reference passed over a directory it could not read: it then passed, or
/// complained of the files it had not seen.
const STRICTER: [(&str, &str); 2] = [
    (
        "an empty directory that cannot be read",
        "Unreadable vendor directory: <root>/vendor/pkg/closed",
    ),
    (
        "a directory of files that cannot be read",
        "Unreadable vendor directory: <root>/vendor/pkg/deep",
    ),
];

/// A network that must not be asked.
struct Unasked;

impl Transport for Unasked {
    fn now(&mut self) -> Duration {
        Duration::ZERO
    }

    fn pause(&mut self, _: Duration) {}

    fn fetch(&mut self, address: &str, _: u64, _: Duration) -> Answer {
        panic!("{address} was asked for")
    }
}

fn text<'a>(case: &'a Document, name: &str) -> &'a str {
    let held = case.get(name).and_then(Document::text);
    held.unwrap_or_else(|| panic!("no text {name}"))
}

fn fields<'a>(case: &'a Document, name: &str) -> &'a [(String, Document)] {
    let held = case.get(name).and_then(Document::fields);
    held.unwrap_or_else(|| panic!("no record {name}"))
}

fn names<'a>(case: &'a Document, name: &str) -> impl Iterator<Item = &'a str> {
    let held = case.get(name).and_then(Document::items);
    let held = held.unwrap_or_else(|| panic!("no list {name}"));
    held.iter().map(|item| item.text().expect("a name"))
}

fn mode(value: &Document) -> u32 {
    u32::try_from(value.whole().expect("a mode")).expect("a mode")
}

fn put(path: &Path, content: &[u8]) {
    std::fs::create_dir_all(path.parent().expect("a parent")).expect("a directory");
    std::fs::write(path, content).expect("a file");
}

fn private(path: &Path) {
    let made = std::fs::DirBuilder::new().mode(0o700).create(path);
    made.expect("a private directory");
}

/// Build the case below `root` as the recorder built it, and answer its
/// cache and where its evidence goes.
fn built(root: &Path, case: &Document) -> (PathBuf, PathBuf) {
    for (path, body) in fields(case, "files") {
        put(&root.join(path), &base64(body.text().expect("bytes")));
    }
    for path in names(case, "directories") {
        std::fs::create_dir_all(root.join(path)).expect("a directory");
    }
    let vendor = root.join("vendor");
    match text(case, "vendor_state") {
        "link" => {
            std::fs::rename(&vendor, root.join("real-vendor")).expect("a move");
            symlink("real-vendor", &vendor).expect("a link");
        }
        "file" => put(&vendor, b"a file\n"),
        _ => {}
    }
    if let Some(record) = case.get("record").and_then(Document::text) {
        let padding = case.get("padding").and_then(Document::whole);
        let padding = usize::try_from(padding.expect("padding")).expect("padding");
        let record = format!("{record}{}", " ".repeat(padding));
        put(&vendor.join("integrity.json"), record.as_bytes());
    }
    for (path, target) in fields(case, "links") {
        symlink(target.text().expect("a target"), root.join(path)).expect("a link");
    }
    for path in names(case, "pipes") {
        let made = Command::new("mkfifo").arg(root.join(path)).status();
        assert!(made.expect("mkfifo").success());
    }
    let state = text(case, "cache_state");
    let mut cache = root.join("cache");
    let held = if state == "link" {
        root.join("held-cache")
    } else {
        cache.clone()
    };
    match state {
        "there" | "link" => {
            private(&held);
            for (name, body) in fields(case, "cache") {
                put(&held.join(name), &base64(body.text().expect("bytes")));
            }
            let links = fields(case, "cacheLinks");
            for (name, target) in links {
                symlink(target.text().expect("a target"), held.join(name)).expect("a link");
            }
            if !links.is_empty() {
                put(&held.join("elsewhere"), b"license\n");
            }
            let mode = Permissions::from_mode(mode(case.get("cacheMode").expect("a mode")));
            std::fs::set_permissions(&held, mode).expect("a mode");
            if state == "link" {
                symlink("held-cache", &cache).expect("a link");
            }
        }
        "file" => put(&cache, b"a file\n"),
        "private-file" => {
            put(&cache, b"a file\n");
            let mode = Permissions::from_mode(0o600);
            std::fs::set_permissions(&cache, mode).expect("a mode");
        }
        "orphan" => cache = root.join("absent/cache"),
        _ => {}
    }
    let mut output = root.join("evidence");
    match text(case, "output_state") {
        "there" => private(&output),
        "file" => put(&output, b"a file\n"),
        "link" => symlink("nowhere", &output).expect("a link"),
        "orphan" => output = root.join("absent/evidence"),
        _ => {}
    }
    for (path, held) in fields(case, "modes") {
        let mode = Permissions::from_mode(mode(held));
        std::fs::set_permissions(root.join(path), mode).expect("a mode");
    }
    (cache, output)
}

/// The names in the directory `path`, in their order.
fn listed(path: &Path) -> Vec<String> {
    let entries = std::fs::read_dir(path).expect("a directory");
    let name = |entry: std::io::Result<std::fs::DirEntry>| {
        let name = entry.expect("an entry").file_name();
        name.into_string().expect("a name")
    };
    let mut names: Vec<String> = entries.map(name).collect();
    names.sort();
    names
}

/// Replay one case and answer whether it passed alike, was refused alike,
/// was reworded or was refused here alone.
fn replayed(index: usize, case: &Document) -> usize {
    let name = text(case, "name");
    let scratch = Scratch::new(&format!("vendor-verify-{index}"));
    let root = &scratch.0;
    let (cache, output) = built(root, case);
    let mut unasked = Unasked;
    let mut network = Network {
        transport: &mut unasked,
        deadline: Duration::from_secs(1),
    };
    let offline = case.get("offline") == Some(&Document::Bool(true));
    let network = (!offline).then_some(&mut network);
    let asked = Verification {
        root,
        cache: &cache,
        output: &output,
        bounds: Bounds::REVIEWED,
    };
    let verified = verify(&asked, network);
    for (path, _) in fields(case, "modes") {
        let mode = Permissions::from_mode(0o700);
        std::fs::set_permissions(root.join(path), mode).expect("a mode");
    }
    let shown = root.to_str().expect("a path");
    let here = verified
        .err()
        .map(|Problem(words)| words.replace(shown, "<root>"));
    let here = here.as_deref();
    let recorded = case.get("result").expect("a result");
    // What the reference complained of, or what its interpreter said when
    // it was the interpreter that failed.
    let there = recorded.get("refused").or_else(|| recorded.get("said"));
    let there = there.and_then(Document::text);
    let evidence = case.get("evidence").and_then(Document::fields);
    let stricter = STRICTER.iter().find(|(other, _)| *other == name);
    let reworded = REWORDED.iter().find(|(other, _)| *other == name);
    let kind = if let Some((_, words)) = stricter {
        assert_eq!(here, Some(*words), "{name}");
        assert_eq!(listed(&output), [""; 0], "{name}");
        return 3;
    } else if let Some((_, words)) = reworded {
        assert_eq!(here, Some(*words), "{name}");
        assert!(recorded.get("passed").is_none() && there != here, "{name}");
        2
    } else {
        assert_eq!(here, there, "{name}");
        let passed = recorded.get("passed").is_some();
        assert_eq!(passed, here.is_none(), "{name}");
        usize::from(!passed)
    };
    if let Some(evidence) = evidence {
        let wanted: Vec<&str> = evidence.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(listed(&output), wanted, "{name}");
        let mode = output.metadata().expect("evidence").permissions();
        assert_eq!(mode.mode() & 0o777, 0o700, "{name}");
        for (file, content) in evidence {
            let written = std::fs::read(output.join(file)).expect("evidence");
            assert_eq!(
                written,
                base64(content.text().expect("bytes")),
                "{name} {file}"
            );
            let mode = output
                .join(file)
                .metadata()
                .expect("evidence")
                .permissions();
            assert_eq!(mode.mode() & 0o777, 0o600, "{name} {file}");
        }
    }
    if let Some(cached) = case.get("cached").and_then(Document::items) {
        let cached: Vec<&str> = cached.iter().filter_map(Document::text).collect();
        assert_eq!(listed(&cache), cached, "{name}");
    }
    kind
}

#[test]
fn every_recorded_verification_ends_alike_with_the_same_evidence() {
    let recording = Document::read(RECORDING).expect("a recording");
    let cases = recording.get("cases").and_then(Document::items);
    let mut tally = [0; 4];
    for (index, case) in cases.expect("cases").iter().enumerate() {
        tally[replayed(index, case)] += 1;
    }
    assert_eq!(tally, [8, 40, 14, 2]);
}

/// The cases of the reference's own deleted tests, built from their
/// fixture, end alike with the same evidence.
#[test]
fn every_case_of_the_reference_tests_ends_alike() {
    let recording = Document::read(super::super::tests::TESTS).expect("a recording");
    let cases = recording.get("verifications").and_then(Document::items);
    let mut tally = [0; 4];
    for (index, case) in cases.expect("cases").iter().enumerate() {
        tally[replayed(1000 + index, case)] += 1;
    }
    assert_eq!(tally, [1, 13, 0, 0]);
}

/// Evidence that is there is never written over: a second verification
/// into it is refused as the reference refused an output that was there,
/// and the first report stays as it was.
#[test]
fn existing_evidence_is_never_written_over() {
    let recording = Document::read(RECORDING).expect("a recording");
    let cases = recording.get("cases").and_then(Document::items);
    let cases = cases.expect("cases");
    let named = |name: &str| cases.iter().find(|case| text(case, "name") == name);
    let (passed, there) = (
        named("everything as recorded"),
        named("an output that is there"),
    );
    let (passed, there) = (passed.expect("a case"), there.expect("a case"));
    let scratch = Scratch::new("vendor-verify-twice");
    let root = &scratch.0;
    let (cache, output) = built(root, passed);
    let asked = Verification {
        root,
        cache: &cache,
        output: &output,
        bounds: Bounds::REVIEWED,
    };
    assert_eq!(verify(&asked, None), Ok(()));
    let report = std::fs::read(output.join("report.json")).expect("a report");
    let words = REWORDED
        .iter()
        .find(|(name, _)| *name == text(there, "name"));
    let words = words
        .expect("reworded")
        .1
        .replace("<root>", root.to_str().expect("a path"));
    assert_eq!(verify(&asked, None), Err(Problem(words)));
    assert_eq!(
        std::fs::read(output.join("report.json")).expect("a report"),
        report
    );
}

/// The most bytes a source may have.
const LIMIT: usize = Bounds::REVIEWED.source;

/// A network that answers from a list and notes what it was asked.
struct Scripted {
    now: Duration,
    answers: Vec<Answer>,
    asked: Vec<(String, u64, Duration)>,
}

impl Transport for Scripted {
    fn now(&mut self) -> Duration {
        self.now
    }

    fn pause(&mut self, _: Duration) {
        panic!("a source is asked for once")
    }

    fn fetch(&mut self, address: &str, limit: u64, within: Duration) -> Answer {
        self.asked.push((address.to_owned(), limit, within));
        self.answers.remove(0)
    }
}

fn source(body: &[u8]) -> Source {
    Source {
        identifier: "remote".to_owned(),
        url: "https://example.invalid/source".to_owned(),
        sha256: crate::digest(body),
        format: "file".to_owned(),
    }
}

fn body(bytes: &[u8], address: &str) -> Answer {
    Answer::Body {
        bytes: bytes.to_vec(),
        address: address.to_owned(),
    }
}

#[test]
fn a_source_the_cache_lacks_is_fetched_once_authenticated_and_kept() {
    let scratch = Scratch::new("vendor-fetch");
    let cache = scratch.0.join("cache");
    private(&cache);
    let pinned = source(b"good");
    let mut scripted = Scripted {
        now: Duration::from_secs(800),
        answers: vec![body(b"good", "https://cdn.example.invalid/a?b=c")],
        asked: Vec::new(),
    };
    let mut network = Network {
        transport: &mut scripted,
        deadline: Duration::from_secs(900),
    };
    let fetched = source_bytes(&pinned, &cache, Some(&mut network), LIMIT);
    assert_eq!(fetched, Ok(b"good".to_vec()));
    let again = source_bytes(&pinned, &cache, Some(&mut network), LIMIT);
    assert_eq!(again, Ok(b"good".to_vec()));
    let offline = source_bytes(&pinned, &cache, None, LIMIT);
    assert_eq!(offline, Ok(b"good".to_vec()));
    // Asked once, for no more than a source may have, within what is left
    // of the time every source has together.
    let asked = (
        "https://example.invalid/source".to_owned(),
        128 * 1024 * 1024,
    );
    let within = Duration::from_secs(100);
    assert_eq!(scripted.asked, [(asked.0, asked.1, within)]);
    assert_eq!(listed(&cache), std::slice::from_ref(&pinned.sha256));
    let kept = cache.join(&pinned.sha256).metadata().expect("a source");
    assert_eq!(kept.permissions().mode() & 0o777, 0o600);
}

#[test]
fn a_source_is_given_two_minutes_and_none_when_no_time_is_left() {
    let scratch = Scratch::new("vendor-time");
    let cache = scratch.0.join("cache");
    private(&cache);
    let pinned = source(b"good");
    let mut scripted = Scripted {
        now: Duration::ZERO,
        answers: vec![Answer::Late],
        asked: Vec::new(),
    };
    let mut network = Network {
        transport: &mut scripted,
        deadline: Duration::from_secs(900),
    };
    let late = source_bytes(&pinned, &cache, Some(&mut network), LIMIT);
    let words = "Source download failed: remote: no answer in time";
    assert_eq!(late, Err(Problem(words.to_owned())));
    network.deadline = Duration::ZERO;
    let spent = source_bytes(&pinned, &cache, Some(&mut network), LIMIT);
    let words = "Source download failed: remote: no time is left";
    assert_eq!(spent, Err(Problem(words.to_owned())));
    assert_eq!(scripted.asked.len(), 1);
    assert_eq!(scripted.asked[0].2, Duration::from_secs(120));
    assert_eq!(listed(&cache), [""; 0]);
}

#[test]
fn what_was_fetched_is_kept_only_when_it_is_what_the_record_pins() {
    let scratch = Scratch::new("vendor-refused");
    let cache = scratch.0.join("cache");
    private(&cache);
    let pinned = source(b"good");
    let failed = |reason: &str| format!("Source download failed: remote: {reason}");
    for (answer, words) in [
        (
            body(b"wrong", "https://example.invalid/source"),
            "Source SHA-256 differs: remote".to_owned(),
        ),
        (
            body(b"good", "http://example.invalid/source"),
            failed("its last address is no plain HTTPS address"),
        ),
        (
            body(b"good", "https://user@example.invalid/source"),
            failed("its last address is no plain HTTPS address"),
        ),
        (Answer::Status(404), failed("HTTP status 404")),
        (Answer::Oversized, failed("more bytes than its budget")),
        (Answer::Failed, failed("no response")),
    ] {
        let mut scripted = Scripted {
            now: Duration::ZERO,
            answers: vec![answer],
            asked: Vec::new(),
        };
        let mut network = Network {
            transport: &mut scripted,
            deadline: Duration::from_secs(900),
        };
        let refused = source_bytes(&pinned, &cache, Some(&mut network), LIMIT);
        assert_eq!(refused, Err(Problem(words)));
        assert_eq!(listed(&cache), [""; 0]);
    }
    // A source whose address the record's rules refuse is never asked for.
    let mut unasked = Unasked;
    let mut network = Network {
        transport: &mut unasked,
        deadline: Duration::from_secs(900),
    };
    let insecure = Source {
        url: "http://example.invalid/source".to_owned(),
        ..pinned
    };
    let refused = source_bytes(&insecure, &cache, Some(&mut network), LIMIT);
    let words = "Source URLs must be credential-free HTTPS URLs";
    assert_eq!(refused, Err(Problem(words.to_owned())));
}

#[test]
fn a_command_line_names_a_cache_and_new_evidence_and_nothing_twice() {
    let root = Path::new("/checkout");
    let line = |words: &[&str]| {
        let words: Vec<String> = words.iter().map(|word| (*word).to_owned()).collect();
        asked(root, &words)
    };
    let (cache, output) = (PathBuf::from("c"), PathBuf::from("o"));
    let usual = (root.to_path_buf(), cache.clone(), output.clone(), false);
    assert_eq!(
        line(&["verify", "--cache", "c", "--output", "o"]),
        Some(usual)
    );
    let named = (PathBuf::from("r"), cache, output, true);
    let all = [
        "verify",
        "--offline",
        "--output",
        "o",
        "--root",
        "r",
        "--cache",
        "c",
    ];
    assert_eq!(line(&all), Some(named));
    for refused in [
        &[][..],
        &["verify"],
        &["verify", "--cache", "c"],
        &["verify", "--output", "o"],
        &["check", "--cache", "c", "--output", "o"],
        &["_download", "--cache", "c", "--output", "o"],
        &[
            "verify",
            "--cache",
            "c",
            "--output",
            "o",
            "--offline",
            "--offline",
        ],
        &["verify", "--cache", "c", "--cache", "c", "--output", "o"],
        &["verify", "--cache", "c", "--output", "o", "--output", "o"],
        &[
            "verify", "--cache", "c", "--output", "o", "--root", "r", "--root", "r",
        ],
        &["verify", "--cache", "c", "--output", "o", "--root"],
        &["verify", "--cache", "", "--output", "o"],
        &["verify", "--cache", "--output", "o"],
        &["verify", "--cache", "--output", "--output", "o"],
        &["verify", "--cache", "c", "--output", "o", "more"],
        &["verify", "--cache", "c", "--output", "o", "--url", "u"],
    ] {
        assert_eq!(line(refused), None, "{refused:?}");
    }
}

/// The recorded case `name`, built in a directory of its own, and what
/// verifying it within `bounds` comes to with what it leaves as evidence.
fn bounded(name: &str, place: &str, bounds: Bounds) -> (Result<(), String>, Vec<String>) {
    let recording = Document::read(RECORDING).expect("a recording");
    let cases = recording.get("cases").and_then(Document::items);
    let named = |case: &&Document| text(case, "name") == name;
    let case = cases.expect("cases").iter().find(named).expect("a case");
    let scratch = Scratch::new(place);
    let (cache, output) = built(&scratch.0, case);
    let asked = Verification {
        root: &scratch.0,
        cache: &cache,
        output: &output,
        bounds,
    };
    let verified = verify(&asked, None).map_err(|Problem(words)| words);
    (verified, listed(&output))
}

#[test]
fn a_verification_is_held_to_its_bounds_at_their_edges() {
    const REVIEWED: Bounds = Bounds::REVIEWED;
    let recording = Document::read(RECORDING).expect("a recording");
    let cases = recording.get("cases").and_then(Document::items);
    let case = |name: &str| {
        let named = |case: &&Document| text(case, "name") == name;
        cases.expect("cases").iter().find(named).expect("a case")
    };
    // The record is no longer than its bound.
    let usual = "everything as recorded";
    let record = text(case(usual), "record").len();
    let passed = (
        Ok(()),
        vec!["report.json".to_owned(), "vendor.diff".to_owned()],
    );
    let within = Bounds { record, ..REVIEWED };
    assert_eq!(bounded(usual, "vendor-record-at", within), passed);
    let record = record - 1;
    let large = (Err("Manifest size limit exceeded".to_owned()), Vec::new());
    let beyond = Bounds { record, ..REVIEWED };
    assert_eq!(bounded(usual, "vendor-record-past", beyond), large);
    // The differences of every tree together are no more than their bound,
    // where each tree's alone are.
    let wrong = "several things wrong at once";
    let evidence = case(wrong).get("evidence").expect("evidence");
    let differences = base64(text(evidence, "vendor.diff")).len();
    let within = Bounds {
        differences,
        ..REVIEWED
    };
    let (verified, left) = bounded(wrong, "vendor-diff-at", within);
    let complaint = text(case(wrong).get("result").expect("a result"), "refused");
    assert_eq!(verified, Err(complaint.to_owned()));
    assert_eq!(left, ["failure.json", "vendor.diff"]);
    let differences = differences - 1;
    let beyond = Bounds {
        differences,
        ..REVIEWED
    };
    let budget = (Err("Vendor diff budget exceeded".to_owned()), Vec::new());
    assert_eq!(bounded(wrong, "vendor-diff-past", beyond), budget);
}

#[test]
fn the_sources_together_and_each_alone_are_held_to_their_bounds() {
    const REVIEWED: Bounds = Bounds::REVIEWED;
    let recording = Document::read(RECORDING).expect("a recording");
    let cases = recording.get("cases").and_then(Document::items);
    let case = &cases.expect("cases")[0];
    let scratch = Scratch::new("vendor-sources");
    let (cache, _) = built(&scratch.0, case);
    let manifest = crate::vendor::manifest::parse(text(case, "record").as_bytes());
    let manifest = manifest.expect("a record");
    let held = fields(case, "cache").iter();
    let sizes: Vec<usize> = held
        .map(|(_, body)| base64(body.text().expect("bytes")).len())
        .collect();
    let read = |bounds: Bounds| {
        let read = sources(&manifest, &cache, None, &bounds);
        read.map(|(held, records)| (held.len(), records.len()))
    };
    let content = sizes.iter().sum::<usize>() as u64;
    assert_eq!(
        read(Bounds {
            content,
            ..REVIEWED
        }),
        Ok((4, 4))
    );
    let content = content - 1;
    let together = "Combined source byte budget exceeded".to_owned();
    assert_eq!(
        read(Bounds {
            content,
            ..REVIEWED
        }),
        Err(Problem(together))
    );
    let source = *sizes.iter().max().expect("a source");
    assert_eq!(read(Bounds { source, ..REVIEWED }), Ok((4, 4)));
    let source = source - 1;
    let refused = read(Bounds { source, ..REVIEWED });
    let Err(Problem(alone)) = refused else {
        panic!("a source past its bound was read")
    };
    assert!(alone.starts_with("Not a bounded regular file: "), "{alone}");
}

#[test]
fn the_reviewed_bounds_are_the_references() {
    let megabyte = 1024 * 1024;
    let reviewed = Bounds {
        source: 128 * megabyte,
        member: 32 * megabyte as u64,
        content: 256 * megabyte as u64,
        entries: 20000,
        record: 2 * megabyte,
        differences: 64 * megabyte,
    };
    assert_eq!(Bounds::REVIEWED, reviewed);
    assert_eq!(super::DOWNLOAD, Duration::from_secs(120));
    assert_eq!(super::WHOLE, Duration::from_secs(900));
}

#[test]
fn what_an_interrupted_fetch_left_does_not_stop_the_next() {
    let scratch = Scratch::new("vendor-stale");
    let cache = scratch.0.join("cache");
    private(&cache);
    let pinned = source(b"good");
    let stale = format!("download-{}", pinned.sha256);
    put(&cache.join(&stale), b"half of it");
    let mut scripted = Scripted {
        now: Duration::ZERO,
        answers: vec![body(b"good", "https://example.invalid/source")],
        asked: Vec::new(),
    };
    let mut network = Network {
        transport: &mut scripted,
        deadline: Duration::from_secs(900),
    };
    let fetched = source_bytes(&pinned, &cache, Some(&mut network), LIMIT);
    assert_eq!(fetched, Ok(b"good".to_vec()));
    assert_eq!(listed(&cache), std::slice::from_ref(&pinned.sha256));
}

#[test]
fn a_source_that_the_cache_cannot_keep_is_refused() {
    let scratch = Scratch::new("vendor-unkept");
    let cache = scratch.0.join("cache");
    private(&cache);
    let closed = Permissions::from_mode(0o500);
    std::fs::set_permissions(&cache, closed).expect("a mode");
    let pinned = source(b"good");
    let mut scripted = Scripted {
        now: Duration::ZERO,
        answers: vec![body(b"good", "https://example.invalid/source")],
        asked: Vec::new(),
    };
    let mut network = Network {
        transport: &mut scripted,
        deadline: Duration::from_secs(900),
    };
    let fetched = source_bytes(&pinned, &cache, Some(&mut network), LIMIT);
    let open = Permissions::from_mode(0o700);
    std::fs::set_permissions(&cache, open).expect("a mode");
    let words = "Source cannot be kept: remote".to_owned();
    assert_eq!(fetched, Err(Problem(words)));
    assert_eq!(listed(&cache), [""; 0]);
}
