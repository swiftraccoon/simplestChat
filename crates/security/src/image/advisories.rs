//! A reviewed upstream advisory, applied to the image's own packages.
//!
//! The vulnerability scanner blocks what its database maps to a package.
//! One advisory of OpenSSL is held here beside it, by a reviewed file that
//! names the advisory, its two upstream sources by their digests and the
//! series it was reviewed for. Every rpm of the image that looks like one
//! of OpenSSL's by its name, by its source package or by its address must
//! prove its identity in all three, and is blocked while its upstream
//! version is below the fixed one, whatever its release says. A version
//! of another series is not judged at all: it is refused until the
//! advisory is reviewed for it.

use super::notices::{parted, qualified};
use crate::{Fault, Result, digest, document::Document, files, require};
use std::path::Path;

/// The reviewed file, from the repository's root.
pub const POLICY_PATH: &str = "security/advisories/openssl-cve-2026-84782.json";
const MAX_EPOCH: i128 = 4_294_967_295;
/// The most characters of an upstream version.
const MAX_VERSION: usize = 32;
/// The first patch of the series that is not affected.
const FIXED_PATCH: u128 = 9;
/// The reviewed file is not what was reviewed.
const POLICY: Fault = Fault("image_advisory_policy");
/// A package that lacks a field or has one of another kind.
const PACKAGE: Fault = Fault("image_advisory_package");

/// The bounds of a verdict.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// The most bytes of the reviewed file.
    pub policy: u64,
    /// The most packages of the inventory.
    pub packages: usize,
}

impl Limits {
    /// The reviewed bounds.
    pub const REVIEWED: Self = Self {
        policy: 16384,
        packages: 20_000,
    };
}

/// The reviewed file at `path` and the SHA-256 of its bytes. Everything
/// in it but the words of the assessment is held to what was reviewed.
pub fn reviewed_policy(path: &Path, limits: &Limits) -> Result<(Document, String)> {
    let body = files::bounded(path, limits.policy, false)?;
    let value = Document::read(&body).map_err(|_| POLICY)?;
    let source = |url: &str, sha256: &str| {
        Document::record([("url", url.into()), ("sha256", sha256.into())])
    };
    let reviewed = Document::record([
        ("schemaVersion", 1_usize.into()),
        ("id", "CVE-2026-84782".into()),
        ("severity", "High".into()),
        ("reviewedOn", "2026-09-30".into()),
        (
            "scope",
            Document::record([
                ("distribution", "fedora-44".into()),
                ("sourcePackage", "openssl".into()),
                ("series", "3.5".into()),
                ("affectedFrom", "3.5.0".into()),
                ("fixedVersion", "3.5.9".into()),
            ]),
        ),
        (
            "sources",
            Document::List(vec![
                source(
                    "https://openssl-library.org/news/secjson/cve-2026-84782.json",
                    "281dcd4742dedb86b29381be85d1e7cfaa0a5af0583fadb0a7dec6e7c903f1ad",
                ),
                source(
                    "https://openssl-library.org/news/secadv/20260929.txt",
                    "2a6766a213b2dac48b4666aa54c39dbe3af96f73444081a4a25c1e3dc496aeb9",
                ),
            ]),
        ),
    ]);
    let fields = value.fields().ok_or(POLICY)?;
    let assessed = value.get("assessment").and_then(Document::text);
    let assessed = assessed.is_some_and(|assessment| !assessment.is_empty());
    let rest = fields.iter().filter(|(name, _)| name != "assessment");
    let rest = Document::Record(rest.cloned().collect());
    require(assessed && rest.sorted() == reviewed.sorted(), POLICY.0)?;
    Ok((value, digest(&body)))
}

/// Whether a package must prove that it is one of OpenSSL's, or that it
/// is none: an rpm that is named so, built from such a source, or whose
/// address names such a package anywhere, tabs and line ends aside.
fn candidate(package: &Document) -> Result<bool> {
    require(package.fields().is_some(), PACKAGE.0)?;
    if package.get("type").and_then(Document::text) != Some("rpm") {
        return Ok(false);
    }
    let name = words(package, "name")?;
    let optional = |from: Option<&Document>, name: &str| match from.and_then(|from| from.get(name))
    {
        Some(value) => value.text().map(str::to_owned).ok_or(PACKAGE),
        None => Ok(String::new()),
    };
    let metadata = package.get("metadata");
    require(
        metadata.is_none_or(|metadata| metadata.fields().is_some()),
        PACKAGE.0,
    )?;
    let source = optional(metadata, "sourceRpm")?;
    let address = optional(Some(package), "purl")?;
    let address = address.replace(['\t', '\r', '\n'], "");
    Ok(name == "openssl"
        || name.starts_with("openssl-")
        || source.starts_with("openssl-")
        || address.contains("rpm/fedora/openssl@")
        || address.contains("rpm/fedora/openssl-"))
}

/// The text field `name` of a package or of its record.
fn words<'a>(from: &'a Document, name: &str) -> Result<&'a str> {
    from.get(name).and_then(Document::text).ok_or(PACKAGE)
}

/// A small letter or a digit.
fn small(letter: char) -> bool {
    letter.is_ascii_lowercase() || letter.is_ascii_digit()
}

