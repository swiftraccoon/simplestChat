//! The properties the reference's own tests hold triage to that a replay
//! of single cases does not show alone: one identity for a SARIF result and
//! for its alert, every changed part of a finding needing its own review,
//! writes paced apart within the operation's deadline, and the checkout's
//! own reviews bound exactly to their findings and policy.

use super::{
    LANGUAGES, Ledger, WRITE_INTERVAL, digest,
    findings::verdict,
    github::{Clock, Github, Reach, Transport},
    plan::{Checkout, api_finding, false_positive},
    sarif::sarif_findings,
    tests::{Served, Virtual, exceptions},
};
use crate::{
    Fault, Result,
    codeql::{
        tests::{items, planted, recording, text},
        tools::AUTOMATED_LANGUAGES,
    },
    document::Document,
    policy::{self, Date, permitted},
    scratch::Scratch,
};
use std::{cell::RefCell, ffi::OsString, fs, path::Path};

const REVISION: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const REFERENCE: &str = "refs/heads/main";

/// The reference's test alert: one inert high finding.
fn alert() -> Document {
    let text = r#"{"number": 1, "state": "open",
        "tool": {"name": "CodeQL", "version": "2.27.1"},
        "rule": {"id": "py/fixture", "security_severity_level": "high", "tags": ["security"]},
        "most_recent_instance": {"ref": "refs/heads/main",
            "commit_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "state": "open",
            "category": "/language:python/security",
            "location": {"path": "fixture.py", "start_line": 1, "end_line": 1,
                "start_column": 1, "end_column": 5},
            "message": {"text": "Test source value."}}}"#;
    Document::read(text.as_bytes()).expect("an alert")
}

/// The reference's test report: the same finding in SARIF, its rule owned
/// by an extension, its message carrying a numbered related location.
fn sarif(region: &str, uri: &str) -> Document {
    let text = format!(
        r#"{{"version": "2.1.0", "runs": [{{
            "tool": {{"driver": {{"name": "CodeQL", "semanticVersion": "2.27.1"}},
                "extensions": [{{"name": "codeql/python-queries", "rules": [{{"id": "py/fixture",
                    "properties": {{"security-severity": "7.5", "tags": ["security"]}}}}]}}]}},
            "invocations": [{{"executionSuccessful": true}}],
            "results": [{{"ruleId": "py/fixture", "message": {{"text": "Test [source value](1)."}},
                "locations": [{{"physicalLocation": {{"artifactLocation": {{"uri": "{uri}"}},
                    "region": {region}}}}}]}}]}}]}}"#
    );
    Document::read(text.as_bytes()).expect("a report")
}

/// `value` with the field at `path` set to `set`.
fn with(value: &Document, path: &[&str], set: Document) -> Document {
    let Some((first, rest)) = path.split_first() else {
        return set;
    };
    let mut value = value.clone();
    let inner = value.get(first).cloned().unwrap_or(Document::Null);
    value.put(first, with(&inner, rest, set));
    value
}

