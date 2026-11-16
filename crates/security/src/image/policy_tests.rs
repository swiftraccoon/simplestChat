//! Replays of `recorded_policy.json`: what the reference in Python made of
//! the image's policy file, of the status of the scanner's database, of
//! inventories, of vulnerability reports and of the database a report
//! names.

use super::{
    Limits, Moment, database_status, inventory, load_policy, vulnerability_database_binding,
    vulnerability_verdict,
};
use crate::{
    document::Document,
    policy::{Date, Exception},
    scratch::{Scratch, base64},
};
use std::path::{Path, PathBuf};

const RECORDING: &[u8] = include_bytes!("recorded_policy.json");
/// The versions of the scanners that the recorder's lock named.
const SYFT: &str = "1.54.0";
const GRYPE: &str = "0.120.0";

fn recording() -> Document {
    Document::read(RECORDING).expect("a recording")
}

fn cases(name: &str) -> Vec<Document> {
    let held = recording()
        .get(name)
        .and_then(Document::items)
        .map(<[Document]>::to_vec);
    held.unwrap_or_else(|| panic!("no cases {name}"))
}

fn text<'a>(case: &'a Document, name: &str) -> &'a str {
    let held = case.get(name).and_then(Document::text);
    held.unwrap_or_else(|| panic!("no text {name}"))
}

