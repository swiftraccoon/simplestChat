//! The review ledger's licence records, held to the retained evidence of
//! what they review: each needs its exact package and declaration, a
//! changed identity or declaration borrows no review, superseded scopes
//! stay historical, and no review renews itself past its expiry.

use super::{enrich::static_rpm, license_identity::spelled, licenses::license_verdict};
use crate::{
    Fault,
    document::Document,
    image::{policy, small_hex},
    policy::{Date, Exception, parse, read},
    spdx::{Allowed, permits},
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

/// The retained licence evidence `name`.
fn evidence(name: &str) -> Document {
    let path = repository().join("security/license-evidence").join(name);
    policy::report(&path, 64 << 20).expect(name)
}

/// The image policy.
fn image_policy() -> Document {
    policy::load_policy(&repository().join("security/image-policy.json")).expect("the policy")
}

/// The reviews of the ledger, current on `today`.
fn reviews_on(today: &str) -> crate::Result<Vec<Exception>> {
    let today = Date::parse(today).expect("a day");
    read(&repository().join("security/exceptions.json"), today, false)
}

/// The reviews of the ledger, current on the day they were reviewed against.
fn current() -> Vec<Exception> {
    reviews_on("2026-09-30").expect("the reviews")
}

/// What the licence verdict says of `packages` under `reviews`.
fn verdict(packages: &[Document], reviews: &[Exception]) -> Document {
    license_verdict(packages, &image_policy(), reviews).expect("a verdict")
}

/// Whether a verdict passed.
fn passed(verdict: &Document) -> bool {
    verdict.get("passed") == Some(&Document::Bool(true))
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

/// The allowed licences of the image policy, as the verdict reads them.
fn allowed() -> (BTreeSet<String>, Allowed) {
    let policy = image_policy();
    let listed: BTreeSet<String> = items(&policy, "allowedLicenses")
        .iter()
        .map(|item| item.text().expect("a licence").to_owned())
        .collect();
    let held = Allowed::new(listed.iter().map(String::as_str));
    (listed, held)
}

/// Whether `term` is an expression the policy allows.
fn permitted(term: &str, allowed: &Allowed) -> bool {
    permits(term, allowed).expect("an expression")
}

// ---- the Fedora static runtime

/// The retained real ARM observation, read apart from the ledger.
fn observation() -> Document {
    field(
        &evidence("fedora-libstdcxx-16.2.1-2.fc44.json"),
        "observation",
    )
    .clone()
}

/// The retained canonical x86_64 observation of the 16.2.1-3 refresh.
fn canonical_observation() -> Document {
    let evidence = evidence("fedora-gcc-16.2.1-3.fc44.json");
    field(&items(&evidence, "staticRuntimeInputs")[0], "observation").clone()
}

/// The static runtime package as the catalog would hold it, owned by the
/// rpm that `owner` names.
fn runtime(license: &Document, owner: [&str; 3]) -> Document {
    let mut package = Document::record([
        ("name", "libstdc++-static".into()),
        ("type", "binary".into()),
        (
            "licenses",
            Document::List(vec![Document::record([(
                "spdxExpression",
                license.clone(),
            )])]),
        ),
    ]);
    static_rpm(&mut package, owner).expect("an rpm");
    package
}

/// The observed canonical x86_64 owner and its declared aggregate.
fn canonical_package() -> Document {
    let observed = canonical_observation();
    let owner: Vec<&str> = text(&observed, "owner").split('\t').collect();
    runtime(field(&observed, "license"), [owner[0], owner[1], owner[2]])
}

/// The real declared aggregate with the given machine and epoch.
fn package(architecture: &str, epoch: &str) -> Document {
    let observed = observation();
    let owner: Vec<&str> = text(&observed, "owner").split('\t').collect();
    let identity = format!("{epoch}:16.2.1-2.fc44.{architecture}");
    runtime(field(&observed, "license"), [owner[0], &identity, owner[2]])
}

#[test]
fn the_observed_fedora_aggregate_is_retained_and_requires_explicit_review() {
    let selected = package("aarch64", "0");
    assert!(!passed(&verdict(std::slice::from_ref(&selected), &[])));
    let judged = verdict(std::slice::from_ref(&selected), &current());
    assert!(passed(&judged));
    let waived = items(&judged, "waived");
    assert_eq!(waived.len(), 1);
    let expression = format!("({})", text(&observation(), "license"));
    assert_eq!(text(&waived[0], "expression"), expression);
    let fingerprint = format!("license:{}", crate::digest(expression.as_bytes()));
    assert_eq!(text(&waived[0], "fingerprint"), fingerprint);
    let (listed, _) = allowed();
    assert!(!listed.contains("LicenseRef-Fedora-Public-Domain"));
}

#[test]
fn the_observed_architectures_keep_distinct_exact_reviews() {
    assert_eq!(text(&observation(), "platform"), "linux/arm64");
    assert_eq!(text(&canonical_observation(), "platform"), "linux/amd64");
    assert_eq!(
        field(&canonical_observation(), "license"),
        field(&observation(), "license")
    );
    let reviews = current();
    let scoped: Vec<Exception> = reviews
        .iter()
        .filter(|entry| entry.scanner == "image-license")
        .cloned()
        .collect();
    for selected in [package("aarch64", "0"), canonical_package()] {
        let purl = text(&selected, "purl");
        let matching: Vec<Exception> = scoped
            .iter()
            .filter(|entry| entry.scope == purl)
            .cloned()
            .collect();
        assert_eq!(matching.len(), 1, "{purl}");
        assert!(
            passed(&verdict(std::slice::from_ref(&selected), &matching)),
            "{purl}"
        );
    }
    let canonical = canonical_package();
    let purl = text(&canonical, "purl");
    assert!(
        purl.contains("@16.2.1-3.fc44") && purl.contains("arch=x86_64"),
        "{purl}"
    );
    let entry = scoped
        .iter()
        .find(|entry| entry.scope == purl)
        .expect("a review");
    assert!(entry.reachability.contains("canonical linux/amd64"));
    assert!(entry.reachability.contains("fedora-gcc-16.2.1-3.fc44.json"));
    // The superseded x86_64 16.2.1-2 scope and an unobserved ARM 16.2.1-3
    // stay unreviewed.
    let superseded = package("x86_64", "0");
    let arm = package("aarch64", "0");
    let unobserved = with(
        &arm,
        "purl",
        text(&arm, "purl").replace("16.2.1-2", "16.2.1-3").into(),
    );
    for selected in [superseded, unobserved] {
        let purl = text(&selected, "purl");
        assert!(!scoped.iter().any(|entry| entry.scope == purl), "{purl}");
        assert!(
            !passed(&verdict(std::slice::from_ref(&selected), &reviews)),
            "{purl}"
        );
    }
    assert_ne!(
        text(&package("aarch64", "0"), "purl"),
        text(&package("x86_64", "0"), "purl")
    );
}

#[test]
fn a_changed_identity_or_license_does_not_borrow_review() {
    let reviews = current();
    let selected = package("aarch64", "0");
    let scope = text(&selected, "purl").to_owned();
    let licensed = |expression: Option<&str>| {
        let records = expression.map_or_else(Vec::new, |expression| {
            vec![Document::record([("spdxExpression", expression.into())])]
        });
        with(&selected, "licenses", Document::List(records))
    };
    let aggregate = format!("{} AND MIT", text(&observation(), "license"));
    let changes = [
        with(
            &selected,
            "purl",
            scope
                .replace("libstdc%2B%2B-static", "different-runtime")
                .into(),
        ),
        with(
            &selected,
            "purl",
            scope.replace("@16.2.1-2.fc44", "@16.2.1-3.fc44").into(),
        ),
        with(
            &selected,
            "purl",
            scope
                .replace("gcc-16.2.1-2.fc44.src.rpm", "gcc-16.2.1-3.fc44.src.rpm")
                .into(),
        ),
        with(
            &selected,
            "purl",
            scope.replace("arch=aarch64", "arch=ppc64le").into(),
        ),
        licensed(Some("UNKNOWN")),
        licensed(Some("LicenseRef-Fedora-Public-Domain")),
        licensed(Some(&aggregate)),
        licensed(None),
    ];
    for changed in changes {
        assert!(
            !passed(&verdict(std::slice::from_ref(&changed), &reviews)),
            "{changed:?}"
        );
    }
    let nonzero = package("aarch64", "1");
    assert!(text(&nonzero, "purl").contains("epoch=1"));
    assert_eq!(field(field(&nonzero, "metadata"), "epoch").whole(), Some(1));
    assert!(!passed(&verdict(std::slice::from_ref(&nonzero), &reviews)));
}

#[test]
fn a_fedora_review_cannot_be_renewed_implicitly() {
    let path = repository().join("security/exceptions.json");
    let raw = crate::files::bounded(&path, 64 << 20, false).expect("the ledger");
    let ledger = crate::record::document(&raw).expect("a ledger");
    let scope = text(&package("aarch64", "0"), "purl").to_owned();
    let entries: Vec<&serde_json::Value> = ledger["exceptions"]
        .as_array()
        .expect("reviews")
        .iter()
        .filter(|item| item["scanner"] == "image-license" && item["scope"] == scope.as_str())
        .collect();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["expires"], "2026-11-29");
    let after = Date::parse("2026-11-30").expect("a day");
    assert_eq!(
        parse(entries[0], after).err(),
        Some(Fault("expired_security_exception"))
    );
}

// ---- the raw records of rpm licences

/// The current canonical raw records of the public key and of libtool.
fn raw_observations() -> Vec<Document> {
    let value = evidence("fedora-runtime-2026-09-30.json");
    let named = |list: &str, name: &str| {
        let found = items(&value, list)
            .into_iter()
            .find(|item| text(item, "name") == name);
        found.unwrap_or_else(|| panic!("no {name}"))
    };
    let mut public_key = named("existingReviewMatches", "gpg-pubkey");
    public_key.put("type", "rpm".into());
    public_key.put(
        "fingerprint",
        field(&public_key, "reviewFingerprint").clone(),
    );
    public_key.put(
        "rawLicenseRecordsSha256",
        field(&public_key, "rawRecordsSha256").clone(),
    );
    let libtool = named("packages", "libtool-ltdl");
    let declarations = field(&value, "declarations");
    let declaration = field(declarations, text(&libtool, "declaration"));
    let mut identity = field(&libtool, "canonical").clone();
    identity.put("name", "libtool-ltdl".into());
    identity.put("type", "rpm".into());
    identity.put("licenses", field(declaration, "records").clone());
    identity.put(
        "fingerprint",
        field(declaration, "reviewFingerprint").clone(),
    );
    identity.put(
        "rawLicenseRecordsSha256",
        field(declaration, "recordsSha256").clone(),
    );
    vec![public_key, identity]
}

#[test]
fn raw_reviews_bind_full_records_and_preserve_observations() {
    let reviews = current();
    for selected in raw_observations() {
        let name = text(&selected, "name");
        assert!(
            !passed(&verdict(std::slice::from_ref(&selected), &[])),
            "{name}"
        );
        let judged = verdict(std::slice::from_ref(&selected), &reviews);
        assert!(passed(&judged), "{name}");
        let finding = items(&judged, "waived")[0].clone();
        let raw = field(&selected, "licenses").clone();
        let mut normalized = raw.items().expect("records").to_vec();
        for record in &mut normalized {
            let mut locations = items(record, "locations");
            for location in &mut locations {
                location.put("layerID", "sha256:<image-layer>".into());
            }
            record.put("locations", Document::List(locations));
        }
        let spelled = spelled(&Document::List(normalized)).expect("one spelling");
        let expected = format!("license-raw-v2:{}", crate::digest(spelled.as_bytes()));
        assert_eq!(text(&finding, "fingerprint"), expected, "{name}");
        assert_eq!(
            text(&finding, "fingerprint"),
            text(&selected, "fingerprint"),
            "{name}"
        );
        assert_eq!(field(&finding, "rawLicenseRecords"), &raw, "{name}");
        let digest = field(&finding, "rawLicenseRecordsSha256");
        assert_eq!(
            digest,
            field(&selected, "rawLicenseRecordsSha256"),
            "{name}"
        );
        assert_eq!(text(&finding, "expression"), "UNKNOWN", "{name}");
    }
}

#[test]
fn changed_or_missing_raw_records_cannot_borrow_review() {
    let reviews = current();
    for observed in raw_observations() {
        let name = text(&observed, "name");
        let record = items(&observed, "licenses")[0].clone();
        let unreviewed = Document::record([
            ("value", "unreviewed".into()),
            ("spdxExpression", "".into()),
            ("type", "declared".into()),
        ]);
        let variants = [
            vec![],
            vec![Document::record([])],
            vec![with(&record, "value", "".into())],
            vec![with(&record, "value", "unreviewed".into())],
            vec![with(&record, "type", "concluded".into())],
            vec![with(
                &record,
                "spdxExpression",
                "LicenseRef-Unreviewed".into(),
            )],
            vec![with(&record, "locations", Document::List(vec![]))],
            vec![with(
                &record,
                "urls",
                Document::List(vec!["https://example.invalid/different".into()]),
            )],
            vec![record.clone(), unreviewed],
        ];
        for raw in variants {
            let changed = with(&observed, "licenses", Document::List(raw.clone()));
            assert!(!passed(&verdict(&[changed], &reviews)), "{name} {raw:?}");
        }
        let scope = format!("{}&epoch=1", text(&observed, "purl"));
        let changed = with(&observed, "purl", scope.into());
        assert!(!passed(&verdict(&[changed], &reviews)), "{name}");
    }
}

#[test]
fn a_recognized_license_does_not_hide_an_unparsed_record() {
    let selected = raw_observations()[0].clone();
    let known = Document::record([
        ("value", "MIT".into()),
        ("spdxExpression", "MIT".into()),
        ("type", "declared".into()),
    ]);
    let alone = with(&selected, "licenses", Document::List(vec![known.clone()]));
    assert!(passed(&verdict(&[alone], &[])));
    let records = items(&selected, "licenses");
    let mut first = vec![known.clone()];
    first.extend(records.iter().cloned());
    let mut last = records.clone();
    last.push(known.clone());
    for variant in [first, last, vec![known, Document::record([])]] {
        let value = with(&selected, "licenses", Document::List(variant.clone()));
        assert!(!passed(&verdict(&[value], &[])), "{variant:?}");
    }
}

#[test]
fn standard_additions_have_pinned_software_approval() {
    let evidence = evidence("fedora-rpm-review-2026-09-30.json");
    let records = items(&evidence, "standardTerms");
    assert_eq!(records.len(), 55);
    let (listed, held) = allowed();
    for record in records {
        let term = text(&record, "term");
        let status = field(&record, "fedoraStatus");
        assert_eq!(status, &Document::List(vec!["allowed".into()]), "{term}");
        assert!(small_hex(text(&record, "fedoraSha256"), 64), "{term}");
        let source = text(&record, "source");
        assert!(
            source.contains("ref=cbd8b74cd481a598668f05d1878fcd53a8ef78e4"),
            "{term}"
        );
        assert!(listed.contains(term), "{term}");
        assert!(permitted(term, &held), "{term}");
        let unreviewed = format!("{term} AND LicenseRef-Unreviewed");
        assert!(!permitted(&unreviewed, &held), "{term}");
    }
}

#[test]
fn documentation_custom_and_unknown_terms_remain_contextual() {
    let evidence = evidence("fedora-rpm-review-2026-09-30.json");
    let contextual = items(&evidence, "contextualTerms");
    assert_eq!(contextual.len(), 7);
    let (listed, held) = allowed();
    let mut forbidden: Vec<String> = contextual
        .iter()
        .map(|item| text(item, "term").to_owned())
        .collect();
    forbidden.extend(
        [
            "LicenseRef-Fedora-Public-Domain",
            "LicenseRef-Callaway-BSD",
            "LicenseRef-Not-Copyrightable",
            "UNKNOWN",
            "NOASSERTION",
            "GPL-3.0-or-later WITH Unreviewed-exception",
        ]
        .map(str::to_owned),
    );
    for term in forbidden {
        assert!(!listed.contains(&term), "{term}");
        assert!(!permitted(&term, &held), "{term}");
    }
}

// ---- the canonical runtime's licences

const RUNTIME: &str = "fedora-runtime-2026-09-30.json";
const GLIBC: &str = "fedora-glibc-2026-10-01.json";
const TZDATA: &str = "fedora-tzdata-2026-10-10.json";
const TZDATA_2026F: &str = "fedora-tzdata-2026f-1.fc44.json";
const VIM: &str = "fedora-vim-2026-11-11.json";
const VIM_BEFORE: &str = "fedora-vim-2026-10-19.json";
const GCC: &str = "fedora-gcc-16.2.1-3.fc44.json";
const GLIBC_PACKAGES: [&str; 3] = ["glibc", "glibc-common", "glibc-minimal-langpack"];
const TZDATA_PACKAGES: [&str; 1] = ["tzdata"];
const VIM_PACKAGES: [&str; 2] = ["vim-data", "vim-minimal"];
const GCC_PACKAGES: [&str; 3] = ["libgcc", "libgomp", "libstdc++"];
const REFRESH_RUN: i128 = 42_453_211_328;

/// The unchanged canonical packages of `value`, with their full ordered
/// licence records.
fn packages(value: &Document) -> Vec<Document> {
    let declarations = field(value, "declarations");
    let found = items(value, "packages");
    found
        .iter()
        .map(|entry| {
            let mut package = field(entry, "canonical").clone();
            package.put("name", field(entry, "name").clone());
            package.put("type", "rpm".into());
            let declaration = field(declarations, text(entry, "declaration"));
            package.put("licenses", field(declaration, "records").clone());
            package
        })
        .collect()
}

/// The packages of `value` named one of `names`.
fn named(value: &Document, names: &[&str]) -> Vec<Document> {
    let kept = packages(value).into_iter();
    kept.filter(|package| names.contains(&text(package, "name")))
        .collect()
}

/// The names of `packages`.
fn names(packages: &[Document]) -> BTreeSet<String> {
    packages
        .iter()
        .map(|package| text(package, "name").to_owned())
        .collect()
}

/// The scopes of `packages`.
fn scopes(packages: &[Document]) -> BTreeSet<String> {
    packages
        .iter()
        .map(|package| text(package, "purl").to_owned())
        .collect()
}

/// The texts of the list `name` of `value`, as a set.
fn listed(value: &Document, name: &str) -> BTreeSet<String> {
    let found = items(value, name);
    found
        .iter()
        .map(|item| item.text().expect("a text").to_owned())
        .collect()
}

/// The runtime's packages with the superseded observations replaced by the
/// newer evidence retained apart.
fn current_packages() -> Vec<Document> {
    let superseded: Vec<&str> = GLIBC_PACKAGES
        .iter()
        .chain(&TZDATA_PACKAGES)
        .chain(&VIM_PACKAGES)
        .chain(&GCC_PACKAGES)
        .copied()
        .collect();
    let mut kept: Vec<Document> = packages(&evidence(RUNTIME))
        .into_iter()
        .filter(|package| !superseded.contains(&text(package, "name")))
        .collect();
    for name in [GLIBC, TZDATA_2026F, VIM, GCC] {
        kept.extend(packages(&evidence(name)));
    }
    kept
}

/// The reviews whose scope is `scope`.
fn scoped<'a>(reviews: &'a [Exception], scope: &str) -> Vec<&'a Exception> {
    reviews
        .iter()
        .filter(|entry| entry.scope == scope)
        .collect()
}

