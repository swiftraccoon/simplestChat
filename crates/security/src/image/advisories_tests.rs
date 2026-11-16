//! Replays of `recorded_advisories.json`: what the reference in Python
//! made of the reviewed advisory's file, of which packages it looked at,
//! of their identities and of whole verdicts.

use super::{Limits, candidate, identity, reviewed_policy, verdict};
use crate::{
    document::Document,
    scratch::{Scratch, base64},
};
use std::path::{Path, PathBuf};

const RECORDING: &[u8] = include_bytes!("recorded_advisories.json");

fn cases(name: &str) -> Vec<Document> {
    let recording = Document::read(RECORDING).expect("a recording");
    let held = recording.get(name).and_then(Document::items);
    held.unwrap_or_else(|| panic!("no cases {name}")).to_vec()
}

fn text<'a>(case: &'a Document, name: &str) -> &'a str {
    let held = case.get(name).and_then(Document::text);
    held.unwrap_or_else(|| panic!("no text {name}"))
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

/// Write the policy of a case below `root` as its recorder left it.
fn policy(case: &Document, root: &Path, kind: &str, content: Option<&str>) -> PathBuf {
    let path = root.join("policy.json");
    match kind {
        "file" => std::fs::write(&path, base64(content.expect("a policy"))).expect("a file"),
        "link" => {
            let reviewed = cases("policies")[0].clone();
            std::fs::write(root.join("real.json"), base64(text(&reviewed, "content")))
                .expect("a file");
            std::os::unix::fs::symlink("real.json", &path).expect("a link");
        }
        "directory" => std::fs::create_dir(&path).expect("a directory"),
        "pipe" => {
            let made = std::process::Command::new("mkfifo").arg(&path).status();
            assert!(made.expect("mkfifo").success());
        }
        "absent" => {}
        other => panic!("no kind {other} of {}", text(case, "name")),
    }
    path
}

fn policies() -> Vec<(String, String, String)> {
    let mut all = Vec::new();
    for case in cases("policies") {
        let scratch = Scratch::new("advisory-policy");
        let content = case.get("content").and_then(Document::text);
        let path = policy(&case, &scratch.0, text(&case, "kind"), content);
        let came = reviewed_policy(&path, &Limits::REVIEWED);
        let came = came.map(|(policy, digest)| Document::List(vec![policy, digest.into()]));
        all.push((text(&case, "name").to_owned(), here(came), there(&case)));
    }
    all
}

fn candidates() -> Vec<(String, String, String)> {
    let mut all = Vec::new();
    for case in cases("candidates") {
        let came = candidate(case.get("package").expect("a package")).map(Document::from);
        all.push((text(&case, "name").to_owned(), here(came), there(&case)));
    }
    all
}

fn identities() -> Vec<(String, String, String)> {
    let mut all = Vec::new();
    for case in cases("identities") {
        let came = identity(case.get("package").expect("a package"));
        let came =
            came.map(|(scope, patch)| Document::List(vec![scope.into(), patch.to_string().into()]));
        all.push((text(&case, "name").to_owned(), here(came), there(&case)));
    }
    all
}

fn verdicts() -> Vec<(String, String, String)> {
    let mut all = Vec::new();
    for case in cases("verdicts") {
        let scratch = Scratch::new("advisory-verdict");
        let content = case.get("policy").and_then(Document::text);
        let kind = if content.is_some() { "file" } else { "absent" };
        let path = policy(&case, &scratch.0, kind, content);
        let mut limits = Limits::REVIEWED;
        if let Some(limit) = case.get("limit").and_then(Document::whole) {
            limits.packages = usize::try_from(limit).expect("a bound");
        }
        let packages = case
            .get("packages")
            .and_then(Document::items)
            .expect("packages");
        let came = verdict(packages, &path, &limits);
        all.push((text(&case, "name").to_owned(), here(came), there(&case)));
    }
    all
}

#[test]
fn the_reviewed_file_of_the_repository_is_the_recorded_one() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(super::POLICY_PATH);
    let (policy, digest) = reviewed_policy(&path, &Limits::REVIEWED).expect("the reviewed policy");
    let all = cases("policies");
    let recorded = all
        .iter()
        .find(|case| text(case, "name") == "the reviewed policy");
    let recorded = recorded.expect("a case");
    let bytes = std::fs::read(&path).expect("the reviewed file");
    assert_eq!(base64(text(recorded, "content")), bytes);
    let held = recorded.get("result").and_then(|result| result.get("held"));
    assert_eq!(held, Some(&Document::List(vec![policy, digest.into()])));
}

