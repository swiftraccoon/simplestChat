//! The review ledger's secret records, held to the retained evidence of the
//! regions they review: each current region needs its own exact review, a
//! changed region inherits none, unreviewed content stays blocking, and
//! every superseded region stays historical, never a current permission.

use crate::{
    document::Document,
    image::{
        policy::report,
        secrets::{fingerprint, secret_verdict},
        spans,
    },
    policy::{Date, Exception, read},
};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

/// The checkout this crate was built in.
fn repository() -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    root.canonicalize().expect("the checkout")
}

/// The retained evidence `name` of the reviewed secret regions.
fn evidence(name: &str) -> Document {
    let path = repository().join("security/secret-evidence").join(name);
    report(&path, 64 << 20).expect(name)
}

/// The reviews of the ledger, current on the day they were reviewed against.
fn current() -> Vec<Exception> {
    let today = Date::parse("2026-09-30").expect("a day");
    read(&repository().join("security/exceptions.json"), today, false).expect("the reviews")
}

/// What the secret verdict says of `rows` with their `paths` and `spans`,
/// under `reviews`.
fn verdict(
    rows: &[Document],
    reviews: &[Exception],
    paths: &Document,
    spans: &Document,
) -> Document {
    let report = Document::List(rows.to_vec());
    let judged = secret_verdict(&report, reviews, paths, spans, &spans::Limits::REVIEWED);
    judged.expect("a verdict")
}

/// The items of the list `name` of `value`.
fn items(value: &Document, name: &str) -> Vec<Document> {
    let held = value.get(name).and_then(Document::items);
    held.unwrap_or_else(|| panic!("no list {name}")).to_vec()
}

/// The text `name` of `value`.
fn text<'a>(value: &'a Document, name: &str) -> &'a str {
    let held = value.get(name).and_then(Document::text);
    held.unwrap_or_else(|| panic!("no text {name}"))
}

/// The field `name` of `value`.
fn field<'a>(value: &'a Document, name: &str) -> &'a Document {
    value.get(name).unwrap_or_else(|| panic!("no field {name}"))
}

/// `value` with its field `name` set to `to`.
fn with(value: &Document, name: &str, to: Document) -> Document {
    let mut changed = value.clone();
    changed.put(name, to);
    changed
}

/// The scanner's row of a finding in the file `name` by `rule`, at the
/// place `span` holds.
fn row(name: &str, rule: &Document, span: &Document) -> Document {
    let place = |at: &str| field(span, at).clone();
    Document::record([
        ("File", name.into()),
        ("RuleID", rule.clone()),
        ("StartLine", place("startLine")),
        ("EndLine", place("endLine")),
        ("StartColumn", place("startColumn")),
        ("EndColumn", place("endColumn")),
    ])
}

/// The regions of a report of `format` that are `spans`.
fn regions(format: &Document, spans: Vec<Document>) -> Document {
    Document::record([
        ("format", format.clone()),
        ("findings", Document::List(spans)),
    ])
}

/// The fingerprints of the findings `name` of a verdict.
fn fingerprints(verdict: &Document, name: &str) -> BTreeSet<String> {
    let found = items(verdict, name);
    found
        .iter()
        .map(|item| text(item, "fingerprint").to_owned())
        .collect()
}

/// Whether a verdict passed.
fn passed(verdict: &Document) -> bool {
    verdict.get("passed") == Some(&Document::Bool(true))
}

/// The reviews but the one of `fingerprint`.
fn without(reviews: &[Exception], fingerprint: &str) -> Vec<Exception> {
    let kept = reviews
        .iter()
        .filter(|item| item.fingerprint != fingerprint);
    kept.cloned().collect()
}

// ---- the public test material of the base image

/// The base image's observation, without candidate secret text: every
/// finding, the identity of each file and the regions found.
fn base_observation() -> (Vec<Document>, Document, Document) {
    let evidence = evidence("fedora-base-2026-09-30.json");
    let (mut rows, mut paths, mut spans) = (Vec::new(), Vec::new(), Vec::new());
    for file in items(&evidence, "files") {
        let name = text(&file, "neutralName");
        let identity = ["path", "sha256", "projectionSha256", "projectionFormat"];
        let identity = identity.map(|key| (key, field(&file, key).clone()));
        paths.push((name.to_owned(), Document::record(identity)));
        for finding in items(&file, "findings") {
            let span = field(&finding, "span").clone();
            rows.push(row(name, field(&finding, "rule"), &span));
            spans.push(span);
        }
    }
    let format = field(&evidence, "matchFormat");
    (rows, Document::Record(paths), regions(format, spans))
}