/// The declaration `key` of `value` and the notice set `notices`.
fn declared(value: &Document, key: &str, notices: &str) -> (Document, Document) {
    let declaration = field(field(value, "declarations"), key).clone();
    let notice = field(field(value, "noticeSets"), notices).clone();
    (declaration, notice)
}

/// An older and a newer declaration and notice set review the same
/// records and files under another owner.
fn carried(old: &(Document, Document), new: &(Document, Document)) {
    assert_eq!(
        field(&old.0, "reviewFingerprint"),
        field(&new.0, "reviewFingerprint")
    );
    assert_eq!(field(&old.1, "files"), field(&new.1, "files"));
    assert_ne!(field(&old.1, "ownerPurl"), field(&new.1, "ownerPurl"));
}

#[test]
fn every_runtime_record_needs_its_exact_current_review() {
    let observed = current_packages();
    let reviews = current();
    assert_eq!(observed.len(), 26);
    assert_eq!(items(&verdict(&observed, &[]), "blocked").len(), 26);
    let judged = verdict(&observed, &reviews);
    assert!(passed(&judged));
    assert_eq!(items(&judged, "waived").len(), 26);
    for package in &observed {
        let purl = text(package, "purl");
        let others: Vec<Exception> = reviews
            .iter()
            .filter(|entry| entry.scope != purl)
            .cloned()
            .collect();
        assert!(
            !passed(&verdict(std::slice::from_ref(package), &others)),
            "{purl}"
        );
    }
}