/// Whether `hash` is a whole SHA-256 in lowercase hexadecimal.
fn sha256(hash: &str) -> bool {
    hash.len() == 64
        && hash
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

/// The review of exactly `finding`.
fn review(finding: &Document) -> Vec<policy::Exception> {
    let record = Document::record([
        ("scanner", "codeql".into()),
        ("fingerprint", text(finding, "fingerprint").into()),
        ("scope", text(finding, "scope").into()),
    ]);
    exceptions(&Document::List(vec![record]))
}

/// Whether the verdict on `findings` under `reviews` passed.
fn passed(findings: &[Document], reviews: &[policy::Exception]) -> bool {
    let judged = verdict(findings, reviews).expect("a verdict");
    judged.get("passed") == Some(&Document::Bool(true))
}

/// A checkout of `files`, each committed as it is.
fn checkout(files: &[(&str, &str)]) -> (Scratch, Document) {
    let scratch = Scratch::new("codeql-triage-properties");
    let tree = files.iter().map(|(name, body)| {
        (
            (*name).to_owned(),
            Document::record([("text", (*body).into())]),
        )
    });
    let tree = Document::Record(tree.collect());
    planted(&scratch.0, &tree);
    (scratch, tree)
}

/// The finding the alert `raw` comes to in the checkout `root`.
fn api(raw: &Document, root: &Path, git: &Served<'_>) -> Result<Document> {
    let mut checkout = Checkout {
        root,
        sources: None,
        git,
    };
    api_finding(raw, &mut checkout, REVISION, REFERENCE)
}

/// The one finding of the report `raw` in the checkout `root`.
fn local(raw: &Document, root: &Path, git: &Served<'_>, clock: &Virtual) -> Document {
    let found = sarif_findings(raw, root, REVISION, None, git, clock).expect("findings");
    found.into_iter().next().expect("a finding")
}

const FULL: &str = r#"{"startLine": 1, "endLine": 1, "startColumn": 1, "endColumn": 5}"#;

#[test]
fn a_sarif_result_and_its_alert_are_one_identity_and_omitted_defaults_keep_it() {
    let (scratch, tree) = checkout(&[("fixture.py", "fixture\n")]);
    let clock = Virtual::new(0.0);
    let git = Served::new(&scratch.0, REVISION, &tree, &Document::Null, &clock, 0.0);
    let found = api(&alert(), &scratch.0, &git).expect("the alert's finding");
    for region in [
        FULL,
        r#"{"startLine": 1, "startColumn": 1, "endColumn": 5}"#,
        r#"{"startLine": 1, "endLine": 1, "endColumn": 5}"#,
        r#"{"startLine": 1, "endColumn": 5}"#,
    ] {
        let local = local(&sarif(region, "fixture.py"), &scratch.0, &git, &clock);
        assert_eq!(
            local.get("fingerprint"),
            found.get("fingerprint"),
            "{region}"
        );
        assert_eq!(local.get("scope"), found.get("scope"), "{region}");
        assert!(!passed(std::slice::from_ref(&local), &[]), "{region}");
        assert!(passed(&[local], &review(&found)), "{region}");
    }
    // An explicit multiline range and start column keep their own identity.
    let region = r#"{"startLine": 1, "endLine": 2, "startColumn": 3, "endColumn": 5}"#;
    let local = local(&sarif(region, "fixture.py"), &scratch.0, &git, &clock);
    let moved = with(
        &alert(),
        &["most_recent_instance", "location", "end_line"],
        2_u64.into(),
    );
    let moved = with(
        &moved,
        &["most_recent_instance", "location", "start_column"],
        3_u64.into(),
    );
    let moved = api(&moved, &scratch.0, &git).expect("the moved finding");
    assert_eq!(local.get("fingerprint"), moved.get("fingerprint"));
    assert!(!passed(&[local], &review(&found)));
}

#[test]
fn every_changed_part_of_a_finding_needs_its_own_review() {
    let (scratch, tree) = checkout(&[("fixture.py", "fixture\n")]);
    let clock = Virtual::new(0.0);
    let git = Served::new(&scratch.0, REVISION, &tree, &Document::Null, &clock, 0.0);
    let original = api(&alert(), &scratch.0, &git).expect("the original");
    let reviewed = review(&original);
    assert!(passed(std::slice::from_ref(&original), &reviewed));
    let message = Document::record([("text", "Different path.".into())]);
    for changed in [
        with(&alert(), &["rule", "id"], "py/other".into()),
        with(&alert(), &["tool", "version"], "2.28.0".into()),
        with(
            &alert(),
            &["most_recent_instance", "location", "start_column"],
            2_u64.into(),
        ),
        with(&alert(), &["most_recent_instance", "message"], message),
    ] {
        let item = api(&changed, &scratch.0, &git).expect("a changed finding");
        assert_ne!(item.get("fingerprint"), original.get("fingerprint"));
        assert!(!passed(&[item], &reviewed));
    }
    // The same finding over committed source of other bytes.
    let (other, tree) = checkout(&[("fixture.py", "edited\n")]);
    let git = Served::new(&other.0, REVISION, &tree, &Document::Null, &clock, 0.0);
    let item = api(&alert(), &other.0, &git).expect("the finding of other source");
    assert_ne!(item.get("sourceSha256"), original.get("sourceSha256"));
    assert!(!passed(&[item], &reviewed));
}

#[test]
fn a_change_of_an_included_source_alone_needs_a_review_on_both_paths() {
    let files = [
        (
            "bin/main.rs",
            r#"mod account { include!("../clients/account.rs"); }"#,
        ),
        (
            "clients/account.rs",
            r#"include!("guard.rs"); fn refresh() {}"#,
        ),
        ("clients/guard.rs", "fn loopback_only() {}"),
    ];
    let clock = Virtual::new(0.0);
    let raw = sarif(FULL, "bin/main.rs");
    let path = ["most_recent_instance", "location", "path"];
    let moved = with(&alert(), &path, "bin/main.rs".into());
    let (scratch, tree) = checkout(&files);
    let git = Served::new(&scratch.0, REVISION, &tree, &Document::Null, &clock, 0.0);
    let original = local(&raw, &scratch.0, &git, &clock);
    let api_original = api(&moved, &scratch.0, &git).expect("the alert");
    assert_eq!(api_original.get("fingerprint"), original.get("fingerprint"));
    assert!(passed(std::slice::from_ref(&original), &review(&original)));
    let mut changed_files = files;
    changed_files[2].1 = "fn external_origins_allowed() {}";
    let (other, tree) = checkout(&changed_files);
    let git = Served::new(&other.0, REVISION, &tree, &Document::Null, &clock, 0.0);
    let changed = local(&raw, &other.0, &git, &clock);
    let api_changed = api(&moved, &other.0, &git).expect("the changed alert");
    assert_eq!(original.get("sourceSha256"), changed.get("sourceSha256"));
    assert_ne!(original.get("fingerprint"), changed.get("fingerprint"));
    assert_eq!(api_changed.get("fingerprint"), changed.get("fingerprint"));
    assert!(!passed(&[changed], &review(&original)));
}

#[test]
fn a_native_source_from_another_archive_is_other_evidence() {
    let recorded = recording(include_bytes!("recorded_triage.json"));
    let case = items(&recorded, "findings")
        .iter()
        .find(|case| text(case, "name") == "a native source")
        .expect("the recorded native finding");
    let first = case.get("result").and_then(|result| result.get("held"));
    let first = first.expect("a held finding");
    let body = crate::digest(b"int fixture_value = 1;\n");
    assert_eq!(text(first, "sourceSha256"), body);
    let mut altered = first.clone();
    altered.put("sourceArchiveSha256", "f".repeat(64).into());
    assert_ne!(digest(first), digest(&altered));
}

/// A `gh` that answers at once with `status`, takes `seconds` of the
/// clock's time, and notes when each call began.
struct Paced<'a> {
    clock: &'a Virtual,
    status: i32,
    seconds: f64,
    started: RefCell<Vec<f64>>,
}

