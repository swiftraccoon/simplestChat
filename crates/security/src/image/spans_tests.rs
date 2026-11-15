//! Replays of `recorded_spans.json`: how the reference in Python cut
//! files into the scanner's fragments, which bytes coordinates name, and
//! what it made of whole scanner reports over projections.

use super::{Limits, Reader, VERSION, collect, fragment, region};
use crate::{
    digest,
    document::Document,
    scratch::{Scratch, base64},
};
use std::{io::Read, path::Path};

const RECORDING: &[u8] = include_bytes!("recorded_spans.json");

fn recording() -> Document {
    Document::read(RECORDING).expect("a recording")
}

fn text<'a>(case: &'a Document, name: &str) -> &'a str {
    let held = case.get(name).and_then(Document::text);
    held.unwrap_or_else(|| panic!("no text {name}"))
}

/// Bytes that the recording keeps deflated.
fn inflated(kept: &Document) -> Vec<u8> {
    let mut data = Vec::new();
    let packed = base64(text(kept, "deflated"));
    let read = flate2::read::ZlibDecoder::new(&packed[..]).read_to_end(&mut data);
    read.expect("deflated bytes");
    assert_eq!(digest(&data), text(kept, "sha256"));
    data
}

#[test]
fn every_recorded_file_is_cut_into_the_same_fragments() {
    let recording = recording();
    let cases = recording
        .get("fragments")
        .and_then(Document::items)
        .expect("cases");
    assert_eq!(cases.len(), 26);
    for case in cases {
        let data = inflated(case.get("data").expect("data"));
        let mut reader = Reader {
            source: &data[..],
            pending: Vec::new(),
            position: 0,
        };
        let mut lengths = Vec::new();
        while let Some(part) = fragment(&mut reader).expect("a fragment") {
            lengths.push(Document::from(part.len()));
        }
        assert_eq!(
            Some(&Document::List(lengths)),
            case.get("result"),
            "{}",
            text(case, "name")
        );
    }
}

#[test]
fn every_small_coordinate_names_the_recorded_bytes() {
    let recording = recording();
    let regions = recording.get("regions").expect("regions");
    let data = base64(text(regions, "data"));
    let lines: Vec<usize> = (0..data.len()).filter(|at| data[*at] == b'\n').collect();
    let mut results = regions
        .get("results")
        .and_then(Document::items)
        .expect("results")
        .iter();
    let (mut named, mut unnamed) = (0, 0);
    for first in -1..5 {
        for last in -1..5 {
            for column in -1..6 {
                for end in -1..6 {
                    let coordinates = [first, last, column, end];
                    let said = results.next().expect("a result").text().map(base64);
                    let found = region(&data, &lines, coordinates).map(<[u8]>::to_vec);
                    assert_eq!(found, said, "{coordinates:?}");
                    named += usize::from(found.is_some());
                    unnamed += usize::from(found.is_none());
                }
            }
        }
    }
    assert!(results.next().is_none());
    assert_eq!((named, unnamed), (55, 1709));
}

#[test]
fn the_reviewed_bounds_and_names_are_the_references() {
    let recording = recording();
    let bounds = recording.get("bounds").expect("bounds");
    let bound = |name: &str| bounds.get(name).and_then(Document::whole).expect("a bound");
    assert_eq!(text(bounds, "VERSION"), VERSION);
    assert_eq!(text(bounds, "FORMAT"), super::FORMAT);
    let reviewed = Limits::REVIEWED;
    assert_eq!(bound("MAX_FILE"), i128::from(reviewed.file));
    assert_eq!(bound("MAX_TOTAL"), i128::from(reviewed.total));
    assert_eq!(bound("MAX_FINDINGS"), reviewed.findings as i128);
    assert_eq!(bound("MAX_CANDIDATES"), reviewed.candidates as i128);
    assert_eq!(bound("SECONDS"), i128::from(reviewed.seconds));
}

/// What a case came to there: what was held, the refusal, or the class of
/// what the reference's interpreter raised.
fn there(case: &Document) -> String {
    let result = case.get("result").expect("a result");
    if let Some(held) = result.get("held") {
        return format!("held {}", held.pretty());
    }
    if let Some(words) = result.get("refused").and_then(Document::text) {
        return format!("refused {words}");
    }
    format!("RAISED {}", text(result, "raised"))
}

/// The bounds of a case: the reviewed ones but for those it names as the
/// reference's module names them.
fn limits(case: &Document) -> Limits {
    let mut limits = Limits::REVIEWED;
    for (name, value) in case
        .get("limits")
        .and_then(Document::fields)
        .expect("limits")
    {
        let value = value.whole().expect("a bound");
        let (unsigned, sized) = (u64::try_from(value).ok(), usize::try_from(value).ok());
        match name.as_str() {
            "MAX_FILE" => limits.file = unsigned.expect("a bound"),
            "MAX_TOTAL" => limits.total = unsigned.expect("a bound"),
            "MAX_FINDINGS" => limits.findings = sized.expect("a bound"),
            "MAX_CANDIDATES" => limits.candidates = sized.expect("a bound"),
            "SECONDS" => limits.seconds = i64::try_from(value).expect("a bound"),
            other => panic!("no bound {other}"),
        }
    }
    limits
}

