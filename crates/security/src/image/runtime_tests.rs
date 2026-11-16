//! Replays of `recorded_runtime.json`: what the reference in Python made
//! of source reviews, of the image's filesystem profile, of published
//! report digests, of whole runtime proofs, of the advisory's affected
//! scopes, of dispositions and of their absence.

use super::{
    Evidence, Limits, affected_scopes, apply, encoded, filesystem_profile, not_required, proof,
    report_digest, source_review, written,
};
use crate::{
    Fault,
    document::Document,
    image::{
        notices::tests::built,
        policy::{
            Moment,
            tests::{There, tallied},
        },
        secrets::tests::{here, text, there},
    },
    policy::Date,
    scratch::{Scratch, base64},
};

const RECORDING: &[u8] = include_bytes!("recorded_runtime.json");
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

fn whole(case: &Document, name: &str) -> i128 {
    case.get(name)
        .and_then(Document::whole)
        .unwrap_or_else(|| panic!("no number {name}"))
}

fn reviews() -> Vec<(String, String, String)> {
    let mut all = Vec::new();
    for case in cases("reviews") {
        let scratch = Scratch::new("runtime");
        let root = built(&case, &scratch.0);
        let asked = case
            .get("asked")
            .and_then(Document::items)
            .expect("answers")
            .to_vec();
        let mut answers = asked.iter();
        let today = Date::parse(text(&case, "today")).expect("a day");
        let came = {
            let mut git = |arguments: &[String], limit: u64| -> crate::Result<Vec<u8>> {
                let answer = answers.next().expect("an answer git gave");
                let recorded: Vec<&str> = answer
                    .get("arguments")
                    .and_then(Document::items)
                    .expect("arguments")
                    .iter()
                    .map(|argument| argument.text().expect("text"))
                    .collect();
                assert_eq!(arguments, recorded.as_slice());
                assert_eq!(i128::from(limit), whole(answer, "limit"));
                match answer.get("output").and_then(Document::text) {
                    Some(output) => Ok(base64(output)),
                    None => Err(Fault("runtime_source_git")),
                }
            };
            source_review(
                &root,
                text(&case, "revision"),
                today,
                &Limits::REVIEWED,
                &mut git,
            )
        };
        assert!(
            answers.next().is_none(),
            "{}: git was asked less",
            text(&case, "name")
        );
        all.push((text(&case, "name").to_owned(), here(came), there(&case)));
    }
    all
}

fn limited(case: &Document) -> Limits {
    let mut limits = Limits::REVIEWED;
    for (name, value) in case
        .get("limits")
        .and_then(Document::fields)
        .expect("limits")
    {
        let value = usize::try_from(value.whole().expect("a bound")).expect("a bound");
        match name.as_str() {
            "MAX_ENTRIES" => limits.entries = value,
            "MAX_DEPTH" => limits.depth = value,
            other => panic!("no bound {other}"),
        }
    }
    limits
}

fn filesystems() -> Vec<(String, String, String)> {
    let nss = base64(text(&recording(), "nss"));
    let mut all = Vec::new();
    for case in cases("filesystems") {
        let scratch = Scratch::new("runtime");
        let root = built(&case, &scratch.0);
        let came = filesystem_profile(&root, &nss, &limited(&case));
        all.push((text(&case, "name").to_owned(), here(came), there(&case)));
    }
    all
}

fn digests() -> Vec<(String, String, String)> {
    let mut all = Vec::new();
    for case in cases("digests") {
        let scratch = Scratch::new("runtime");
        let root = built(&case, &scratch.0);
        let expected = case.get("expected").expect("a value");
        let limit = case.get("limit").and_then(Document::whole);
        let limit = limit.map_or(Limits::REVIEWED.report, |limit| {
            8 * u64::try_from(limit).expect("a bound")
        });
        let came = report_digest(&root.join("report.json"), expected, limit).map(Document::from);
        all.push((text(&case, "name").to_owned(), here(came), there(&case)));
    }
    all
}

fn proofs() -> Vec<(String, String, String)> {
    let policy = text(&recording(), "advisoryPolicySha256").to_owned();
    let mut all = Vec::new();
    for case in cases("proofs") {
        let scratch = Scratch::new("runtime");
        let base = built(&case, &scratch.0);
        let (tree, root) = (base.join("output/archive"), base.join("checkout"));
        let recorded = case.get("review").expect("a review");
        let mut review = |revision: &str| -> crate::Result<Document> {
            assert_eq!(revision, "b".repeat(40));
            match recorded.get("refused").and_then(Document::text) {
                Some("runtime_source_review_expired") => {
                    Err(Fault("runtime_source_review_expired"))
                }
                Some(other) => panic!("no refusal {other}"),
                None => Ok(recorded.clone()),
            }
        };
        let evidence = Evidence {
            tree: &tree,
            elf: case.get("elf").expect("binary evidence"),
            native: case.get("native").expect("a native record"),
            manifest: case.get("manifest").expect("a manifest"),
            root: &root,
            sbom_sha256: text(&case, "sbom"),
            advisory_policy_sha256: &policy,
        };
        let came = proof(&evidence, &Limits::REVIEWED, &mut review);
        all.push((text(&case, "name").to_owned(), here(came), there(&case)));
    }
    all
}