#[test]
fn a_changed_runtime_scope_or_declaration_cannot_borrow_review() {
    let reviews = current();
    for package in current_packages() {
        let scope = text(&package, "purl");
        let mut extended = items(&package, "licenses");
        extended.push(Document::record([
            ("spdxExpression", "LicenseRef-Unreviewed".into()),
            ("value", "unreviewed".into()),
        ]));
        for changed in [
            with(&package, "purl", format!("{scope}&unreviewed=1").into()),
            with(&package, "licenses", Document::List(vec![])),
            with(&package, "licenses", Document::List(extended)),
        ] {
            assert!(!passed(&verdict(&[changed], &reviews)), "{scope}");
        }
    }
    let libtool = current_packages()
        .into_iter()
        .find(|package| text(package, "name") == "libtool-ltdl");
    let libtool = libtool.expect("libtool");
    let old = text(&libtool, "purl").replace("arch=x86_64", "arch=aarch64");
    assert!(!passed(&verdict(
        &[with(&libtool, "purl", old.into())],
        &reviews
    )));
}

#[test]
fn superseded_glibc_scopes_are_historical_evidence_only() {
    let runtime = evidence(RUNTIME);
    let updated = evidence(GLIBC);
    let old = named(&runtime, &GLIBC_PACKAGES);
    let new = packages(&updated);
    assert_eq!(names(&new), GLIBC_PACKAGES.map(str::to_owned).into());
    assert_eq!(new.len(), 3);
    assert_eq!(listed(&updated, "supersedes"), scopes(&old));
    let reviews = current();
    assert_eq!(items(&verdict(&old, &reviews), "blocked").len(), 3);
    assert!(passed(&verdict(&new, &reviews)));
    carried(
        &declared(&runtime, "D03", "N02"),
        &declared(&updated, "D03", "N02"),
    );
}