/// Build the projections of a case as its recorder left them.
fn built(case: &Document, root: &Path) -> std::path::PathBuf {
    let directory = root.join("secret-input");
    std::fs::create_dir(&directory).expect("a directory");
    for (name, kept) in case.get("files").and_then(Document::fields).expect("files") {
        std::fs::write(directory.join(name), inflated(kept)).expect("a projection");
    }
    let target = directory.join("content-000000");
    match case.get("edit").and_then(Document::text) {
        Some("link") => {
            std::fs::write(root.join("original-0"), b"inert\n").expect("a file");
            std::fs::remove_file(&target).expect("a projection");
            std::os::unix::fs::symlink(root.join("original-0"), &target).expect("a link");
        }
        Some("pipe") => {
            std::fs::remove_file(&target).expect("a projection");
            let made = std::process::Command::new("mkfifo").arg(&target).status();
            assert!(made.expect("mkfifo").success());
        }
        Some("absent") => std::fs::remove_file(&target).expect("a projection"),
        Some("directory") => {
            std::fs::remove_file(&target).expect("a projection");
            std::fs::create_dir(&target).expect("a directory");
        }
        Some(other) => panic!("no edit {other}"),
        None => {}
    }
    match case.get("directory").and_then(Document::text) {
        Some("absent") => std::fs::remove_dir_all(&directory).expect("a directory"),
        Some("link") => {
            std::fs::rename(&directory, root.join("real")).expect("a directory");
            std::os::unix::fs::symlink("real", &directory).expect("a link");
        }
        Some("file") => {
            std::fs::remove_dir_all(&directory).expect("a directory");
            std::fs::write(&directory, b"").expect("a file");
        }
        Some(other) => panic!("no edit {other}"),
        None => {}
    }
    directory
}

/// Every whole report: its name, what it came to here and there.
fn reports() -> Vec<(String, String, String)> {
    let recording = recording();
    let cases = recording
        .get("spans")
        .and_then(Document::items)
        .expect("cases");
    let mut all = Vec::new();
    for case in cases {
        let scratch = Scratch::new("spans");
        let directory = built(case, &scratch.0);
        let version = case
            .get("version")
            .and_then(Document::text)
            .unwrap_or(VERSION);
        let (rows, paths) = (
            case.get("rows").expect("rows"),
            case.get("paths").expect("paths"),
        );
        let came = collect(rows, paths, &directory, version, &limits(case));
        let here = match came {
            Ok(held) => format!("held {}", held.pretty()),
            Err(fault) => format!("refused {fault}"),
        };
        all.push((text(case, "name").to_owned(), here, there(case)));
    }
    all
}

/// The recorded case `name`, built anew: its scratch directory, its
/// projections, its report and its map of projections.
fn case(name: &str) -> (Scratch, std::path::PathBuf, Document, Document) {
    let recording = recording();
    let cases = recording
        .get("spans")
        .and_then(Document::items)
        .expect("cases");
    let case = cases
        .iter()
        .find(|case| text(case, "name") == name)
        .expect("a case");
    let scratch = Scratch::new("spans-direct");
    let directory = built(case, &scratch.0);
    let (rows, paths) = (
        case.get("rows").expect("rows"),
        case.get("paths").expect("paths"),
    );
    (scratch, directory, rows.clone(), paths.clone())
}

fn statuses(came: crate::Result<Document>) -> Result<Vec<String>, &'static str> {
    let held = came.map_err(|fault| fault.0)?;
    let findings = held
        .get("findings")
        .and_then(Document::items)
        .expect("findings");
    Ok(findings
        .iter()
        .map(|finding| text(finding, "status").to_owned())
        .collect())
}

#[test]
fn a_link_to_the_very_projection_is_not_followed() {
    let (scratch, directory, rows, paths) = case("a token after a line end");
    let target = directory.join("content-000000");
    std::fs::rename(&target, scratch.0.join("moved")).expect("a projection");
    std::os::unix::fs::symlink(scratch.0.join("moved"), &target).expect("a link");
    let came = collect(&rows, &paths, &directory, VERSION, &Limits::REVIEWED);
    assert_eq!(statuses(came), Err("secret_span_file_kind"));
}