#[test]
fn the_reviewed_bounds_are_the_references() {
    let recording = Document::read(RECORDING).expect("a recording");
    let bounds = recording.get("bounds").expect("bounds");
    let bound = |name: &str| bounds.get(name).and_then(Document::whole).expect("a bound");
    assert_eq!(bound("MAX_POLICY"), i128::from(Limits::REVIEWED.policy));
    assert_eq!(bound("MAX_PACKAGES"), Limits::REVIEWED.packages as i128);
    assert_eq!(bound("MAX_EPOCH"), super::MAX_EPOCH);
    assert_eq!(bound("MAX_VERSION"), super::MAX_VERSION as i128);
    assert_eq!(bound("FIXED_PATCH"), super::FIXED_PATCH as i128);
}

/// Hold every case to the recording, but for those that `reworded` names
/// with the code they have here, where the reference failed in its
/// interpreter's words, and those that `otherwise` names with what they
/// come to here, where the reference came to something else. Answers how
/// many came to the same held, the same refusal, a refusal in other words
/// and something else.
fn tallied(
    all: Vec<(String, String, String)>,
    reworded: &[(&str, &str)],
    otherwise: &[(&str, &str)],
) -> [usize; 4] {
    let mut tally = [0; 4];
    for (name, here, there) in all {
        let named = |table: &[(&str, &str)]| {
            let found = table.iter().find(|(other, _)| *other == name);
            found.map(|(_, said)| (*said).to_owned())
        };
        if let Some(code) = named(reworded) {
            assert_eq!(here, format!("refused {code}"), "{name}");
            assert!(there.starts_with("RAISED "), "{name}");
            tally[2] += 1;
        } else if let Some(said) = named(otherwise) {
            // What is held is written on its own line.
            assert_eq!(here.trim_end(), said, "{name}");
            assert_ne!(here, there, "{name}");
            tally[3] += 1;
        } else {
            assert_eq!(here, there, "{name}");
            tally[usize::from(here.starts_with("refused "))] += 1;
        }
    }
    tally
}

/// Policy files that the reference failed on in its interpreter's words:
/// bytes that are no JSON object of text, a name twice, an assessment of
/// another kind, a file that cannot be opened.
const UNREAD: [(&str, &str); 10] = [
    ("a policy that is no JSON", "image_advisory_policy"),
    ("a policy that is a list", "image_advisory_policy"),
    ("a policy that is no text", "image_advisory_policy"),
    ("a policy that names a field twice", "image_advisory_policy"),
    ("a policy with more after it", "image_advisory_policy"),
    (
        "a policy with an assessment that is a number",
        "image_advisory_policy",
    ),
    (
        "a policy with an assessment that is nothing",
        "image_advisory_policy",
    ),
    ("a policy that is a link", "tool_entry_unavailable"),
    ("a policy that is not there", "tool_entry_missing"),
    ("a policy that is a directory", "nonregular_tool_file"),
];

/// Packages that the reference failed on in its interpreter's words when
/// it asked whether they are of OpenSSL: a missing name, a field of
/// another kind.
const UNDESCRIBED: [(&str, &str); 6] = [
    ("another package without a name", "image_advisory_package"),
    (
        "another package whose name is a number",
        "image_advisory_package",
    ),
    (
        "another package whose source is a number",
        "image_advisory_package",
    ),
    (
        "another package whose metadata is a list",
        "image_advisory_package",
    ),
    (
        "another package whose metadata is nothing",
        "image_advisory_package",
    ),
    (
        "another package whose address is a number",
        "image_advisory_package",
    ),
];

/// Packages that the reference passed over and that are asked here to
/// prove that they are of OpenSSL, or that they are none: their address
/// names such a package, but not where the reference's URL library looked
/// for it. Any address that the reference read as one of OpenSSL's is read
/// so here too.
const WIDER: [(&str, &str); 5] = [
    (
        "another package at 'pkg://rpm/fedora/openssl@1'",
        "held true",
    ),
    (
        "another package at 'pkg:/rpm/fedora/openssl@1'",
        "held true",
    ),
    (
        "another package at 'pkg:deb/x?q=rpm/fedora/openssl@1'",
        "held true",
    ),
    (
        "another package at 'pkg:deb/x#rpm/fedora/openssl@1'",
        "held true",
    ),
    ("another package at 'x/rpm/fedora/openssl@1'", "held true"),
];

/// Packages at an address whose host the reference's URL library could not
/// read, so that it raised. Here an address is only text when it is asked
/// whether it names a package of OpenSSL; one that does is held to its
/// identity as any other, and one that does not is passed over.
const UNREADABLE: [(&str, &str); 2] = [
    (
        "another package at an address whose host cannot be read",
        "held false",
    ),
    (
        "another package at an address of OpenSSL whose host cannot be read",
        "held true",
    ),
];