impl Transport for Paced<'_> {
    fn exchange(
        &self,
        _: &Path,
        _: &[String],
        _: &[(OsString, OsString)],
        _: &[u8],
        _: f64,
        _: (u64, u64),
    ) -> Result<(i32, Vec<u8>)> {
        self.started.borrow_mut().push(self.clock.now());
        self.clock.advance(self.seconds);
        Ok((self.status, b"{}".to_vec()))
    }
}

/// A search path holding a `gh`, below `scratch`.
fn search(scratch: &Scratch) -> Vec<(OsString, OsString)> {
    let directory = scratch.0.join("bin");
    fs::create_dir_all(&directory).expect("a directory");
    let gh = directory.join("gh");
    fs::write(&gh, "#!/bin/sh\nexit 99\n").expect("a program");
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).expect("a mode");
    vec![("PATH".into(), directory.into_os_string())]
}

#[test]
fn ninety_seven_writes_are_paced_below_eighty_a_minute_within_the_deadline() {
    let scratch = Scratch::new("codeql-triage-pacing");
    let environment = search(&scratch);
    let clock = Virtual::new(0.0);
    let paced = Paced {
        clock: &clock,
        status: 0,
        seconds: 0.25,
        started: RefCell::new(Vec::new()),
    };
    let reach = Reach {
        clock: &clock,
        transport: &paced,
        environment: &environment,
    };
    let mut client = Github::new("owner/repository", true, reach).expect("a client");
    let reader = Github::new("owner/repository", false, reach).expect("a client");
    assert_eq!(client.deadline, 900.0);
    assert_eq!(reader.deadline, 300.0);
    let body = Document::record([("state", "dismissed".into())]);
    for number in 1..98 {
        let answer = client.request(&format!("alerts/{number}"), Some(&body));
        assert_eq!(answer, Ok(Document::Record(Vec::new())));
    }
    let started = paced.started.into_inner();
    assert_eq!(started.len(), 97);
    assert!(
        started
            .windows(2)
            .all(|pair| pair[1] - pair[0] >= WRITE_INTERVAL + 0.25)
    );
    for start in &started {
        let within = started
            .iter()
            .filter(|at| **at >= *start && **at < start + 60.0);
        assert!(within.count() < 80);
    }
    assert!(clock.sleeps().items().expect("waits").iter().all(|wait| {
        matches!(wait, Document::Number(seconds) if seconds.as_f64().is_some_and(|s| s > 0.0))
    }));
    assert!(clock.now() < 900.0);
}