#[test]
fn what_is_no_file_is_refused_although_it_has_the_size_of_the_projection() {
    let (_scratch, directory, rows, paths) = case("a token after a line end");
    let target = directory.join("content-000000");
    std::fs::remove_file(&target).expect("a projection");
    std::fs::create_dir(&target).expect("a directory");
    let size = target.metadata().expect("a directory").len();
    assert!(size > 0);
    let mut paths = paths.fields().expect("paths").to_vec();
    let Document::Record(expected) = &mut paths[0].1 else {
        panic!("no record");
    };
    for (name, value) in expected {
        if name == "projectionBytes" {
            *value = size.into();
        }
    }
    let paths = Document::Record(paths);
    let came = collect(&rows, &paths, &directory, VERSION, &Limits::REVIEWED);
    assert_eq!(statuses(came), Err("secret_span_file_kind"));
}

#[test]
fn the_bytes_of_every_file_count_once_towards_their_bound() {
    let (_scratch, directory, rows, paths) = case("findings in two files");
    let sizes = paths.fields().expect("paths").iter().map(|(_, expected)| {
        let size = expected.get("projectionBytes").and_then(Document::whole);
        u64::try_from(size.expect("a size")).expect("a size")
    });
    let total: u64 = sizes.sum();
    let fitted = Limits {
        total,
        ..Limits::REVIEWED
    };
    let came = collect(&rows, &paths, &directory, VERSION, &fitted);
    assert_eq!(statuses(came).map(|found| found.len()), Ok(3));
    let fewer = Limits {
        total: total - 1,
        ..Limits::REVIEWED
    };
    let came = collect(&rows, &paths, &directory, VERSION, &fewer);
    assert_eq!(statuses(came), Err("secret_span_read_budget"));
}

#[test]
fn a_projection_is_named_by_six_digits_whatever_the_map_holds() {
    let (_scratch, directory, rows, paths) = case("a token after a line end");
    let expected = paths.get("content-000000").expect("a projection").clone();
    let mut fields = paths.fields().expect("paths").to_vec();
    for name in [
        "other",
        "content-00000a",
        "content-00000",
        "content-0000000",
    ] {
        fields.push((name.to_owned(), expected.clone()));
    }
    let paths = Document::Record(fields);
    for name in [
        "/layers/other",
        "other",
        "/layers/content-00000a",
        "/layers/content-00000",
        "/layers/content-0000000",
    ] {
        let mut row = rows.items().expect("rows")[0]
            .fields()
            .expect("a row")
            .to_vec();
        assert_eq!(row[0].0, "File");
        row[0].1 = name.into();
        let rows = Document::List(vec![Document::Record(row)]);
        let came = collect(&rows, &paths, &directory, VERSION, &Limits::REVIEWED);
        assert_eq!(statuses(came), Err("secret_span_file"), "{name}");
    }
}

/// Reports and maps of projections that the reference failed on in its
/// interpreter's words: a missing field, a value of another kind, a
/// projection that cannot be opened. Each has a code here.
const REWORDED: [(&str, &str); 16] = [
    ("a projection that is a link", "secret_span_file_kind"),
    ("a projection that is not there", "secret_span_file_kind"),
    ("a projection that is a directory", "secret_span_file_kind"),
    ("a finding whose file is no text", "secret_span_report"),
    ("a finding without its File", "secret_span_report"),
    ("a finding without its RuleID", "secret_span_report"),
    ("a finding without its StartLine", "secret_span_report"),
    ("a finding without its EndColumn", "secret_span_report"),
    ("a projection without a size", "secret_span_file_size"),
    (
        "a projection with a digest that is no text",
        "secret_span_file_digest",
    ),
    ("a projection without a digest", "secret_span_paths"),
    ("a projection without a path", "secret_span_paths"),
    (
        "a projection without the digest of its source",
        "secret_span_paths",
    ),
    ("a projection that is no record", "secret_span_paths"),
    ("a report that is no list", "secret_span_report"),
    ("a finding that is no record", "secret_span_report"),
];

#[test]
fn every_recorded_report_comes_to_the_same() {
    let (mut held, mut refused, mut reworded) = (0, 0, 0);
    let mut statuses = std::collections::BTreeMap::new();
    for (name, here, there) in reports() {
        if let Some((_, code)) = REWORDED.iter().find(|(other, _)| *other == name) {
            assert_eq!(here, format!("refused {code}"), "{name}");
            assert!(there.starts_with("RAISED "), "{name}");
            reworded += 1;
            continue;
        }
        assert_eq!(here, there, "{name}");
        held += usize::from(here.starts_with("held "));
        refused += usize::from(here.starts_with("refused "));
        for status in ["resolved", "ambiguous", "unresolved"] {
            let said = format!("\"status\": \"{status}\"");
            *statuses.entry(status).or_insert(0) += here.matches(&said).count();
        }
    }
    assert_eq!((held, refused, reworded), (36, 55, 16));
    let statuses: Vec<(&str, usize)> = statuses.into_iter().collect();
    assert_eq!(
        statuses,
        [("ambiguous", 1), ("resolved", 33), ("unresolved", 7)]
    );
}