#[test]
fn only_the_fifteen_observed_base_findings_are_reviewed() {
    let (rows, paths, spans) = base_observation();
    let reviews = current();
    assert_eq!(rows.len(), 15);
    assert_eq!(paths.fields().map(<[_]>::len), Some(5));
    assert_eq!(
        items(&verdict(&rows, &[], &paths, &spans), "blocked").len(),
        15
    );
    let result = verdict(&rows, &reviews, &paths, &spans);
    assert!(passed(&result));
    let waived = items(&result, "waived");
    assert_eq!(waived.len(), 15);
    for finding in &waived {
        let reviewed = text(finding, "fingerprint");
        let remaining = without(&reviews, reviewed);
        let blocked = items(&verdict(&rows, &remaining, &paths, &spans), "blocked");
        assert_eq!(blocked.len(), 1, "{reviewed}");
        assert_eq!(text(&blocked[0], "fingerprint"), reviewed);
    }
}

#[test]
fn a_changed_base_span_path_or_rule_cannot_inherit_review() {
    let (rows, paths, spans) = base_observation();
    let reviews = current();
    let format = field(&spans, "format");
    for (finding, original) in rows.iter().zip(items(&spans, "findings")) {
        let name = text(finding, "File");
        for (changed, value) in [
            ("spanSha256", Document::from("a".repeat(64))),
            ("spanBytes", Document::from(1_u64)),
            ("path", Document::from("other/unreviewed-file")),
            ("rule", Document::from("github-pat")),
        ] {
            let mut altered_paths = paths.clone();
            let mut altered_row = finding.clone();
            let span = with(&original, changed, value.clone());
            if changed == "path" {
                let identity = with(field(&paths, name), "path", value.clone());
                altered_paths.put(name, identity);
            }
            if changed == "rule" {
                altered_row.put("RuleID", value);
            }
            let result = verdict(
                &[altered_row],
                &reviews,
                &altered_paths,
                &regions(format, vec![span]),
            );
            assert!(!passed(&result), "{name} {changed}");
        }
    }
}

#[test]
fn unreviewed_database_and_application_content_remain_blocking() {
    let (rows, paths, spans) = base_observation();
    let reviews = current();
    let name = text(&rows[0], "File");
    let original = items(&spans, "findings")[0].clone();
    for path in [
        "001/usr/lib/sysimage/rpm/rpmdb.sqlite",
        "008/app/web/dist/assets/app.js",
    ] {
        let mut altered = paths.clone();
        altered.put(
            "content-999999",
            with(field(&paths, name), "path", path.into()),
        );
        let mut found = rows.clone();
        found.push(with(&rows[0], "File", "content-999999".into()));
        let mut spanned = items(&spans, "findings");
        spanned.push(with(&original, "path", path.into()));
        let result = verdict(
            &found,
            &reviews,
            &altered,
            &regions(field(&spans, "format"), spanned),
        );
        assert!(!passed(&result), "{path}");
        assert_eq!(items(&result, "waived").len(), 15, "{path}");
        assert_eq!(items(&result, "blocked").len(), 1, "{path}");
    }
}

// ---- the public regions of the rpm database's index

const EVIDENCE: &str = "fedora-rpm-2026-09-30-pr6.json";
const HISTORICAL: &str = "fedora-rpm-2026-09-30.json";
const REFRESH: &str = "fedora-rpm-2026-10-19.json";
const DIRECT: &str = "fedora-rpm-2026-10-29.json";
const CURRENT: &str = "fedora-rpm-2026-11-11.json";
const RETIRING: &str = "fedora-rpm-2026-11-08.json";
const BASE: &str = "fedora-base-2026-09-30.json";
const DATABASE: &str = "001/usr/lib/sysimage/rpm/rpmdb.sqlite";