#[test]
fn a_failed_write_is_not_retried_and_pacing_never_outlives_the_deadline() {
    let scratch = Scratch::new("codeql-triage-deadline");
    let environment = search(&scratch);
    let clock = Virtual::new(0.0);
    let failing = Paced {
        clock: &clock,
        status: 1,
        seconds: 0.0,
        started: RefCell::new(Vec::new()),
    };
    let reach = Reach {
        clock: &clock,
        transport: &failing,
        environment: &environment,
    };
    let mut client = Github::new("owner/repository", true, reach).expect("a client");
    let body = Document::record([("state", "dismissed".into())]);
    let answer = client.request("alerts/1", Some(&body));
    assert_eq!(answer, Err(Fault("codeql_api_failed")));
    assert_eq!(failing.started.borrow().len(), 1);
    assert_eq!(client.next_write, WRITE_INTERVAL);
    client.next_write = client.deadline;
    let answer = client.request("alerts/2", Some(&body));
    assert_eq!(answer, Err(Fault("codeql_api_deadline")));
    assert_eq!(failing.started.borrow().len(), 1);
    assert_eq!(clock.sleeps(), Document::List(Vec::new()));
}

#[test]
fn native_code_is_analyzed_locally_but_never_automated() {
    assert!(LANGUAGES.contains(&"c-cpp"));
    assert!(!AUTOMATED_LANGUAGES.contains(&"c-cpp"));
    assert!(
        AUTOMATED_LANGUAGES
            .iter()
            .all(|language| LANGUAGES.contains(language))
    );
    assert_eq!(super::expected().len(), 4);
}

/// The checkout this crate was built from.
fn repository() -> &'static Path {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .and_then(Path::parent)
        .expect("the checkout")
}