#[test]
fn the_superseded_tzdata_scope_is_historical_evidence_only() {
    let runtime = evidence(RUNTIME);
    let updated = evidence(TZDATA);
    let old = named(&runtime, &TZDATA_PACKAGES);
    let new = packages(&updated);
    assert_eq!(names(&new), TZDATA_PACKAGES.map(str::to_owned).into());
    assert_eq!(new.len(), 1);
    assert_eq!(listed(&updated, "supersedes"), scopes(&old));
    let reviews = current();
    for historical in [&old, &new] {
        assert_eq!(items(&verdict(historical, &reviews), "blocked").len(), 1);
    }
    let entries = items(&runtime, "packages");
    let tzdata = entries
        .iter()
        .find(|item| text(item, "name") == "tzdata")
        .expect("tzdata");
    let before = declared(
        &runtime,
        text(tzdata, "declaration"),
        text(tzdata, "noticeSet"),
    );
    carried(&before, &declared(&updated, "D01", "N01"));
}

#[test]
fn the_tzdata_2026f_refresh_supersedes_the_2026e_scope() {
    let updated = evidence(TZDATA_2026F);
    let previous = evidence(TZDATA);
    let old = packages(&previous);
    let new = packages(&updated);
    assert_eq!(names(&new), TZDATA_PACKAGES.map(str::to_owned).into());
    assert_eq!(new.len(), 1);
    assert_eq!(listed(&updated, "supersedes"), scopes(&old));
    let reviews = current();
    assert_eq!(items(&verdict(&old, &reviews), "blocked").len(), 1);
    assert!(passed(&verdict(&new, &reviews)));
    let matching = scoped(&reviews, text(&new[0], "purl"));
    assert_eq!(matching.len(), 1);
    assert!(
        matching[0]
            .reachability
            .contains("fedora-tzdata-2026f-1.fc44.json")
    );
    carried(
        &declared(&previous, "D01", "N01"),
        &declared(&updated, "D01", "N01"),
    );
    let canonical = field(&updated, "canonical");
    assert_eq!(field(canonical, "ciRunId").whole(), Some(REFRESH_RUN));
    assert_eq!(field(canonical, "imageCheckPassed"), &Document::Bool(false));
    let before = field(canonical, "licenseCheckPassedBeforeReview");
    assert_eq!(before, &Document::Bool(false));
    assert!(listed(canonical, "licenseBlockedBeforeReview").contains(text(&new[0], "purl")));
}