/// The database regions a refreshed index of `name` no longer contains.
fn dormant(name: &str) -> BTreeSet<String> {
    let trigger = field(&evidence(name), "trigger").clone();
    let removed = items(&trigger, "dormantFingerprintsRemovedFromActiveLedger");
    removed
        .iter()
        .map(|item| item.text().expect("a fingerprint").to_owned())
        .collect()
}

/// The reviewed fingerprints a refresh of `name` saw again.
fn recurring(name: &str) -> BTreeSet<String> {
    let trigger = field(&evidence(name), "trigger").clone();
    let seen = items(&trigger, "recurringReviewedFingerprints");
    seen.iter()
        .map(|item| item.text().expect("a fingerprint").to_owned())
        .collect()
}

/// A single-file observation of the evidence `name`, canonical or replayed.
fn observation(name: &str, replay: bool) -> (Vec<Document>, Document, Document) {
    let evidence = evidence(name);
    let canonical = field(&evidence, "file").clone();
    let file = text(&canonical, "neutralName").to_owned();
    let identity = if replay {
        field(field(&evidence, "replay"), "file").clone()
    } else {
        canonical
    };
    let key = if replay { "replaySpan" } else { "span" };
    let spans: Vec<Document> = items(&evidence, "findings")
        .iter()
        .map(|finding| field(finding, key).clone())
        .collect();
    let rows = spans
        .iter()
        .map(|span| row(&file, field(span, "rule"), span))
        .collect();
    let paths = Document::Record(vec![(file, identity)]);
    (rows, paths, regions(field(&evidence, "matchFormat"), spans))
}

/// The 2026-11-11 observation of several files of layer 001, canonical or
/// replayed.
fn current_observation(replay: bool) -> (Vec<Document>, Document, Document) {
    let evidence = evidence(CURRENT);
    let (mut paths, mut names) = (Vec::new(), Vec::new());
    for file in items(&evidence, "files") {
        let name = text(&file, "neutralName").to_owned();
        names.push((text(&file, "path").to_owned(), name.clone()));
        let source = if replay {
            field(&file, "replay").clone()
        } else {
            file.clone()
        };
        let mut identity = vec![("path".to_owned(), field(&file, "path").clone())];
        for key in [
            "sha256",
            "sourceBytes",
            "projectionSha256",
            "projectionBytes",
        ] {
            identity.push((key.to_owned(), field(&source, key).clone()));
        }
        identity.push((
            "projectionFormat".to_owned(),
            field(&source, "projectionFormat").clone(),
        ));
        paths.push((name, Document::Record(identity)));
    }
    let key = if replay { "replaySpan" } else { "span" };
    let (mut spans, mut rows) = (Vec::new(), Vec::new());
    for finding in items(&evidence, "findings") {
        let span = field(&finding, key).clone();
        let path = text(&span, "path");
        let found = names.iter().find(|(held, _)| held == path);
        let name = &found.expect("a file of the observation").1;
        rows.push(row(name, field(&span, "rule"), &span));
        spans.push(span);
    }
    let format = field(&evidence, "matchFormat");
    (rows, Document::Record(paths), regions(format, spans))
}

/// The thirteen current 2026-11-11 regions.
fn current_findings() -> Vec<Document> {
    items(&evidence(CURRENT), "findings")
}

/// The fingerprint of the region `span`.
fn fingerprinted(span: &Document) -> String {
    fingerprint(span, &spans::Limits::REVIEWED).expect("a fingerprint")
}

#[test]
fn each_current_region_requires_its_individual_review() {
    let (rows, paths, spans) = current_observation(false);
    let reviews = current();
    assert_eq!(rows.len(), 13);
    assert_eq!(
        items(&verdict(&rows, &[], &paths, &spans), "blocked").len(),
        13
    );
    let result = verdict(&rows, &reviews, &paths, &spans);
    assert!(passed(&result));
    assert_eq!(items(&result, "waived").len(), 13);
    let observed: BTreeSet<String> = current_findings()
        .iter()
        .map(|finding| text(finding, "fingerprint").to_owned())
        .collect();
    assert_eq!(fingerprints(&result, "waived"), observed);
    for reviewed in observed {
        let remaining = without(&reviews, &reviewed);
        let blocked = fingerprints(&verdict(&rows, &remaining, &paths, &spans), "blocked");
        assert_eq!(blocked, BTreeSet::from([reviewed.clone()]), "{reviewed}");
    }
}