/// Packages of OpenSSL that the reference failed on in its interpreter's
/// words: a missing field, a field of another kind, a query that is no
/// list of names and values.
const UNIDENTIFIED: [(&str, &str); 19] = [
    ("a machine that is a list", "image_advisory_rpm_identity"),
    ("a package with no metadata", "image_advisory_package"),
    (
        "a package with a source that is a number",
        "image_advisory_package",
    ),
    (
        "a package with a name that is a number",
        "image_advisory_package",
    ),
    (
        "a package with a release that is a number",
        "image_advisory_package",
    ),
    ("a package without its name", "image_advisory_package"),
    ("a package without its version", "image_advisory_package"),
    ("a package without its purl", "image_advisory_package"),
    (
        "a package without its metadataType",
        "image_advisory_package",
    ),
    ("a package without its metadata", "image_advisory_package"),
    ("metadata without its name", "image_advisory_package"),
    ("metadata without its version", "image_advisory_package"),
    ("metadata without its release", "image_advisory_package"),
    ("metadata without its epoch", "image_advisory_package"),
    (
        "metadata without its architecture",
        "image_advisory_package",
    ),
    ("metadata without its sourceRpm", "image_advisory_package"),
    (
        "an address with an empty qualifier",
        "image_advisory_purl_identity",
    ),
    (
        "an address with a qualifier without its sign",
        "image_advisory_purl_identity",
    ),
    ("an address that is a number", "image_advisory_package"),
];

/// Addresses that the reference's URL library took after changing them:
/// it read the kind without regard to case, dropped white space before
/// the address and line ends anywhere in it, and took an empty fragment
/// for none. An address is held here as it is written.
const LOOSE: [(&str, &str); 4] = [
    (
        "an address with an empty fragment",
        "refused image_advisory_purl_identity",
    ),
    (
        "an address with its kind in capitals",
        "refused image_advisory_purl_identity",
    ),
    (
        "an address with a space before it",
        "refused image_advisory_purl_identity",
    ),
    (
        "an address with a line end inside its name",
        "refused image_advisory_purl_identity",
    ),
];

/// Verdicts that the reference failed on in its interpreter's words.
const UNJUDGED: [(&str, &str); 4] = [
    (
        "another package at an address of OpenSSL whose host cannot be read",
        "image_advisory_rpm_identity",
    ),
    ("a package that is no record", "image_advisory_package"),
    ("a package without a name", "image_advisory_package"),
    ("no package and no policy", "tool_entry_missing"),
];

/// Verdicts over a package at an address that the reference could not
/// read, so that it raised: here the package names no package of OpenSSL
/// and is passed over, and the verdict is the one that the reference gave
/// over the other packages alone, which another recorded case holds.
const PASSED_OVER: [(&str, &str); 1] = [(
    "another package at an address whose host cannot be read, and a fixed one",
    "one fixed package",
)];

#[test]
fn every_recorded_policy_file_is_read_alike() {
    assert_eq!(tallied(policies(), &UNREAD, &[]), [7, 28, 10, 0]);
}

#[test]
fn every_recorded_package_is_looked_at_alike() {
    let otherwise: Vec<(&str, &str)> = WIDER.iter().chain(&UNREADABLE).copied().collect();
    assert_eq!(
        tallied(candidates(), &UNDESCRIBED, &otherwise),
        [39, 0, 6, 7]
    );
}

#[test]
fn every_recorded_package_of_openssl_is_identified_alike() {
    assert_eq!(
        tallied(identities(), &UNIDENTIFIED, &LOOSE),
        [32, 80, 19, 4]
    );
}

#[test]
fn every_recorded_verdict_is_the_same() {
    let all = verdicts();
    let mut rest = Vec::new();
    for (name, here, there) in &all {
        let Some((_, sibling)) = PASSED_OVER.iter().find(|(case, _)| case == name) else {
            rest.push((name.clone(), here.clone(), there.clone()));
            continue;
        };
        let theirs = all.iter().find(|(other, _, _)| other == sibling);
        let (_, _, theirs) = theirs.expect("the verdict over the other packages");
        assert!(
            there.starts_with("RAISED ") && theirs.starts_with("held "),
            "{name}"
        );
        assert_eq!(here, theirs, "{name}");
    }
    assert_eq!(rest.len() + PASSED_OVER.len(), all.len());
    assert_eq!(tallied(rest, &UNJUDGED, &[]), [44, 15, 4, 0]);
}
