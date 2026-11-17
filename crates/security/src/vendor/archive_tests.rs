//! Replays of `recorded_archives.json`: what the reference in Python read
//! from tar.gz and zip archives built with its interpreter's own writers.
//! Archives are kept deflated. Where the reference failed in the words of
//! its gzip, tar or zip library, the tables give the words here.

use super::{files, tar_members, zip_members};
use crate::{
    digest,
    document::Document,
    scratch::base64,
    vendor::{Bounds, Problem, Verdict, compare::Files, manifest::Source},
};
use std::io::Read;

const RECORDING: &[u8] = include_bytes!("recorded_archives.json");

const GZIP: &str = "Unreadable gzip stream";
const TAR: &str = "Unreadable tar archive";
const ZIP: &str = "Unreadable zip archive";
const ESCAPED: &str = "Unsafe or noncanonical path: 'pkg-1.0/caf\\xe9'";

/// Tar archives that both refuse in other words: the reference in those of
/// its gzip and tar libraries, and with a name that is no ASCII shown as it
/// is. A sparse member and a header without its file are found here by the
/// reader of the archive; so are a pax sparse map that the reference's
/// library failed to read as numbers, once in the member's own bytes and
/// once after two headers for the whole archive had it read the map twice,
/// and such a header that no member follows.
const REWORDED_TARS: [(&str, &str); 18] = [
    ("nothing", GZIP),
    ("no gzip", GZIP),
    ("text", GZIP),
    ("gzip cut short", GZIP),
    ("gzip with a wrong check", GZIP),
    ("gzip and more bytes", GZIP),
    ("gzip of text", TAR),
    ("gzip of nothing", TAR),
    ("a tar cut short inside a file", TAR),
    ("a tar with a damaged header check", TAR),
    ("a sparse file of the GNU kind", TAR),
    ("a file larger than its bytes", TAR),
    ("a tar cut short after a header", "Tar member size differs"),
    ("a name that is no ASCII", ESCAPED),
    ("a name that is no ASCII in the GNU format", ESCAPED),
    (
        "a sparse member of pax 1.0 whose map is no number",
        "Unsupported tar member type",
    ),
    (
        "two global headers before a member",
        "Unsupported tar member type",
    ),
    ("a global sparse map after the members", TAR),
];

/// Zip archives that both refuse in other words.
const REWORDED_ZIPS: [(&str, &str); 9] = [
    ("nothing", ZIP),
    ("text", ZIP),
    ("an archive cut short", ZIP),
    ("a stored member that is damaged", ZIP),
    ("a name that is no ASCII", ESCAPED),
    (
        "a name with a high byte and no flag",
        "Unsafe or noncanonical path: 'pkg-1.0/a\\u0398b'",
    ),
    ("a Unicode path field that is cut short", ZIP),
    ("an extra field that is cut short", ZIP),
    ("a member whose own header names it otherwise", ZIP),
];

/// Sources of one format that are read as the other.
const REWORDED_SOURCES: [(&str, &str); 2] = [
    ("a tarball said to be a zip", ZIP),
    ("a zip said to be a tarball", GZIP),
];

/// Archives the reference read and that are refused here: zip members
/// packed by another method than the two that every reader has, and a
/// file whose extended header names it as a directory is named, which
/// the reference's library took for the file without the slash.
const STRICTER: [(&str, &str); 3] = [
    ("a member packed with bzip2", "Unsupported zip member type"),
    ("a member packed with lzma", "Unsupported zip member type"),
    (
        "an extended header that names the next file with a slash",
        "Unsafe or noncanonical path: 'pkg-1.0/new.c/'",
    ),
];

fn text<'a>(document: &'a Document, name: &str) -> &'a str {
    let held = document.get(name).and_then(Document::text);
    held.unwrap_or_else(|| panic!("no text {name}"))
}

/// The archive of a case, unpacked from how the recording keeps it.
fn archive(case: &Document) -> Vec<u8> {
    let kept = case.get("archive").expect("an archive");
    let mut data = Vec::new();
    let packed = base64(text(kept, "deflated"));
    let read = flate2::read::ZlibDecoder::new(&packed[..]).read_to_end(&mut data);
    read.expect("a deflated archive");
    assert_eq!(digest(&data), text(kept, "sha256"));
    data
}

/// Hold what was read in a case to its recorded result, and answer whether
/// it was read alike, refused alike, reworded or refused here alone.
fn alike(reworded: &[(&str, &str)], case: &Document, read: &Verdict<Files>) -> usize {
    let name = text(case, "name");
    let recorded = case.get("result").expect("a result");
    let here = read.as_ref().err().map(|Problem(words)| words.as_str());
    let there = recorded.get("refused").and_then(Document::text);
    if let Some((_, words)) = STRICTER.iter().find(|(other, _)| *other == name) {
        assert_eq!(here, Some(*words), "{name}");
        assert!(recorded.get("files").is_some(), "{name}");
        return 3;
    }
    if let Some((_, words)) = reworded.iter().find(|(other, _)| *other == name) {
        assert_eq!(here, Some(*words), "{name}");
        assert!(recorded.get("files").is_none() && there != here, "{name}");
        return 2;
    }
    let Some(listed) = recorded.get("files") else {
        assert_eq!(here, there, "{name}");
        assert!(there.is_some(), "{name}");
        return 1;
    };
    let read = read
        .as_ref()
        .unwrap_or_else(|problem| panic!("{name}: {problem}"));
    let identity = |(path, body): (&String, &Vec<u8>)| {
        let identity = vec![body.len().into(), digest(body).into()];
        (path.clone(), Document::List(identity))
    };
    let found: Vec<(String, Document)> = read.iter().map(identity).collect();
    let mut wanted = listed.fields().expect("files").to_vec();
    wanted.sort_by(|left, right| left.0.cmp(&right.0));
    assert_eq!(found, wanted, "{name}");
    0
}