#[test]
fn a_changed_region_length_rule_or_path_cannot_inherit_review() {
    let reviews = current();
    for (rows, paths, spans) in [current_observation(false), current_observation(true)] {
        let format = field(&spans, "format");
        for (finding, original) in rows.iter().zip(items(&spans, "findings")) {
            let name = text(finding, "File");
            let named = fingerprinted(&original);
            let rule = if text(&original, "rule") == "private-key" {
                "generic-api-key"
            } else {
                "private-key"
            };
            for (changed, value) in [
                ("spanSha256", Document::from("a".repeat(64))),
                ("spanBytes", Document::from(1_u64)),
                ("rule", Document::from(rule)),
                ("path", Document::from("002/etc/private.env")),
            ] {
                let mut changed_paths = paths.clone();
                let mut changed_row = finding.clone();
                let changed_span = with(&original, changed, value.clone());
                if changed == "rule" {
                    changed_row.put("RuleID", value.clone());
                }
                if changed == "path" {
                    let identity = with(field(&paths, name), "path", value);
                    changed_paths.put(name, identity);
                }
                let report = regions(format, vec![changed_span]);
                let result = verdict(&[changed_row], &reviews, &changed_paths, &report);
                assert!(!passed(&result), "{named} {changed}");
            }
        }
    }
}

#[test]
fn replay_and_canonical_bind_the_same_regions_with_different_complete_files() {
    let reviews = current();
    let results: Vec<Document> = [current_observation(false), current_observation(true)]
        .iter()
        .map(|(rows, paths, spans)| verdict(rows, &reviews, paths, spans))
        .collect();
    assert!(results.iter().all(passed));
    let mut moved = 0;
    let (left, right) = (items(&results[0], "waived"), items(&results[1], "waived"));
    assert_eq!(left.len(), right.len());
    for (canonical, replay) in left.iter().zip(right) {
        for name in [
            "fingerprint",
            "spanSha256",
            "spanBytes",
            "startColumn",
            "endColumn",
        ] {
            assert_eq!(field(canonical, name), field(&replay, name), "{name}");
        }
        let database = text(canonical, "path") == DATABASE;
        moved += usize::from(field(canonical, "startLine") != field(&replay, "startLine"));
        for name in ["fileSha256", "projectionSha256", "startLine", "endLine"] {
            if database && name.ends_with("Sha256") {
                assert_ne!(field(canonical, name), field(&replay, name), "{name}");
            } else if !database {
                assert_eq!(field(canonical, name), field(&replay, name), "{name}");
            }
        }
    }
    // Eight database regions keep their projection lines.
    assert_eq!(moved, 1);
    // Historical replays still bind the same identities, now uniformly inactive.
    for (name, same_coordinates) in [(EVIDENCE, true), (REFRESH, false)] {
        let historical: Vec<Document> = [observation(name, false), observation(name, true)]
            .iter()
            .map(|(rows, paths, spans)| verdict(rows, &reviews, paths, spans))
            .collect();
        assert!(!passed(&historical[0]) && !passed(&historical[1]), "{name}");
        assert!(items(&historical[0], "waived").is_empty(), "{name}");
        let (left, right) = (
            items(&historical[0], "blocked"),
            items(&historical[1], "blocked"),
        );
        assert_eq!(left.len(), right.len(), "{name}");
        for (canonical, replay) in left.iter().zip(right) {
            assert_eq!(
                field(canonical, "fingerprint"),
                field(&replay, "fingerprint")
            );
            assert_ne!(field(canonical, "fileSha256"), field(&replay, "fileSha256"));
            let same = field(canonical, "startLine") == field(&replay, "startLine");
            assert_eq!(same, same_coordinates, "{name}");
        }
    }
}