#[test]
fn superseded_gcc_scopes_are_historical_evidence_only() {
    let runtime = evidence(RUNTIME);
    let updated = evidence(GCC);
    let old = named(&runtime, &GCC_PACKAGES);
    let new = packages(&updated);
    assert_eq!(names(&new), GCC_PACKAGES.map(str::to_owned).into());
    assert_eq!(new.len(), 3);
    let superseded = listed(&updated, "supersedes");
    assert!(scopes(&old).is_subset(&superseded));
    assert_eq!(superseded.len(), 4);
    let reviews = current();
    assert!(
        reviews
            .iter()
            .all(|entry| !superseded.contains(&entry.scope))
    );
    assert_eq!(items(&verdict(&old, &reviews), "blocked").len(), 3);
    assert!(passed(&verdict(&new, &reviews)));
    for package in &new {
        let matching = scoped(&reviews, text(package, "purl"));
        assert_eq!(matching.len(), 1, "{}", text(package, "name"));
        assert!(
            matching[0]
                .reachability
                .contains("fedora-gcc-16.2.1-3.fc44.json")
        );
    }
    carried(
        &declared(&runtime, "D09", "N08"),
        &declared(&updated, "D01", "N01"),
    );
    let canonical = field(&updated, "canonical");
    assert_eq!(field(canonical, "ciRunId").whole(), Some(REFRESH_RUN));
    assert_eq!(field(canonical, "imageCheckPassed"), &Document::Bool(false));
    let mut blocked = scopes(&new);
    let input = &items(&updated, "staticRuntimeInputs")[0];
    blocked.insert(text(field(input, "canonical"), "purl").to_owned());
    let verification = field(&updated, "verification");
    assert_eq!(listed(verification, "beforeReviewBlockedScopes"), blocked);
}