#[test]
fn every_recorded_archive_is_read_alike() {
    let recording = Document::read(RECORDING).expect("a recording");
    let cases = |name: &str| {
        recording
            .get(name)
            .and_then(Document::items)
            .expect("cases")
    };
    let mut tally = [[0; 4]; 3];
    for case in cases("tars") {
        let read = tar_members(&archive(case), &Bounds::REVIEWED);
        tally[0][alike(&REWORDED_TARS, case, &read)] += 1;
    }
    for case in cases("zips") {
        let read = zip_members(&archive(case), &Bounds::REVIEWED);
        tally[1][alike(&REWORDED_ZIPS, case, &read)] += 1;
    }
    for case in cases("sources") {
        let source = Source {
            identifier: "upstream".to_owned(),
            url: "https://example.invalid/upstream".to_owned(),
            sha256: text(case, "sha256").to_owned(),
            format: text(case, "format").to_owned(),
        };
        let read = files(&source, &archive(case), &Bounds::REVIEWED);
        tally[2][alike(&REWORDED_SOURCES, case, &read)] += 1;
    }
    assert_eq!(tally, [[45, 35, 18, 1], [20, 18, 9, 2], [2, 3, 2, 0]]);
}

/// The archive of the recorded case `name`.
fn recorded(section: &str, name: &str) -> Vec<u8> {
    let recording = Document::read(RECORDING).expect("a recording");
    let cases = recording.get(section).and_then(Document::items);
    let named = |case: &&Document| text(case, "name") == name;
    archive(cases.expect("cases").iter().find(named).expect("a case"))
}

fn refused<T>(words: &str) -> Verdict<T> {
    Err(Problem(words.to_owned()))
}

#[test]
fn an_archive_is_held_to_every_bound_at_its_edge() {
    const REVIEWED: Bounds = Bounds::REVIEWED;
    type Reader = fn(&[u8], &Bounds) -> Verdict<Files>;
    let (tar, zip) = (recorded("tars", "two files"), recorded("zips", "two files"));
    // Both hold two files of two bytes each.
    for (reader, archive) in [(tar_members as Reader, &tar), (zip_members, &zip)] {
        let read = |bounds: Bounds| reader(archive, &bounds).map(|files| files.len());
        let (member, entries) = (2, 2);
        assert_eq!(read(Bounds { member, ..REVIEWED }), Ok(2));
        let member = member - 1;
        let budget = refused("Archive expansion budget exceeded");
        assert_eq!(read(Bounds { member, ..REVIEWED }), budget);
        assert_eq!(
            read(Bounds {
                entries,
                ..REVIEWED
            }),
            Ok(2)
        );
        let entries = entries - 1;
        let excess = refused("Duplicate or excessive archive members");
        assert_eq!(
            read(Bounds {
                entries,
                ..REVIEWED
            }),
            excess
        );
    }
    // A tar archive is unpacked whole before it is read, and no further
    // than the bound.
    let mut raw = Vec::new();
    let unpacked = flate2::read::GzDecoder::new(&tar[..]).read_to_end(&mut raw);
    let content = unpacked.expect("a gzip stream") as u64;
    let read = |content: u64| {
        let bounds = Bounds {
            content,
            ..REVIEWED
        };
        tar_members(&tar, &bounds).map(|files| files.len())
    };
    assert_eq!(read(content), Ok(2));
    assert_eq!(
        read(content - 1),
        refused("Archive expansion budget exceeded")
    );
    // A zip archive's members are counted as they are admitted.
    let read = |content: u64| {
        let bounds = Bounds {
            content,
            ..REVIEWED
        };
        zip_members(&zip, &bounds).map(|files| files.len())
    };
    assert_eq!(read(4), Ok(2));
    assert_eq!(read(3), refused("Archive expansion budget exceeded"));
    // A source is no longer than its bound, whatever its digest.
    let pinned = Source {
        identifier: "upstream".to_owned(),
        url: "https://example.invalid/upstream".to_owned(),
        sha256: digest(&tar),
        format: "tar.gz".to_owned(),
    };
    let read = |source: usize| {
        let bounds = Bounds { source, ..REVIEWED };
        files(&pinned, &tar, &bounds).map(|files| files.len())
    };
    assert_eq!(read(tar.len()), Ok(2));
    let differs = refused("Source SHA-256 or size differs: upstream");
    assert_eq!(read(tar.len() - 1), differs);
}

#[test]
fn a_zip_member_that_unpacks_to_more_than_it_says_is_refused() {
    // A megabyte of zero bytes, said by the archive's directory to be
    // `said` bytes.
    let read = |said: u32, member: u64| {
        let mut zip = recorded("zips", "a file of many zero bytes");
        let directory = zip.windows(4).rposition(|window| window == b"PK\x01\x02");
        let size = directory.expect("a directory") + 24;
        zip[size..size + 4].copy_from_slice(&said.to_le_bytes());
        let bounds = Bounds {
            member,
            ..Bounds::REVIEWED
        };
        zip_members(&zip, &bounds).map(|files| files.len())
    };
    let megabyte = 1024 * 1024;
    assert_eq!(read(megabyte, u64::from(megabyte)), Ok(1));
    assert_eq!(read(5, 4), refused("Archive expansion budget exceeded"));
    assert_eq!(read(5, 5), refused("Unreadable zip archive"));
    assert_eq!(
        read(5, 2 * u64::from(megabyte)),
        refused("Unreadable zip archive")
    );
    // And one that says more than its bound is not unpacked at all.
    let budget = refused("Archive expansion budget exceeded");
    assert_eq!(read(megabyte, u64::from(megabyte) - 1), budget);
}