#[test]
fn superseded_fingerprints_are_historical_not_active_reviews() {
    let reviews = current();
    let active: BTreeSet<String> = reviews
        .iter()
        .map(|item| item.fingerprint.clone())
        .collect();
    let mut observed = std::collections::BTreeMap::new();
    for name in [HISTORICAL, EVIDENCE, REFRESH, DIRECT] {
        let (rows, paths, spans) = observation(name, false);
        let previous: BTreeSet<String> = items(&spans, "findings")
            .iter()
            .map(fingerprinted)
            .collect();
        assert!(previous.is_disjoint(&active), "{name}");
        let result = verdict(&rows, &reviews, &paths, &spans);
        assert!(!passed(&result), "{name}");
        assert_eq!(items(&result, "blocked").len(), previous.len(), "{name}");
        assert!(items(&result, "waived").is_empty(), "{name}");
        observed.insert(name, previous);
    }
    let counts: Vec<usize> = [HISTORICAL, EVIDENCE, REFRESH]
        .iter()
        .map(|name| observed[name].len())
        .collect();
    assert_eq!(counts, [4, 4, 2]);
    let first_dormant = dormant(REFRESH);
    assert_eq!(first_dormant.len(), 2);
    assert!(first_dormant.is_subset(&observed[EVIDENCE]));
    let refreshed = recurring(REFRESH);
    let together: BTreeSet<String> = refreshed.union(&first_dormant).cloned().collect();
    assert_eq!(together, observed[EVIDENCE]);
    // The six regions active before 2026-11-08 are exactly the ones its
    // refresh retires.
    let retired = dormant(RETIRING);
    let expected: BTreeSet<String> = refreshed
        .iter()
        .chain(&observed[REFRESH])
        .chain(&observed[DIRECT])
        .cloned()
        .collect();
    assert_eq!(retired, expected);
    assert_eq!(retired.len(), 6);
    assert!(recurring(DIRECT).is_subset(&retired));
    let newly = dormant(CURRENT);
    let still: BTreeSet<&String> = retired
        .union(&newly)
        .filter(|item| active.contains(*item))
        .collect();
    assert_eq!((newly.len(), still.len()), (3, 0));
    let trigger = field(&evidence(CURRENT), "trigger").clone();
    assert_eq!(items(&trigger, "recurringReviewedFingerprints").len(), 5);
    let now: BTreeSet<String> = current_findings()
        .iter()
        .map(|finding| text(finding, "fingerprint").to_owned())
        .collect();
    assert!(retired.is_disjoint(&now));
}

#[test]
fn complete_source_regions_have_bounded_structural_coverage() {
    let evidence = evidence(EVIDENCE);
    let replay = field(&evidence, "replay");
    assert_eq!(field(replay, "wholeProjectionEqual"), &Document::Bool(true));
    assert_eq!(field(replay, "wholeDatabaseEqual"), &Document::Bool(false));
    assert_eq!(
        field(replay, "installedRuntimePackageIdentitiesEqual").whole(),
        Some(148)
    );
    let found = items(&evidence, "findings");
    let mut freeblocks = Vec::new();
    for region in &found {
        let context = field(region, "context");
        let span = field(region, "span");
        assert_eq!(
            field(context, "coveredSourceBytes"),
            field(span, "spanBytes")
        );
        assert_eq!(field(context, "spanSha256"), field(span, "spanSha256"));
        let bounded = field(
            context,
            "allBytesAreFilenameIndexRecordsOrFreeblockMetadata",
        );
        assert_eq!(bounded, &Document::Bool(true));
        assert_eq!(text(context, "index"), "Basenames_key_idx");
        for record in items(context, "records") {
            assert_eq!(text(&record, "package"), "fedora-gpg-keys");
        }
        freeblocks.extend(items(context, "freeblocks"));
    }
    assert_eq!(freeblocks.len(), 1);
    assert_eq!(field(&freeblocks[0], "bytes").whole(), Some(34));
    let base = field(&freeblocks[0], "authenticatedBaseLiveRecord");
    assert_eq!(text(base, "package"), "fedora-gpg-keys");
}

/// Whether `text` is 64 small hexadecimal digits.
fn digest(text: &str) -> bool {
    crate::image::small_hex(text, 64)
}