#[test]
fn superseded_vim_scopes_are_historical_evidence_only() {
    let runtime = evidence(RUNTIME);
    let updated = evidence(VIM);
    let old = packages(&evidence(VIM_BEFORE));
    let new = packages(&updated);
    assert_eq!(names(&new), VIM_PACKAGES.map(str::to_owned).into());
    assert_eq!(new.len(), 2);
    assert_eq!(listed(&updated, "supersedes"), scopes(&old));
    let reviews = current();
    assert_eq!(items(&verdict(&old, &reviews), "blocked").len(), 2);
    assert!(passed(&verdict(&new, &reviews)));
    let entries = items(&runtime, "packages");
    let after = declared(&updated, "D01", "N01");
    for name in VIM_PACKAGES {
        let entry = entries
            .iter()
            .find(|item| text(item, "name") == name)
            .expect(name);
        let before = declared(
            &runtime,
            text(entry, "declaration"),
            text(entry, "noticeSet"),
        );
        carried(&before, &after);
    }
    let canonical = field(&updated, "canonical");
    for name in [
        "imageCheckPassed",
        "licenseCheckPassedBeforeReview",
        "secretCheckPassedBeforeReview",
    ] {
        assert_eq!(field(canonical, name), &Document::Bool(false), "{name}");
    }
    assert_eq!(field(canonical, "ciRunId").whole(), Some(42_952_247_902));
    assert_eq!(
        listed(canonical, "licenseBlockedBeforeReview"),
        scopes(&new)
    );
}