/// The repository's own files, as the reference read them.
fn repository(path: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(path)
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

fn here(came: crate::Result<Document>) -> String {
    match came {
        Ok(held) => format!("held {}", held.pretty()),
        Err(fault) => format!("refused {fault}"),
    }
}

/// The reviewed policy, as the reference read it for every other section.
fn reviewed() -> Document {
    load_policy(&repository("security/image-policy.json")).expect("the reviewed policy")
}

/// The moment that the recorder took for now.
fn now() -> Moment {
    let recorded = recording();
    assert_eq!(text(&recorded, "now"), "2026-09-30T00:00:00+00:00");
    Moment(1_790_726_400 * 1_000_000)
}

fn policies() -> Vec<(String, String, String)> {
    let mut all = Vec::new();
    for case in cases("policies") {
        let scratch = Scratch::new("image-policy");
        let path = scratch.0.join("image-policy.json");
        match text(&case, "kind") {
            "file" => {
                let content = base64(text(&case, "content"));
                std::fs::write(&path, content).expect("a policy");
            }
            "link" => {
                let reviewed = std::fs::read(repository("security/image-policy.json"));
                std::fs::write(scratch.0.join("real.json"), reviewed.expect("the policy"))
                    .expect("a policy");
                std::os::unix::fs::symlink("real.json", &path).expect("a link");
            }
            "directory" => std::fs::create_dir(&path).expect("a directory"),
            "pipe" => {
                let made = std::process::Command::new("mkfifo").arg(&path).status();
                assert!(made.expect("mkfifo").success());
            }
            "absent" => {}
            other => panic!("no kind {other}"),
        }
        all.push((
            text(&case, "name").to_owned(),
            here(load_policy(&path)),
            there(&case),
        ));
    }
    all
}

fn databases() -> Vec<(String, String, String)> {
    let mut all = Vec::new();
    let mut policy = reviewed();
    for case in cases("databases") {
        if let Document::Record(fields) = &mut policy {
            for (name, value) in fields.iter_mut() {
                if name == "maximumDatabaseAgeHours" {
                    *value = case.get("hours").cloned().expect("hours");
                }
            }
        }
        let status = case.get("status").expect("a status");
        let came = database_status(status, &policy, now()).map(|()| Document::Null);
        all.push((text(&case, "name").to_owned(), here(came), there(&case)));
    }
    all
}

fn limits(case: &Document, field: fn(&mut Limits, usize)) -> Limits {
    let mut limits = Limits::REVIEWED;
    if let Some(limit) = case.get("limit").and_then(Document::whole) {
        field(&mut limits, usize::try_from(limit).expect("a bound"));
    }
    limits
}

fn inventories() -> Vec<(String, String, String)> {
    let mut all = Vec::new();
    let policy = reviewed();
    for case in cases("inventories") {
        let limits = limits(&case, |limits, limit| limits.packages = limit);
        let value = case.get("value").expect("an inventory");
        let came = inventory(value, &policy, SYFT, &limits).map(Document::List);
        all.push((text(&case, "name").to_owned(), here(came), there(&case)));
    }
    all
}

/// A review as the recorder wrote it.
fn exception(record: &Document) -> Exception {
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

fn vulnerabilities() -> Vec<(String, String, String)> {
    let mut all = Vec::new();
    let advisory = repository(super::advisories::POLICY_PATH);
    for case in cases("vulnerabilities") {
        let limits = limits(&case, |limits, limit| limits.matches = limit);
        let reviews = case
            .get("exceptions")
            .and_then(Document::items)
            .expect("reviews");
        let reviews: Vec<Exception> = reviews.iter().map(exception).collect();
        let packages = case
            .get("packages")
            .and_then(Document::items)
            .expect("packages");
        let value = case.get("value").expect("a report");
        let came = vulnerability_verdict(value, &reviews, packages, GRYPE, &advisory, &limits);
        all.push((text(&case, "name").to_owned(), here(came), there(&case)));
    }
    all
}

fn bindings() -> Vec<(String, String, String)> {
    let mut all = Vec::new();
    for case in cases("bindings") {
        let (value, database) = (case.get("value").expect("a report"), case.get("database"));
        let came = vulnerability_database_binding(value, database.expect("a database"));
        let came = came.map(|()| Document::Null);
        all.push((text(&case, "name").to_owned(), here(came), there(&case)));
    }
    all
}

#[test]
fn the_reviewed_policy_of_the_repository_is_the_recorded_one() {
    let all = cases("policies");
    let recorded = all
        .iter()
        .find(|case| text(case, "name") == "the reviewed policy");
    let recorded = recorded.expect("a case");
    let bytes = std::fs::read(repository("security/image-policy.json")).expect("the policy");
    assert_eq!(base64(text(recorded, "content")), bytes);
    let held = recorded.get("result").and_then(|result| result.get("held"));
    assert_eq!(held, Some(&reviewed()));
}

#[test]
fn the_reviewed_bounds_are_the_references() {
    let recorded = recording();
    let bounds = recorded.get("bounds").expect("bounds");
    let bound = |name: &str| bounds.get(name).and_then(Document::whole).expect("a bound");
    assert_eq!(bound("MAX_REPORT"), i128::from(Limits::REVIEWED.report));
    assert_eq!(bound("MAX_PACKAGES"), Limits::REVIEWED.packages as i128);
    assert_eq!(bound("MAX_MATCHES"), Limits::REVIEWED.matches as i128);
    assert_eq!(bound("MIN_DATABASE_HOURS"), *super::DATABASE_HOURS.start());
    assert_eq!(bound("MAX_DATABASE_HOURS"), *super::DATABASE_HOURS.end());
}

#[test]
fn a_day_is_counted_from_1970_across_the_whole_calendar() {
    // What the reference's calendar counts for each day.
    for (year, month, day, days) in [
        (1970, 1, 1, 0),
        (1969, 12, 31, -1),
        (2000, 2, 29, 11_016),
        (2000, 3, 1, 11_017),
        (2024, 2, 29, 19_782),
        (1900, 3, 1, -25_508),
        (1, 1, 1, -719_162),
        (9999, 12, 31, 2_932_896),
        (2026, 9, 30, 20_726),
    ] {
        assert_eq!(
            super::days_from_civil(year, month, day),
            days,
            "{year}-{month}-{day}"
        );
    }
}

#[test]
fn a_query_is_decoded_as_the_reference_decodes_one() {
    // What the reference's query reader made of each value.
    for (text, said) in [
        ("plain", "plain"),
        ("%41%6a%6A", "Ajj"),
        ("a%C3%A9", "a\u{e9}"),
        ("%", "%"),
        ("%4", "%4"),
        ("%zz", "%zz"),
        ("%+4", "% 4"),
        ("%4 ", "%4 "),
        ("a%", "a%"),
        ("%ff", "\u{fffd}"),
        ("a+b", "a b"),
        ("%2B", "+"),
        ("%25", "%"),
        ("%%41", "%A"),
        ("%C3", "\u{fffd}"),
        ("%C3%A9%", "\u{e9}%"),
        ("\u{e9}%41", "\u{e9}A"),
        ("%e9", "\u{fffd}"),
        ("+%2b+", " + "),
    ] {
        assert_eq!(super::decoded(text), said, "{text}");
    }
}

#[test]
fn the_clock_reads_the_moment_it_is() {
    let micros = |moment: std::time::SystemTime| {
        let since = moment
            .duration_since(std::time::UNIX_EPOCH)
            .expect("a time after 1970");
        i64::try_from(since.as_micros()).expect("a moment")
    };
    let before = micros(std::time::SystemTime::now());
    let moment = Moment::now();
    let after = micros(std::time::SystemTime::now());
    assert!(
        before <= moment.0 && moment.0 <= after,
        "{before} {} {after}",
        moment.0
    );
    assert!(moment > now());
}

/// What a time of building comes to.
#[derive(Debug, PartialEq)]
enum Read {
    At(i64),
    Naive,
}

fn read(text: &str) -> Option<Read> {
    super::built(text).map(|built| match built {
        super::Built::At(moment) => Read::At(moment.0),
        super::Built::Naive => Read::Naive,
    })
}

#[test]
fn a_time_is_read_as_the_reference_reads_one() {
    // The moment in microseconds since 1970 that the reference's
    // fromisoformat made of each, a day and time of no place, or nothing.
    for (text, said) in [
        (
            "2026-09-30T00:00:00Z",
            Some(Read::At(1_790_726_400_000_000)),
        ),
        (
            "2026-09-30T00:00:00+00:00",
            Some(Read::At(1_790_726_400_000_000)),
        ),
        (
            "2026-09-30T00:00:00-00:00",
            Some(Read::At(1_790_726_400_000_000)),
        ),
        (
            "2026-09-29T19:00:00-05:00",
            Some(Read::At(1_790_726_400_000_000)),
        ),
        (
            "2026-09-30T05:30:00+05:30",
            Some(Read::At(1_790_726_400_000_000)),
        ),
        (
            "2026-09-30T00:00:00+00:30",
            Some(Read::At(1_790_724_600_000_000)),
        ),
        (
            "2026-09-30T00:00:00-00:30",
            Some(Read::At(1_790_728_200_000_000)),
        ),
        (
            "2026-09-30T00:00:00+23:59",
            Some(Read::At(1_790_640_060_000_000)),
        ),
        (
            "2026-09-30T00:00:00-23:59",
            Some(Read::At(1_790_812_740_000_000)),
        ),
        (
            "2026-09-30T00:00:00+00:59",
            Some(Read::At(1_790_722_860_000_000)),
        ),
        ("2000-02-29T00:00:00Z", Some(Read::At(951_782_400_000_000))),
        ("1900-02-29T00:00:00Z", None),
        (
            "2024-02-29T00:00:00Z",
            Some(Read::At(1_709_164_800_000_000)),
        ),
        ("2026-02-29T00:00:00Z", None),
        ("2100-02-29T00:00:00Z", None),
        (
            "2400-02-29T00:00:00Z",
            Some(Read::At(13_574_563_200_000_000)),
        ),
        (
            "2026-01-31T00:00:00Z",
            Some(Read::At(1_769_817_600_000_000)),
        ),
        (
            "2026-03-31T00:00:00Z",
            Some(Read::At(1_774_915_200_000_000)),
        ),
        (
            "2026-04-30T00:00:00Z",
            Some(Read::At(1_777_507_200_000_000)),
        ),
        ("2026-04-31T00:00:00Z", None),
        ("2026-09-31T00:00:00Z", None),
        (
            "2026-02-28T23:59:59Z",
            Some(Read::At(1_772_323_199_000_000)),
        ),
        (
            "2026-12-31T23:59:59Z",
            Some(Read::At(1_798_761_599_000_000)),
        ),
        ("2026-13-01T00:00:00Z", None),
        ("2026-00-01T00:00:00Z", None),
        ("2026-01-00T00:00:00Z", None),
        ("0000-01-01T00:00:00Z", None),
        (
            "0001-01-01T00:00:00Z",
            Some(Read::At(-62_135_596_800_000_000)),
        ),
        (
            "9999-12-31T23:59:59Z",
            Some(Read::At(253_402_300_799_000_000)),
        ),
        (
            "2026-09-30T23:00:00Z",
            Some(Read::At(1_790_809_200_000_000)),
        ),
        (
            "2026-09-30T00:59:00Z",
            Some(Read::At(1_790_729_940_000_000)),
        ),
        ("2026-09-30T00:60:00Z", None),
        (
            "2026-09-30T00:00:59Z",
            Some(Read::At(1_790_726_459_000_000)),
        ),
        ("2026-09-30T00:00:60Z", None),
        (
            "2026-09-30T00:00:00.1Z",
            Some(Read::At(1_790_726_400_100_000)),
        ),
        (
            "2026-09-30T00:00:00.12Z",
            Some(Read::At(1_790_726_400_120_000)),
        ),
        (
            "2026-09-30T00:00:00.123Z",
            Some(Read::At(1_790_726_400_123_000)),
        ),
        (
            "2026-09-30T00:00:00.1234Z",
            Some(Read::At(1_790_726_400_123_400)),
        ),
        (
            "2026-09-30T00:00:00.12345Z",
            Some(Read::At(1_790_726_400_123_450)),
        ),
        (
            "2026-09-30T00:00:00.123456Z",
            Some(Read::At(1_790_726_400_123_456)),
        ),
        (
            "2026-09-30T00:00:00.1234567Z",
            Some(Read::At(1_790_726_400_123_456)),
        ),
        (
            "2026-09-30T00:00:00.123456789Z",
            Some(Read::At(1_790_726_400_123_456)),
        ),
        (
            "2026-09-30T00:00:00.000001Z",
            Some(Read::At(1_790_726_400_000_001)),
        ),
        ("2026-09-30T00:00:00.Z", None),
        ("2026-09-30T00:00:00.5", Some(Read::Naive)),
        ("2026-09-30T00:00:00", Some(Read::Naive)),
        ("2026-09-30", Some(Read::Naive)),
        ("2026-09/30T00:00:00Z", None),
        ("2026/09-30T00:00:00Z", None),
        ("2026-09-30T00X00:00Z", None),
        ("2026-09-30T00:00X00Z", None),
        ("2026-09-30T00:00:00+00X00", None),
        ("2026-09-30T00:00:00+0X:00", None),
        ("2026-09-30T00:00:00+24:00", None),
        ("2026-09-30T00:00:00*00:00", None),
    ] {
        assert_eq!(read(text), said, "{text}");
    }
    // The reference took these for the midnight of the next day and an
    // offset of an hour; RFC 3339 has neither.
    for text in ["2026-09-30T24:00:00Z", "2026-09-30T00:00:00+00:60"] {
        assert_eq!(read(text), None, "{text}");
    }
}

/// What the reference did with the cases that come to something else here.
#[derive(Clone, Copy)]
enum There {
    /// It failed in its interpreter's words.
    Raised,
    /// It took them.
    Held,
    /// It refused them in other words.
    Refused,
}

/// Hold every case to the recording, but for those that `tables` name with
/// the code that refuses them here and what the reference did instead.
/// Answers how many came to the same held, the same refusal, and to a
/// refusal here where the reference raised, held and refused otherwise.
fn tallied(all: Vec<(String, String, String)>, tables: &[(&[(&str, &str)], There)]) -> [usize; 5] {
    let mut tally = [0; 5];
    for (name, here, there) in all {
        let found = tables.iter().find_map(|(table, kind)| {
            let found = table.iter().find(|(other, _)| *other == name);
            found.map(|(_, code)| (*code, *kind))
        });
        let Some((code, kind)) = found else {
            assert_eq!(here, there, "{name}");
            tally[usize::from(here.starts_with("refused "))] += 1;
            continue;
        };
        assert_eq!(here, format!("refused {code}"), "{name}");
        let (prefix, slot) = match kind {
            There::Raised => ("RAISED ", 2),
            There::Held => ("held ", 3),
            There::Refused => ("refused ", 4),
        };
        assert!(there.starts_with(prefix) && here != there, "{name}");
        tally[slot] += 1;
    }
    tally
}

/// Policy files that the reference failed on in its interpreter's words:
/// bytes that are no JSON object of text, a field of another kind, a file
/// that cannot be opened.
const UNREAD_POLICIES: [(&str, &str); 13] = [
    ("a policy that is no JSON", "image_report_invalid"),
    ("a policy that is a list", "image_policy_fields"),
    ("a policy that is no text", "image_report_invalid"),
    ("a policy that names a field twice", "image_report_invalid"),
    ("a policy with more after it", "image_report_invalid"),
    (
        "a policy whose scanner base is a number",
        "image_scanner_base",
    ),
    (
        "a policy whose scanner base is nothing",
        "image_scanner_base",
    ),
    (
        "a policy whose allowed licences are one that is a number",
        "image_license_policy",
    ),
    (
        "a policy whose allowed licences are one that is nothing",
        "image_license_policy",
    ),
    (
        "a policy whose allowed licences are no list",
        "image_license_policy",
    ),
    ("a policy that is a link", "tool_entry_unavailable"),
    ("a policy that is not there", "tool_entry_missing"),
    ("a policy that is a directory", "nonregular_tool_file"),
];

/// Policy files that the reference took and that are refused here: a
/// scanner base whose dot is another character, which the reference's
/// pattern took for any character, and a first party published as
/// nought, which the reference's interpreter took for false.
const STRICTER_POLICIES: [(&str, &str); 2] = [
    (
        "a policy whose scanner base is another character for its dot",
        "image_scanner_base",
    ),
    (
        "a policy whose first party has a publication of nought",
        "image_first_party_policy",
    ),
];

/// Statuses of the database that the reference failed on in its
/// interpreter's words: a field that is missing or of another kind, a
/// time that it could not read.
const UNREAD_DATABASES: [(&str, &str); 16] = [
    (
        "a database whose schema is a number",
        "image_database_schema",
    ),
    ("a database without its schema", "image_database_schema"),
    (
        "a database from an address that is a number",
        "image_database_source",
    ),
    ("a database from nowhere", "image_database_source"),
    (
        "a database built at '2026-09-30T00:00:00z'",
        "image_database_time",
    ),
    (
        "a database built at '2026-09-29T23:59:60+00:00'",
        "image_database_time",
    ),
    (
        "a database built at '2026-02-30T00:00:00+00:00'",
        "image_database_time",
    ),
    (
        "a database built at '2026-09-30T00:00:00+24:00'",
        "image_database_time",
    ),
    (
        "a database built at ' 2026-09-30T00:00:00+00:00'",
        "image_database_time",
    ),
    (
        "a database built at '2026-09-30T00:00:00+00:00 '",
        "image_database_time",
    ),
    (
        "a database built at '2026-09-30T00:00:00+00:00\\n'",
        "image_database_time",
    ),
    (
        "a database built at '+2026-09-30T00:00:00+00:00'",
        "image_database_time",
    ),
    (
        "a database built at '٢026-09-30T00:00:00+00:00'",
        "image_database_time",
    ),
    ("a database built at ''", "image_database_time"),
    ("a database built at a number", "image_database_time"),
    (
        "a database without its time of building",
        "image_database_time",
    ),
];

/// Statuses that the reference took and that are refused here. An address
/// is read as it is written: the reference's URL library took the kind and
/// the host without regard to case, a user and a port beside the host, cut
/// a fragment off and dropped white space. A time is read as the scanner
/// writes it, RFC 3339: the reference took any character between the day
/// and the time, weeks, compact forms and offsets of other shapes.
const STRICTER_DATABASES: [(&str, &str); 18] = [
    (
        "a database from an address with its kind in capitals",
        "image_database_source",
    ),
    (
        "a database from an address with its host in capitals",
        "image_database_source",
    ),
    (
        "a database from an address with a user before its host",
        "image_database_source",
    ),
    (
        "a database from an address with a port after its host",
        "image_database_source",
    ),
    (
        "a database from an address with a checksum and a fragment",
        "image_database_source",
    ),
    (
        "a database from an address with a space before it",
        "image_database_source",
    ),
    (
        "a database from an address with a line end inside it",
        "image_database_source",
    ),
    (
        "a database built at '2026-09-30 00:00:00+00:00'",
        "image_database_time",
    ),
    (
        "a database built at '2026-09-30t00:00:00+00:00'",
        "image_database_time",
    ),
    (
        "a database built at '2026-09-30X00:00:00+00:00'",
        "image_database_time",
    ),
    (
        "a database built at '2026-W40-3T00:00:00+00:00'",
        "image_database_time",
    ),
    (
        "a database built at '20260930T000000Z'",
        "image_database_time",
    ),
    (
        "a database built at '2026-09-30T00+00:00'",
        "image_database_time",
    ),
    (
        "a database built at '2026-09-30T00:00+00:00'",
        "image_database_time",
    ),
    (
        "a database built at '2026-09-30T00:00:00+0000'",
        "image_database_time",
    ),
    (
        "a database built at '2026-09-30T00:00:00+00'",
        "image_database_time",
    ),
    (
        "a database built at '2026-09-30T00:00:00+00:00:00'",
        "image_database_time",
    ),
    (
        "a database built at '2026-09-30T00:00:00.000+00:00:00.000001'",
        "image_database_time",
    ),
];

/// Statuses that the reference refused as stale and that are refused here
/// as times that cannot be read: an hour of 24, which it took for the
/// midnight of the next day, and a comma before the fraction of a second.
const OTHERWISE_DATABASES: [(&str, &str); 2] = [
    (
        "a database built at '2026-09-30T24:00:00+00:00'",
        "image_database_time",
    ),
    (
        "a database built at '2026-09-30T00:00:00,5+00:00'",
        "image_database_time",
    ),
];

/// Inventories that the reference failed on in its interpreter's words.
const UNREAD_INVENTORIES: [(&str, &str); 9] = [
    ("an inventory made by a list", "image_sbom_tool_identity"),
    ("an inventory without its maker", "image_sbom_tool_identity"),
    ("an inventory of a list", "image_distribution_unsupported"),
    (
        "an inventory of a package without a name",
        "image_package_identity",
    ),
    (
        "an inventory of a package without a version",
        "image_package_identity",
    ),
    (
        "an inventory of a package without its own name",
        "image_package_identity",
    ),
    (
        "an inventory of a package whose version is a number",
        "image_package_identity",
    ),
    (
        "an inventory of a package that is no record",
        "image_inventory_shape",
    ),
    (
        "an inventory whose packages are no list",
        "image_inventory_shape",
    ),
];

/// Vulnerability reports that the reference failed on in its
/// interpreter's words.
const UNREAD_REPORTS: [(&str, &str); 19] = [
    ("a finding of severity 7", "image_vulnerability_report"),
    ("a finding of severity None", "image_vulnerability_report"),
    (
        "a report made by a list",
        "image_vulnerability_tool_identity",
    ),
    (
        "a report without its maker",
        "image_vulnerability_tool_identity",
    ),
    (
        "a report with ignored findings that are no list",
        "image_ignored_vulnerabilities",
    ),
    ("a report without findings", "image_vulnerability_report"),
    (
        "a report whose findings are no list",
        "image_vulnerability_report",
    ),
    ("a finding that is no record", "image_vulnerability_report"),
    (
        "a finding without its vulnerability",
        "image_vulnerability_report",
    ),
    (
        "a finding without its package",
        "image_vulnerability_report",
    ),
    (
        "a finding whose package is text",
        "image_vulnerability_report",
    ),
    (
        "a low finding whose package is text",
        "image_vulnerability_report",
    ),
    (
        "a finding whose vulnerability is text",
        "image_vulnerability_report",
    ),
    (
        "a high finding without an address",
        "image_vulnerability_report",
    ),
    (
        "a high finding whose address is a number",
        "image_vulnerability_report",
    ),
    (
        "a high finding without its name",
        "image_vulnerability_report",
    ),
    ("a high finding without a fix", "image_vulnerability_report"),
    (
        "a high finding whose fix is text",
        "image_vulnerability_report",
    ),
    (
        "a finding without its severity",
        "image_vulnerability_report",
    ),
];

/// Bindings of a report to its database that the reference failed on in
/// its interpreter's words.
const UNREAD_BINDINGS: [(&str, &str); 10] = [
    (
        "a checked database without its time of building",
        "image_vulnerability_database_changed",
    ),
    (
        "a report of a database whose providers are a list",
        "image_vulnerability_providers_missing",
    ),
    (
        "a report of a database without providers",
        "image_vulnerability_providers_missing",
    ),
    (
        "a report of a database without its status",
        "image_vulnerability_database_changed",
    ),
    (
        "a report without its database",
        "image_vulnerability_database_changed",
    ),
    (
        "a report configured without only-fixed",
        "image_vulnerability_filter",
    ),
    (
        "a report configured without only-notfixed",
        "image_vulnerability_filter",
    ),
    (
        "a report configured without ignore-wontfix",
        "image_vulnerability_filter",
    ),
    (
        "a report without its configuration",
        "image_vulnerability_filter",
    ),
    (
        "a report whose configuration is a list",
        "image_vulnerability_filter",
    ),
];

/// Bindings that the reference took because its interpreter holds one
/// equal to true; a value is held here to the same value of the same kind.
const SAME_KIND: [(&str, &str); 2] = [
    (
        "a report of a database whose valid is 1",
        "image_vulnerability_database_changed",
    ),
    (
        "a checked database whose validity is one",
        "image_vulnerability_database_changed",
    ),
];

#[test]
fn every_recorded_policy_file_is_read_alike() {
    let tables = [
        (&UNREAD_POLICIES[..], There::Raised),
        (&STRICTER_POLICIES[..], There::Held),
    ];
    assert_eq!(tallied(policies(), &tables), [10, 40, 13, 2, 0]);
}

#[test]
fn every_recorded_status_of_the_database_is_judged_alike() {
    let tables = [
        (&UNREAD_DATABASES[..], There::Raised),
        (&STRICTER_DATABASES[..], There::Held),
        (&OTHERWISE_DATABASES[..], There::Refused),
    ];
    assert_eq!(tallied(databases(), &tables), [24, 44, 16, 18, 2]);
}

#[test]
fn every_recorded_inventory_is_read_alike() {
    let tables = [(&UNREAD_INVENTORIES[..], There::Raised)];
    assert_eq!(tallied(inventories(), &tables), [7, 20, 9, 0, 0]);
}

#[test]
fn every_recorded_vulnerability_report_is_judged_alike() {
    let tables = [(&UNREAD_REPORTS[..], There::Raised)];
    assert_eq!(tallied(vulnerabilities(), &tables), [25, 9, 19, 0, 0]);
}

#[test]
fn every_recorded_binding_to_a_database_is_judged_alike() {
    let tables = [
        (&UNREAD_BINDINGS[..], There::Raised),
        (&SAME_KIND[..], There::Held),
    ];
    assert_eq!(tallied(bindings(), &tables), [2, 19, 10, 2, 0]);
}