#[test]
fn refreshed_regions_have_bounded_structural_coverage() {
    let evidence = evidence(REFRESH);
    let replay = field(&evidence, "replay");
    for (name, value) in [
        ("wholeProjectionEqual", false),
        ("wholeDatabaseEqual", false),
        ("bothCompleteMatchDigestsAndLengthsEqual", true),
    ] {
        assert_eq!(field(replay, name), &Document::Bool(value), "{name}");
    }
    assert_eq!(
        field(replay, "installedRuntimePackageIdentitiesEqual").whole(),
        Some(148)
    );
    assert_eq!(text(replay, "sqliteQuickCheck"), "ok");
    assert_eq!(text(replay, "scannerVersion"), "8.30.1");
    let found = items(&evidence, "findings");
    assert_eq!(found.len(), 2);
    assert_eq!(text(field(&evidence, "package"), "name"), "fedora-gpg-keys");
    let mut freeblocks = Vec::new();
    for region in &found {
        let context = field(region, "context");
        let span = field(region, "span");
        assert_eq!(text(span, "path"), DATABASE);
        assert_eq!(text(span, "rule"), "generic-api-key");
        assert_eq!(
            field(context, "coveredSourceBytes"),
            field(span, "spanBytes")
        );
        assert_eq!(field(context, "spanSha256"), field(span, "spanSha256"));
        let bounded = field(
            context,
            "allBytesAreFilenameIndexRecordsOrFreeblockMetadata",
        );
        assert_eq!(bounded, &Document::Bool(true));
        assert_eq!(text(context, "index"), "Basenames_key_idx");
        assert_eq!(fingerprinted(span), text(region, "fingerprint"));
        for record in items(context, "records") {
            assert_eq!(text(&record, "package"), "fedora-gpg-keys");
            assert!(digest(text(&record, "filenameSha256")));
        }
        freeblocks.extend(items(context, "freeblocks"));
    }
    assert_eq!(freeblocks.len(), 1);
    assert_eq!(field(&freeblocks[0], "bytes").whole(), Some(32));
    let covered = field(&freeblocks[0], "coveredContentIsTheLiveFilenameBytes");
    assert_eq!(covered, &Document::Bool(true));
    let base = field(&freeblocks[0], "authenticatedBaseLiveRecord");
    assert_eq!(text(base, "package"), "fedora-gpg-keys");
}

#[test]
fn current_database_regions_have_bounded_structural_coverage() {
    let evidence = evidence(CURRENT);
    let replay = field(&evidence, "replay");
    for (name, value) in [
        ("wholeDatabaseEqual", false),
        ("wholeProjectionEqual", false),
        ("detectorFingerprintsEqualCanonicalDatabaseRegions", true),
        ("profileAndMagicFilesWholeFileEqual", true),
    ] {
        assert_eq!(field(replay, name), &Document::Bool(value), "{name}");
    }
    assert_eq!(
        field(replay, "installedRuntimePackageIdentitiesEqual").whole(),
        Some(148)
    );
    assert_eq!(text(replay, "sqliteIntegrityCheck"), "ok");
    assert_eq!(text(replay, "scannerVersion"), "8.30.1");
    assert_eq!(
        field(&evidence, "canonicalImagePassed"),
        &Document::Bool(false)
    );
    let found: Vec<Document> = current_findings()
        .into_iter()
        .filter(|finding| text(field(finding, "span"), "path") == DATABASE)
        .collect();
    assert_eq!(found.len(), 9);
    let mut lengths: Vec<i128> = found
        .iter()
        .map(|region| {
            field(field(region, "span"), "spanBytes")
                .whole()
                .expect("bytes")
        })
        .collect();
    lengths.sort_unstable();
    assert_eq!(lengths, [57, 58, 58, 58, 58, 61, 62, 62, 65]);
    let mut freeblocks = 0;
    for region in &found {
        let context = field(region, "context");
        let span = field(region, "span");
        let named = text(region, "fingerprint");
        assert_eq!(text(span, "rule"), "generic-api-key", "{named}");
        assert_eq!(fingerprinted(span), named);
        assert_eq!(
            field(context, "coveredSourceBytes"),
            field(span, "spanBytes"),
            "{named}"
        );
        assert_eq!(
            field(context, "rawBytes"),
            field(span, "spanBytes"),
            "{named}"
        );
        for name in [
            "allBytesAreFilenameIndexRecordsOrFreeblockMetadata",
            "projectionOfRawEqualsMatch",
        ] {
            assert_eq!(
                field(context, name),
                &Document::Bool(true),
                "{named} {name}"
            );
        }
        assert_eq!(text(context, "btree"), "Basenames_key_idx", "{named}");
        let records = items(context, "liveRecords");
        assert!(!records.is_empty(), "{named}");
        for record in &records {
            assert_eq!(text(record, "package"), "fedora-gpg-keys");
            assert_eq!(text(record, "packageVersion"), "44-2");
            let bases = items(record, "baseLiveRecords");
            assert!(!bases.is_empty(), "{named}");
            for base in bases {
                assert_eq!(text(&base, "package"), "fedora-gpg-keys");
                assert_eq!(text(&base, "packageVersion"), "44-1");
                assert_eq!(
                    field(&base, "filenameSha256"),
                    field(record, "filenameSha256")
                );
            }
        }
        for block in items(context, "freeblocks") {
            freeblocks += 1;
            for name in [
                "headerFullyCovered",
                "staleFilenameEqualsLiveFilenameInSameRegion",
                "staleFilenameIsCompleteLiveKeyInCurrentDatabase",
                "staleRecordEqualsImmutableBaseRecord",
            ] {
                assert_eq!(field(&block, name), &Document::Bool(true), "{named} {name}");
            }
        }
        let candidate = field(region, "candidateContext");
        assert!(digest(text(candidate, "candidateSha256")), "{named}");
    }
    assert_eq!(freeblocks, 6);
}