/// Whether `text` is one of `first` and then at most `more` of `rest`.
fn spelled(text: &str, first: fn(char) -> bool, rest: fn(char) -> bool, more: usize) -> bool {
    let mut letters = text.chars();
    letters.next().is_some_and(first) && letters.clone().count() <= more && letters.all(rest)
}

/// The address of a package of OpenSSL and the patch of its upstream
/// version, once its name, its record of the package database, its source
/// package and its address agree with each other.
fn identity(package: &Document) -> Result<(String, u128)> {
    let field = |from: &Document, name: &str| from.get(name).cloned().ok_or(PACKAGE);
    let metadata = field(package, "metadata")?;
    let name = words(package, "name")?;
    let (version, release) = (words(&metadata, "version")?, words(&metadata, "release")?);
    let (epoch, architecture) = (
        field(&metadata, "epoch")?,
        field(&metadata, "architecture")?,
    );
    let source = words(&metadata, "sourceRpm")?;
    let entry = field(package, "metadataType")?.text() == Some("rpm-db-entry");
    // The record's name is read only of an entry of the database.
    let named = entry && field(&metadata, "name")?.text() == Some(name);
    let suffix = |suffix: &str| {
        spelled(
            suffix,
            small,
            |letter| small(letter) || "+-".contains(letter),
            63,
        )
    };
    let known = name == "openssl" || name.strip_prefix("openssl-").is_some_and(suffix);
    let numbers: Vec<&str> = version.split('.').collect();
    let digits =
        |number: &&str| !number.is_empty() && number.bytes().all(|digit| digit.is_ascii_digit());
    let versioned =
        numbers.len() == 3 && numbers.iter().all(digits) && version.len() <= MAX_VERSION;
    let released = spelled(
        release,
        |letter| letter.is_ascii_alphanumeric(),
        |letter| letter.is_ascii_alphanumeric() || "._+~^".contains(letter),
        127,
    );
    let number = epoch
        .whole()
        .filter(|epoch| (0..=MAX_EPOCH).contains(epoch));
    let machine = matches!(architecture.text(), Some("aarch64" | "x86_64" | "noarch"));
    let built = source == format!("openssl-{version}-{release}.src.rpm");
    let whole = named && known && versioned && released && machine && built;
    let (Some(number), true) = (number, whole) else {
        return Err(Fault("image_advisory_rpm_identity"));
    };
    let version_release = format!("{version}-{release}");
    let said = if number == 0 {
        version_release.clone()
    } else {
        format!("{number}:{version_release}")
    };
    let stated = field(package, "version")?.text() == Some(&said);
    require(stated, "image_advisory_rpm_version")?;
    let scope = words(package, "purl")?;
    let mut expected = vec![
        ("arch", architecture.text().unwrap_or_default().to_owned()),
        ("distro", "fedora-44".to_owned()),
        ("upstream", source.to_owned()),
    ];
    if number != 0 {
        expected.push(("epoch", number.to_string()));
    }
    let path = format!("rpm/fedora/{name}@{version_release}");
    // The address is not read through escapes here: it names the package
    // letter for letter.
    let addressed = parted(scope)
        .is_some_and(|(location, pairs)| location == path && qualified(&pairs, &expected));
    require(addressed, "image_advisory_purl_identity")?;
    // The patch is spelled the one way a number is: no review covers
    // another spelling of it, or another series.
    let patch = numbers[2];
    let plain = patch == "0" || !patch.starts_with('0');
    let reviewed = numbers[0] == "3" && numbers[1] == "5" && plain;
    let patch = patch.parse().ok().filter(|_| reviewed);
    let patch = patch.ok_or(Fault("image_advisory_series_unreviewed"))?;
    Ok((scope.to_owned(), patch))
}

/// What the reviewed advisory at `path` says of the `packages` of the
/// image: every package of OpenSSL with whether it is affected, and a
/// blocking finding for each that is.
pub fn verdict(packages: &[Document], path: &Path, limits: &Limits) -> Result<Document> {
    let within = packages.len() <= limits.packages;
    require(within, "image_advisory_inventory_bound")?;
    let (policy, policy_digest) = reviewed_policy(path, limits)?;
    let of_policy = |name: &str| policy.get(name).cloned().ok_or(POLICY);
    let (mut assessed, mut blocked) = (Vec::new(), Vec::new());
    for package in packages {
        if !candidate(package)? {
            continue;
        }
        let (scope, patch) = identity(package)?;
        let affected = patch < FIXED_PATCH;
        assessed.push(Document::record([
            ("scope", scope.as_str().into()),
            ("affected", affected.into()),
        ]));
        if affected {
            blocked.push(Document::record([
                ("id", of_policy("id")?),
                ("scope", scope.into()),
                ("severity", of_policy("severity")?),
                ("source", "reviewed-openssl-advisory".into()),
                ("upstreamFixedVersion", "3.5.9".into()),
            ]));
        }
    }
    Ok(Document::record([
        ("passed", blocked.is_empty().into()),
        ("policySha256", policy_digest.into()),
        ("sources", of_policy("sources")?),
        ("scope", of_policy("scope")?),
        ("assessed", Document::List(assessed)),
        ("blocked", Document::List(blocked)),
    ]))
}

#[cfg(test)]
#[path = "advisories_tests.rs"]
mod tests;