#[test]
fn declaration_provenance_and_shared_notices_are_retained() {
    let value = evidence(RUNTIME);
    let declarations = field(&value, "declarations")
        .fields()
        .expect("declarations")
        .to_vec();
    // A later package of the same name stands for it, as in a map by name.
    let observed: std::collections::BTreeMap<String, Document> = packages(&value)
        .into_iter()
        .map(|package| (text(&package, "name").to_owned(), package))
        .collect();
    assert_eq!(declarations.len(), 18);
    for (key, declaration) in &declarations {
        let records = spelled(field(declaration, "records")).expect("one spelling");
        assert_eq!(
            crate::digest(records.as_bytes()),
            text(declaration, "recordsSha256"),
            "{key}"
        );
    }
    let notice_sets = field(&value, "noticeSets")
        .fields()
        .expect("notice sets")
        .to_vec();
    assert_eq!(notice_sets.len(), 18);
    let mut total = 0;
    for (key, notice_set) in &notice_sets {
        let owner = &observed[text(notice_set, "ownerPackage")];
        assert_eq!(
            field(notice_set, "ownerPurl"),
            field(owner, "purl"),
            "{key}"
        );
        assert_eq!(
            field(notice_set, "sourceRpm"),
            field(owner, "sourceRpm"),
            "{key}"
        );
        for notice in items(notice_set, "files") {
            assert_ne!(
                notice.get("matchesRpmDigest"),
                Some(&Document::Bool(false)),
                "{key}"
            );
            assert!(small_hex(text(&notice, "sha256"), 64), "{key}");
            total += 1;
        }
    }
    assert_eq!(total, 46);
}