fn scopes() -> Vec<(String, String, String)> {
    let policy = text(&recording(), "advisoryPolicySha256").to_owned();
    let mut all = Vec::new();
    for case in cases("scopes") {
        let verdict = case.get("verdict").expect("a verdict");
        let came = affected_scopes(verdict, &policy);
        let came = came.map(|scopes| {
            Document::List(scopes.iter().map(|scope| scope.as_str().into()).collect())
        });
        all.push((text(&case, "name").to_owned(), here(came), there(&case)));
    }
    all
}

fn applies() -> Vec<(String, String, String)> {
    let policy = text(&recording(), "advisoryPolicySha256").to_owned();
    let mut all = Vec::new();
    for case in cases("applies") {
        let scratch = Scratch::new("runtime");
        let root = built(&case, &scratch.0);
        let (verdict, evidence) = (
            case.get("verdict").expect("a verdict"),
            case.get("evidence").expect("a proof"),
        );
        let now = Moment(i64::try_from(whole(&case, "moment")).expect("a moment"));
        let came = apply(
            verdict,
            evidence,
            &root.join("runtime-proof.json"),
            &policy,
            now,
            &Limits::REVIEWED,
        );
        let came = came.map(|(result, document)| Document::List(vec![result, document]));
        all.push((text(&case, "name").to_owned(), here(came), there(&case)));
    }
    all
}

fn absences() -> Vec<(String, String, String)> {
    let policy = text(&recording(), "advisoryPolicySha256").to_owned();
    let mut all = Vec::new();
    for case in cases("absences") {
        let (manifest, verdict) = (
            case.get("manifest").expect("a manifest"),
            case.get("verdict").expect("a verdict"),
        );
        let now = Moment(i64::try_from(whole(&case, "moment")).expect("a moment"));
        let came = not_required(manifest, text(&case, "imageId"), verdict, &policy, now);
        let came = came.map(|(evidence, document)| Document::List(vec![evidence, document]));
        all.push((text(&case, "name").to_owned(), here(came), there(&case)));
    }
    all
}

#[test]
fn the_bounds_are_the_reviewed_ones() {
    let recording = recording();
    let bounds = recording.get("bounds").expect("bounds");
    let bound = |name: &str| bounds.get(name).and_then(Document::whole).expect("a bound");
    let reviewed = Limits::REVIEWED;
    assert_eq!(bound("MAX_REVIEW"), i128::from(reviewed.review));
    assert_eq!(bound("MAX_SOURCE"), i128::from(reviewed.source));
    assert_eq!(8 * bound("MAX_SOURCE"), i128::from(reviewed.report));
    assert_eq!(bound("MAX_ENTRIES"), reviewed.entries as i128);
    assert_eq!(bound("MAX_DEPTH"), reviewed.depth as i128);
    assert_eq!(bound("MAX_ENVIRONMENT"), reviewed.environment as i128);
    assert_eq!(
        bound("MAX_ENVIRONMENT_BYTES"),
        reviewed.environment_bytes as i128
    );
    assert_eq!(bound("MAX_REVIEW_DAYS"), i128::from(reviewed.review_days));
}

#[test]
fn every_recorded_moment_is_written_alike() {
    for case in cases("moments") {
        let moment = Moment(i64::try_from(whole(&case, "micros")).expect("a moment"));
        assert_eq!(written(moment), text(&case, "written"));
    }
}

#[test]
fn every_recorded_binding_is_spelled_alike() {
    for case in cases("compacts") {
        assert_eq!(
            encoded(case.get("value").expect("a value")),
            text(&case, "written")
        );
    }
}

