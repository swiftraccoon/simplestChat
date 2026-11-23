//! Replays of `recorded_runtime.json`: what the reference in Python made
//! of source reviews, of the image's filesystem profile, of published
//! report digests, of whole runtime proofs, of the advisory's affected
//! scopes, of dispositions and of their absence. Three pins hold what the
//! proof binds in this checkout: the profile's source, the committed
//! review's digests of the profile files, and the image recipe's removal of
//! the loader cache and installation of the reviewed name switch.

use super::{
    Evidence, Limits, PROFILE, REVIEW, affected_scopes, apply, encoded, filesystem_profile,
    not_required, proof, report_digest, source_review, written,
};
use crate::{
    Fault, digest,
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
use std::path::Path;

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

/// The profile file that the recorded reviews bind, and the source that
/// holds the profile's checks now. A replayed review, its checkout and the
/// arguments git is asked name the second wherever the recording names the
/// first; the recorded bytes of that file stay as recorded.
const RENAMED_SINCE_RECORDING: (&str, &str) = (
    "ops/ansible/files/runtime_profile.py",
    "ops/src/release/profile.rs",
);

/// Move a replayed checkout's profile file to its current name and rename it
/// in the review, which names it at most once. Answers the review's bytes as
/// recorded and as renamed, or nothing when the case has no review.
fn renamed(root: &Path) -> Option<(Vec<u8>, Vec<u8>)> {
    let (recorded, current) = RENAMED_SINCE_RECORDING;
    if root.join(recorded).is_file() {
        let moved = root.join(current);
        std::fs::create_dir_all(moved.parent().expect("a directory")).expect("a directory");
        std::fs::rename(root.join(recorded), moved).expect("the profile file moved");
    }
    let review = root.join(REVIEW);
    if !review.is_file() {
        return None;
    }
    let body = std::fs::read(&review).expect("the review");
    let named = |window: &[u8]| window == recorded.as_bytes();
    let at: Vec<usize> = (0..body.len())
        .filter(|at| body[*at..].get(..recorded.len()).is_some_and(named))
        .collect();
    assert!(at.len() <= 1, "the review names its profile file once");
    let Some(&at) = at.first() else {
        return Some((body.clone(), body));
    };
    let mut changed = body[..at].to_vec();
    changed.extend_from_slice(current.as_bytes());
    changed.extend_from_slice(&body[at + recorded.len()..]);
    std::fs::write(&review, &changed).expect("the renamed review");
    Some((body, changed))
}

/// What a replayed review came to there, with its profile file renamed: a
/// review that held binds the file by its current name, and the digest it
/// answers, recorded as that of the recorded review, is that of the renamed
/// one.
fn renamed_there(case: &Document, review: Option<&(Vec<u8>, Vec<u8>)>) -> String {
    let Some(held) = case.get("result").and_then(|result| result.get("held")) else {
        return there(case);
    };
    let (recorded, current) = RENAMED_SINCE_RECORDING;
    let (original, changed) = review.expect("a review that held");
    let Document::Record(fields) = held.clone() else {
        panic!("a held review is a record");
    };
    let mut renamed = Vec::new();
    for (name, value) in fields {
        let value = match name.as_str() {
            "sha256" => {
                assert_eq!(value.text(), Some(digest(original).as_str()));
                digest(changed).into()
            }
            "profileFiles" => {
                let Document::Record(files) = value else {
                    panic!("the profile files are a record");
                };
                let files = files
                    .into_iter()
                    .map(|(path, bound)| match path == recorded {
                        true => (current.to_owned(), bound),
                        false => (path, bound),
                    });
                Document::Record(files.collect())
            }
            _ => value,
        };
        renamed.push((name, value));
    }
    format!("held {}", Document::Record(renamed).pretty())
}

fn reviews() -> Vec<(String, String, String)> {
    let (earlier, current) = RENAMED_SINCE_RECORDING;
    let mut all = Vec::new();
    for case in cases("reviews") {
        let scratch = Scratch::new("runtime");
        let root = built(&case, &scratch.0);
        let review = renamed(&root);
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
                let recorded: Vec<String> = answer
                    .get("arguments")
                    .and_then(Document::items)
                    .expect("arguments")
                    .iter()
                    .map(|argument| argument.text().expect("text").replace(earlier, current))
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
        let expected = renamed_there(&case, review.as_ref());
        all.push((text(&case, "name").to_owned(), here(came), expected));
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

/// The bytes of the file at `path` below this checkout.
fn checkout(path: &str) -> Vec<u8> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    std::fs::read(root.join(path)).unwrap_or_else(|_| panic!("no {path} in the checkout"))
}

/// The profile file the proof binds is the source of the image defaults the
/// proof applies: the file of `simplestchat-ops release profile` that
/// defines `image_defaults`.
#[test]
fn the_profile_file_is_the_source_of_the_image_profile() {
    assert_eq!(PROFILE, "ops/src/release/profile.rs");
    let source = String::from_utf8(checkout(PROFILE)).expect("Rust source is text");
    let defined = "\npub fn image_defaults(config: &[u8]) -> Result<(), Option<&'static str>> {\n";
    assert_eq!(source.matches(defined).count(), 1);
}

/// The committed review binds exactly the profile's source and the name
/// switch, each by the SHA-256 of this checkout's bytes: a changed byte of
/// either is a change that needs a fresh review.
#[test]
fn the_committed_review_binds_the_checkouts_profile_files() {
    let review = Document::read(&checkout(REVIEW)).expect("the review is JSON");
    let bound = review.get("profileFiles").and_then(Document::fields);
    let bound = bound.expect("the review's profile files");
    let names: Vec<&str> = bound.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(
        names,
        [
            "ops/src/release/profile.rs",
            "security/runtime/nsswitch.conf"
        ]
    );
    for (path, digested) in bound {
        assert_eq!(
            digested.text(),
            Some(digest(&checkout(path)).as_str()),
            "{path}"
        );
    }
}

/// The image recipe makes the runtime files the proof requires: the name
/// switch it installs reads local files for every table and adds DNS for
/// hosts alone, and its runtime stage removes the loader cache after its
/// package installation and before that switch is copied in, with nothing
/// from that stage onwards regenerating the cache.
#[test]
fn the_runtime_stage_removes_the_loader_cache_and_installs_the_reviewed_name_switch() {
    let switch = checkout("security/runtime/nsswitch.conf");
    let switch = String::from_utf8(switch).expect("the name switch is text");
    let tables: Vec<(&str, &str)> = switch
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| line.split_once(": ").expect("a table and its sources"))
        .collect();
    let sources = |name: &str| {
        let found = tables.iter().find(|(table, _)| *table == name);
        found.map(|(_, sources)| *sources)
    };
    assert_eq!(sources("hosts"), Some("files dns"));
    for name in ["passwd", "group", "shadow", "gshadow", "initgroups"] {
        assert_eq!(sources(name), Some("files"), "{name}");
    }
    let local = |(table, sources): &(&str, &str)| *table == "hosts" || *sources == "files";
    assert!(tables.iter().all(local));
    let recipe = String::from_utf8(checkout("Dockerfile")).expect("the recipe is text");
    let (_, stage) = recipe
        .split_once(" AS runtime-base\n")
        .expect("a runtime stage");
    let at = |text: &str| {
        let found = stage.find(text);
        found.unwrap_or_else(|| panic!("the runtime stage holds no {text}"))
    };
    assert!(at("dnf install") < at("rm -f /etc/ld.so.cache"));
    assert!(at("rm -f /etc/ld.so.cache") < at("COPY security/runtime/nsswitch.conf"));
    assert!(!stage.contains("ldconfig"));
}
