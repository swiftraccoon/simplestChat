//! Replays of `recorded_verdicts.json`: what the reference in Python made
//! of the fingerprints of resolved spans, of the binding of a finding to
//! its span, and of whole verdicts over secrets.

use super::{binding, fingerprint, secret_verdict};
use crate::{
    document::Document,
    image::spans::Limits,
    policy::{Date, Exception},
};

const RECORDING: &[u8] = include_bytes!("recorded_verdicts.json");

pub(crate) fn cases(name: &str) -> Vec<Document> {
    let recording = Document::read(RECORDING).expect("a recording");
    let held = recording
        .get(name)
        .and_then(Document::items)
        .map(<[Document]>::to_vec);
    held.unwrap_or_else(|| panic!("no cases {name}"))
}

pub(crate) fn text<'a>(case: &'a Document, name: &str) -> &'a str {
    let held = case.get(name).and_then(Document::text);
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

pub(crate) fn here(came: crate::Result<Document>) -> String {
    match came {
        Ok(held) => format!("held {}", held.pretty()),
        Err(fault) => format!("refused {fault}"),
    }
}

/// A review as the recorder wrote it.
pub(crate) fn exception(record: &Document) -> Exception {
    let field = |name: &str| text(record, name).to_owned();
    Exception {
        scanner: field("scanner"),
        fingerprint: field("fingerprint"),
        scope: field("scope"),
        owner: field("owner"),
        rationale: field("rationale"),
        reachability: field("reachability"),
        expires: Date::parse(text(record, "expires")).expect("a day"),
        review: field("review"),
    }
}

fn fingerprints() -> Vec<(String, String, String)> {
    let mut all = Vec::new();
    for case in cases("fingerprints") {
        let span = case.get("span").expect("a span");
        let came = fingerprint(span, &Limits::REVIEWED).map(Document::from);
        all.push((text(&case, "name").to_owned(), here(came), there(&case)));
    }
    all
}

fn bindings() -> Vec<(String, String, String)> {
    let mut all = Vec::new();
    for case in cases("bindings") {
        let (finding, span) = (
            case.get("finding").expect("a finding"),
            case.get("span").expect("a span"),
        );
        let paths = case.get("paths").expect("paths");
        let came = binding(finding, paths, span, &Limits::REVIEWED);
        let came = came.map(|(record, _)| Document::Record(record));
        all.push((text(&case, "name").to_owned(), here(came), there(&case)));
    }
    all
}

fn secrets() -> Vec<(String, String, String)> {
    let mut all = Vec::new();
    for case in cases("secrets") {
        let mut limits = Limits::REVIEWED;
        if let Some(limit) = case.get("limit").and_then(Document::whole) {
            limits.findings = usize::try_from(limit).expect("a bound");
        }
        let reviews = case
            .get("exceptions")
            .and_then(Document::items)
            .expect("reviews");
        let reviews: Vec<Exception> = reviews.iter().map(exception).collect();
        let (findings, spans) = (
            case.get("findings").expect("findings"),
            case.get("spans").expect("spans"),
        );
        let paths = case.get("paths").expect("paths");
        let came = secret_verdict(findings, &reviews, paths, spans, &limits);
        all.push((text(&case, "name").to_owned(), here(came), there(&case)));
    }
    all
}

/// Hold every case to the recording, but for those that `reworded` names
/// with the code they have here, where the reference failed in its
/// interpreter's words. Answers how many were held alike, refused alike
/// and refused in other words.
pub(crate) fn tallied(all: Vec<(String, String, String)>, reworded: &[(&str, &str)]) -> [usize; 3] {
    let mut tally = [0; 3];
    for (name, here, there) in all {
        if let Some((_, code)) = reworded.iter().find(|(other, _)| *other == name) {
            assert_eq!(here, format!("refused {code}"), "{name}");
            assert!(there.starts_with("RAISED "), "{name}");
            tally[2] += 1;
            continue;
        }
        assert_eq!(here, there, "{name}");
        tally[usize::from(here.starts_with("refused "))] += 1;
    }
    tally
}

/// Spans whose fingerprint the reference failed on in its interpreter's
/// words.
const UNREAD_SPANS: [(&str, &str); 4] = [
    ("a span whose digest is a number", "image_secret_shape"),
    ("a span whose digest is nothing", "image_secret_shape"),
    ("a span without its spanBytes", "image_secret_shape"),
    ("a span without its spanSha256", "image_secret_shape"),
];

/// Findings and spans that the reference failed on in its interpreter's
/// words when it bound them: a field that is missing or of another kind.
const UNREAD_BINDINGS: [(&str, &str); 16] = [
    ("a finding in a file that is a number", "image_secret_shape"),
    ("a finding without its file", "image_secret_shape"),
    (
        "a finding in a file whose identity is no record",
        "image_secret_shape",
    ),
    ("a finding of a path that is a number", "image_secret_shape"),
    (
        "a finding of an identity without a path",
        "image_secret_shape",
    ),
    ("a finding of a rule that is a number", "image_secret_shape"),
    ("a finding without its rule", "image_secret_shape"),
    ("a finding of no projection", "image_secret_shape"),
    ("a finding whose sha256 is a number", "image_secret_shape"),
    ("a finding without its sha256", "image_secret_shape"),
    (
        "a finding whose projectionSha256 is a number",
        "image_secret_shape",
    ),
    (
        "a finding without its projectionSha256",
        "image_secret_shape",
    ),
    ("a finding without its StartLine", "image_secret_shape"),
    ("a finding without its EndLine", "image_secret_shape"),
    ("a finding without its StartColumn", "image_secret_shape"),
    ("a finding without its EndColumn", "image_secret_shape"),
];

/// Verdicts over secrets that the reference failed on in its
/// interpreter's words.
const UNREAD_SECRETS: [(&str, &str); 5] = [
    ("spans without their findings", "image_secret_shape"),
    ("spans whose findings are no list", "image_secret_shape"),
    ("findings that are no list", "image_secret_shape"),
    ("a finding that is no record", "image_secret_shape"),
    ("a span that is no record", "image_secret_shape"),
];

#[test]
fn every_recorded_fingerprint_is_the_same() {
    assert_eq!(tallied(fingerprints(), &UNREAD_SPANS), [10, 16, 4]);
}

#[test]
fn every_recorded_finding_is_bound_to_its_span_alike() {
    assert_eq!(tallied(bindings(), &UNREAD_BINDINGS), [23, 93, 16]);
}

#[test]
fn every_recorded_verdict_over_secrets_is_the_same() {
    assert_eq!(tallied(secrets(), &UNREAD_SECRETS), [14, 9, 5]);
}

#[test]
fn the_reviewed_bounds_are_the_references() {
    let recording = Document::read(RECORDING).expect("a recording");
    let bounds = recording.get("bounds").expect("bounds");
    let bound = |name: &str| bounds.get(name).and_then(Document::whole).expect("a bound");
    assert_eq!(bound("MAX_SECRET_PATH"), super::MAX_PATH as i128);
    assert_eq!(bound("MAX_SECRET_IDENTITY"), super::MAX_IDENTITY as i128);
    assert_eq!(bound("MAX_FILE"), i128::from(Limits::REVIEWED.file));
    assert_eq!(bound("MAX_FINDINGS"), Limits::REVIEWED.findings as i128);
}