#[test]
fn every_reviewed_record_of_the_checkout_is_exact_and_fixes_stay_unwaived() {
    let root = repository();
    let ledger = Ledger::of(root);
    let raw = fs::read(&ledger.reviews[0]).expect("the review");
    let raw = Document::read(&raw).expect("a review");
    let entries = items(&raw, "alerts");
    let mut numbers: Vec<u64> = entries
        .iter()
        .map(|item| super::positive(item.get("number").expect("a number")).expect("a number"))
        .collect();
    numbers.sort_unstable();
    let mut expected: Vec<u64> = (1..=69).collect();
    expected.extend([102, 953]);
    assert_eq!(numbers, expected);
    let today = Date::new(2026, 9, 30).expect("a day");
    let reviews = policy::read(&ledger.policy, today, false).expect("the policy");
    let (mut pending, mut retired) = (Vec::new(), Vec::new());
    for item in entries {
        let decided = text(item, "disposition") == "false-positive";
        let fingerprint = text(item, "fingerprint");
        let held = permitted(&reviews, "codeql", fingerprint, text(item, "scope"));
        assert_eq!(held, decided, "{fingerprint}");
        assert!(sha256(text(item, "sourceSha256")), "{fingerprint}");
        let ours = false_positive(item, "swiftraccoon/simplestChat", &ledger);
        assert_eq!(ours, Ok(decided), "{fingerprint}");
        let theirs = false_positive(item, "another/repository", &ledger);
        assert_eq!(theirs, Ok(false), "{fingerprint}");
        let number = super::positive(item.get("number").expect("a number")).expect("one");
        match text(item, "disposition") {
            "fix-pending" => pending.push(number),
            "retired" => retired.push(number),
            _ => {}
        }
    }
    pending.sort_unstable();
    assert_eq!(pending, [5, 24, 25, 26]);
    // A retired observation stays as it was reviewed, and no exception may
    // permit its identity again.
    retired.sort_unstable();
    let mut retirements: Vec<u64> = vec![1, 4];
    retirements.extend((9..=23).chain(27..=32).chain(59..=65));
    assert_eq!(retired, retirements);
}

#[test]
fn every_native_review_record_of_the_checkout_binds_its_source_and_policy() {
    let root = repository();
    let ledger = Ledger::of(root);
    let raw = fs::read(&ledger.reviews[1]).expect("the native review");
    let raw = Document::read(&raw).expect("a review");
    let entries = items(&raw, "alerts");
    let mut numbers: Vec<u64> = entries
        .iter()
        .map(|item| super::positive(item.get("number").expect("a number")).expect("a number"))
        .collect();
    numbers.sort_unstable();
    assert_eq!(numbers, (70..102).collect::<Vec<u64>>());
    let revalidation = raw.get("manifestRevalidation").expect("a revalidation");
    let manifest = text(revalidation, "sourceManifestSha256");
    let previous = text(revalidation, "previousManifestSha256");
    let today = Date::parse(text(&raw, "reviewed")).expect("a day");
    let reviews = policy::read(&ledger.policy, today, true).expect("the policy");
    let fields = [
        "rule",
        "toolVersion",
        "path",
        "region",
        "messageSha256",
        "sourceSha256",
        "sourceArchiveSha256",
        "sourceManifestSha256",
        "sourceWrapSha256",
        "sourcePatchSha256",
        "sourceOverlaySha256",
        "sourceMember",
    ];
    let mut retired = Vec::new();
    for item in entries {
        let scope = text(item, "scope");
        let decided = text(item, "disposition") == "false-positive";
        assert_eq!(
            false_positive(item, "swiftraccoon/simplestChat", &ledger),
            Ok(decided)
        );
        assert_eq!(text(item, "sourceManifestSha256"), manifest);
        let identity = item.fields().expect("a record").iter();
        let identity = identity.filter(|(name, _)| fields.contains(&name.as_str()));
        let mut identity = Document::Record(identity.cloned().collect());
        let fingerprint = format!("codeql:{}", digest(&identity));
        assert_eq!(text(item, "fingerprint"), fingerprint);
        assert_eq!(permitted(&reviews, "codeql", &fingerprint, scope), decided);
        if text(item, "disposition") == "retired" {
            retired.push(super::positive(item.get("number").expect("a number")).expect("one"));
        }
        identity.put("sourceManifestSha256", previous.into());
        let stale = format!("codeql:{}", digest(&identity));
        assert!(!permitted(&reviews, "codeql", &stale, scope));
        for field in [
            "sourceSha256",
            "sourceArchiveSha256",
            "sourceManifestSha256",
            "sourceWrapSha256",
        ] {
            assert!(sha256(text(item, field)), "{field}");
        }
        assert!(!scope.contains('*'));
    }
    // Every observation binds a member of the 0.17.0 worker tree, which the
    // checkout no longer holds: each stays as it was reviewed, retired, and
    // no exception may permit its identity again.
    assert!(!root.join("vendor/mediasoup-sys-0.17.0").exists());
    retired.sort_unstable();
    assert_eq!(retired, (70..102).collect::<Vec<u64>>());
}