/// Reviews that the reference failed on in its interpreter's words: a file
/// that is not there, bytes that are no JSON object or no text, a field of
/// another kind, a day that is not one.
const UNREAD_REVIEWS: [(&str, &str); 12] = [
    (
        "a review of a date of other digits",
        "runtime_source_review_date",
    ),
    ("a review of a day that is not", "runtime_evidence_shape"),
    (
        "a review of a file digest that is a number",
        "runtime_evidence_shape",
    ),
    (
        "a review of a tree that is a number",
        "runtime_evidence_shape",
    ),
    ("a review that is a list", "runtime_evidence_shape"),
    ("a review that is no JSON", "runtime_evidence_shape"),
    (
        "a review whose assessment is a number",
        "runtime_evidence_shape",
    ),
    (
        "a review whose expiry is a number",
        "runtime_evidence_shape",
    ),
    ("a review whose trees are a list", "runtime_evidence_shape"),
    ("a working profile that is not there", "tool_entry_missing"),
    ("no review", "tool_entry_missing"),
    (
        "trees answered in bytes that are no text",
        "runtime_evidence_shape",
    ),
];

/// Trees of the image that the reference failed on in its interpreter's
/// words.
const UNREAD_FILESYSTEMS: [(&str, &str); 1] = [("no name switch", "runtime_evidence_shape")];

/// Published reports that the reference failed on in its interpreter's
/// words.
const UNREAD_DIGESTS: [(&str, &str); 5] = [
    ("a report that is a link", "tool_entry_unavailable"),
    ("a report that is a list", "runtime_evidence_shape"),
    ("a report that is no JSON", "runtime_evidence_shape"),
    (
        "a report that names a field twice",
        "runtime_evidence_shape",
    ),
    ("no report", "tool_entry_missing"),
];

/// Published reports that the reference took for the value they were made
/// of because its interpreter holds 1 equal to true and 1.0 equal to 1; a
/// report holds its value here in the same kinds.
const SAME_KIND: [(&str, &str); 2] = [
    (
        "a report of a fraction for a number",
        "runtime_report_changed",
    ),
    ("a report of a number for a truth", "runtime_report_changed"),
];

/// Proofs that the reference failed on in its interpreter's words.
const UNREAD_PROOFS: [(&str, &str); 10] = [
    (
        "an archive report without its image",
        "runtime_evidence_shape",
    ),
    (
        "an image configuration that is no JSON",
        "runtime_evidence_shape",
    ),
    (
        "an image whose Env is 'PATH=/usr/bin'",
        "runtime_evidence_shape",
    ),
    ("an image whose Env is [7]", "runtime_evidence_shape"),
    ("an image whose Labels is {}", "runtime_evidence_shape"),
    (
        "an image whose machine is a number",
        "runtime_evidence_shape",
    ),
    ("an image without its environment", "runtime_evidence_shape"),
    ("an image without its labels", "runtime_evidence_shape"),
    (
        "binary evidence without its checks",
        "runtime_evidence_shape",
    ),
    ("no published reports", "tool_entry_missing"),
];

/// Verdicts that the reference failed on in its interpreter's words.
const UNREAD_SCOPES: [(&str, &str); 2] = [
    ("a finding without its scope", "runtime_evidence_shape"),
    ("no reviewed advisories", "runtime_evidence_shape"),
];

/// Manifests that the reference failed on in its interpreter's words.
const UNREAD_ABSENCES: [(&str, &str); 1] =
    [("a manifest without its revision", "runtime_evidence_shape")];

#[test]
fn every_recorded_source_review_is_judged_alike() {
    let tables = [(&UNREAD_REVIEWS[..], There::Raised)];
    assert_eq!(tallied(reviews(), &tables), [7, 37, 12, 0, 0]);
}

#[test]
fn every_recorded_filesystem_profile_is_judged_alike() {
    let tables = [(&UNREAD_FILESYSTEMS[..], There::Raised)];
    assert_eq!(tallied(filesystems(), &tables), [4, 21, 1, 0, 0]);
}

#[test]
fn every_recorded_report_digest_is_the_same() {
    let tables = [
        (&UNREAD_DIGESTS[..], There::Raised),
        (&SAME_KIND[..], There::Held),
    ];
    assert_eq!(tallied(digests(), &tables), [4, 3, 5, 2, 0]);
}

#[test]
fn every_recorded_proof_is_the_same() {
    let tables = [(&UNREAD_PROOFS[..], There::Raised)];
    assert_eq!(tallied(proofs(), &tables), [6, 44, 10, 0, 0]);
}

#[test]
fn every_recorded_set_of_affected_scopes_is_the_same() {
    let tables = [(&UNREAD_SCOPES[..], There::Raised)];
    assert_eq!(tallied(scopes(), &tables), [4, 3, 2, 0, 0]);
}

#[test]
fn every_recorded_disposition_is_the_same() {
    assert_eq!(tallied(applies(), &[]), [4, 8, 0, 0, 0]);
}

#[test]
fn every_recorded_absence_of_a_claim_is_the_same() {
    let tables = [(&UNREAD_ABSENCES[..], There::Raised)];
    assert_eq!(tallied(absences(), &tables), [2, 1, 1, 0, 0]);
}