#[test]
fn refreshed_layer_copies_repeat_reviewed_base_bytes_at_their_own_path() {
    let base = evidence(BASE);
    let mut reviewed = std::collections::BTreeMap::new();
    for file in items(&base, "files") {
        for finding in items(&file, "findings") {
            reviewed.insert(
                text(&finding, "fingerprint").to_owned(),
                field(&finding, "span").clone(),
            );
        }
    }
    let reviews = current();
    let (rows, paths, spans) = current_observation(false);
    let copies: Vec<Document> = current_findings()
        .into_iter()
        .filter(|finding| text(field(finding, "span"), "path") != DATABASE)
        .collect();
    assert_eq!(copies.len(), 4);
    for finding in &copies {
        let span = field(finding, "span");
        let named = text(finding, "fingerprint");
        let counterpart = field(field(finding, "context"), "reviewedBaseCounterpart");
        let original = &reviewed[text(counterpart, "fingerprint")];
        let path = text(span, "path");
        assert_eq!(
            text(original, "path"),
            format!("000/{}", &path[4..]),
            "{named}"
        );
        for name in ["rule", "spanBytes", "spanSha256"] {
            assert_eq!(field(original, name), field(span, name), "{named} {name}");
        }
        assert_ne!(text(counterpart, "fingerprint"), named);
        let counted = reviews
            .iter()
            .any(|item| item.fingerprint == text(counterpart, "fingerprint"));
        assert!(counted, "{named}");
        let remaining = without(&reviews, named);
        let blocked = fingerprints(&verdict(&rows, &remaining, &paths, &spans), "blocked");
        assert_eq!(blocked, BTreeSet::from([named.to_owned()]));
        let entry = reviews
            .iter()
            .find(|item| item.fingerprint == named)
            .expect("a review");
        assert_eq!(entry.scope, path);
    }
}

#[test]
fn an_additional_database_finding_is_not_covered() {
    let (rows, paths, spans) = current_observation(false);
    let spanned = items(&spans, "findings");
    let database = spanned
        .iter()
        .position(|span| text(span, "path") == DATABASE)
        .expect("a database region");
    let extra = with(&spanned[database], "spanSha256", "b".repeat(64).into());
    let mut found = rows.clone();
    found.push(rows[database].clone());
    let mut all = spanned;
    all.push(extra);
    let result = verdict(
        &found,
        &current(),
        &paths,
        &regions(field(&spans, "format"), all),
    );
    assert!(!passed(&result));
    assert_eq!(items(&result, "waived").len(), 13);
    assert_eq!(items(&result, "blocked").len(), 1);
}