#[test]
fn the_artifact_binding_preserves_failed_image_status_and_payload_evidence() {
    let value = evidence(RUNTIME);
    let canonical = field(&value, "canonical");
    assert_eq!(text(canonical, "platform"), "linux/amd64");
    assert_eq!(field(canonical, "imageCheckPassed"), &Document::Bool(false));
    assert_eq!(
        text(canonical, "imageCheckError"),
        "image_runtime_rpm_owner"
    );
    assert_eq!(field(canonical, "noticeCheckPassed"), &Document::Bool(true));
    assert_eq!(field(canonical, "ciRunId").whole(), Some(36_738_045_459));
    assert_eq!(field(canonical, "artifactId").whole(), Some(11_109_374_252));
    let selected = field(canonical, "selectedImage");
    let image = format!("sha256:{}", text(selected, "archiveConfigSha256"));
    assert_eq!(text(canonical, "imageId"), image);
    for key in ["outcomeSha256", "noticeSha256", "spdxSha256", "syftSha256"] {
        assert!(small_hex(text(canonical, key), 64), "{key}");
    }
    let entries = items(&value, "packages");
    let rootfiles = entries
        .iter()
        .find(|item| text(item, "name") == "rootfiles")
        .expect("rootfiles");
    let payloads = items(rootfiles, "payloadExpectations");
    assert_eq!(payloads.len(), 6);
    for payload in payloads {
        assert!(small_hex(text(&payload, "sha256"), 64));
        assert!(text(&payload, "spdxFileId").starts_with("SPDXRef-File-"));
    }
}

#[test]
fn runtime_expiry_and_contextual_terms_remain_restrictive() {
    let reviews = current();
    let observed = scopes(&current_packages());
    let selected: Vec<&Exception> = reviews
        .iter()
        .filter(|entry| observed.contains(&entry.scope))
        .collect();
    assert_eq!(selected.len(), 26);
    let expiry = Date::parse("2026-11-29").expect("a day");
    assert!(selected.iter().all(|entry| entry.expires == expiry));
    assert_eq!(
        reviews_on("2026-11-30").err(),
        Some(Fault("expired_security_exception"))
    );
    let (listed, _) = allowed();
    for term in [
        "LicenseRef-Fedora-Public-Domain",
        "LicenseRef-Callaway-BSD",
        "UNKNOWN",
    ] {
        assert!(!listed.contains(term), "{term}");
    }
}
