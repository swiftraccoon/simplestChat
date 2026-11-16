//! What the image's packages say of their notices, held to the image.
//!
//! This collects evidence; it decides nothing about a licence and takes no
//! exception. For every package of the image's own package database it
//! proves that the inventory's address and the database's record name one
//! package, and for every licence or notice file that the package lists
//! it hashes the file that the image really has there, through the
//! image's own links and never out of the notices. No content of a file
//! enters the report.

use super::{
    license_identity::{measured, raw_license_identity},
    small_hex,
};
use crate::{Fault, Result, digest, document::Document, elf, hexadecimal, require, secrets};
use sha2::{Digest as _, Sha256};
use std::{
    collections::BTreeSet, fs::OpenOptions, io::Read, os::unix::fs::OpenOptionsExt, path::Path,
};

/// The bounds of a collection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// The most packages of the inventory.
    pub packages: usize,
    /// The most files that the packages may list.
    pub metadata_files: usize,
    /// The most notices.
    pub notices: usize,
    /// The most bytes of one notice.
    pub notice_bytes: u64,
    /// The most bytes of every notice together.
    pub total_bytes: u64,
    /// The most bytes of the report as it is written.
    pub report_bytes: usize,
    /// The most bytes of one package's licence records.
    pub license_bytes: usize,
    /// The most characters of a path.
    pub path: usize,
}

impl Limits {
    /// The reviewed bounds.
    pub const REVIEWED: Self = Self {
        packages: 20_000,
        metadata_files: 200_000,
        notices: 4096,
        notice_bytes: 1024 * 1024,
        total_bytes: 32 * 1024 * 1024,
        report_bytes: 16 * 1024 * 1024,
        license_bytes: 64 * 1024,
        path: 4096,
    };
}

/// What the collection is of: the image, its archive, its inventory, the
/// revision that built it and the platform it is for.
#[derive(Clone, Copy, Debug)]
pub struct Image<'a> {
    /// The image's identity, `sha256:` and a digest.
    pub image_id: &'a str,
    /// The SHA-256 of the saved image.
    pub archive_sha256: &'a str,
    /// The SHA-256 of the inventory.
    pub sbom_sha256: &'a str,
    /// The revision.
    pub revision: &'a str,
    /// `linux/amd64` or `linux/arm64`.
    pub platform: &'a str,
}

const MAX_EPOCH: i128 = 4_294_967_295;
/// Who finds the packages of the image's own database.
const CATALOGER: &str = "rpm-db-cataloger";
/// Who adds the packages that were only built from.
const NATIVE_CATALOGER: &str = "simplestchat-authenticated-native-inputs";
/// A package that lacks a field or has one of another kind.
const PACKAGE: Fault = Fault("rpm_notice_package");
/// A listed file that lacks a field or has one that cannot be.
const METADATA: Fault = Fault("rpm_notice_file_metadata");
const KIND: i128 = 0o170_000;

/// A letter as the reference's pattern reads it without regard to case.
fn folded(letter: char) -> char {
    match letter {
        '\u{17f}' => 's',
        '\u{130}' | '\u{131}' => 'i',
        other => other.to_ascii_lowercase(),
    }
}

/// Whether a file among the documentation has the name of a notice: one
/// of five words, alone or before a dot, a line or a dash and anything
/// on the same line.
fn notice_name(name: &str) -> bool {
    const WORDS: [&str; 5] = ["copying", "license", "licence", "copyright", "notice"];
    WORDS.iter().any(|word| {
        let mut letters = name.chars();
        let begins = word
            .chars()
            .all(|wanted| letters.next().map(folded) == Some(wanted));
        let rest = letters.as_str();
        let parted = rest.starts_with(['.', '_', '-']) && !rest.contains('\n');
        begins && (rest.is_empty() || parted)
    })
}

/// Whether the plain absolute path `value` is one of a notice: anything
/// among the licences, and what is named as a notice among the
/// documentation. A path that is not spelled the one way it can be is
/// refused.
pub fn notice_path(value: &str, limits: &Limits) -> Result<bool> {
    // Exactly two slashes may begin a path; a third is an empty step.
    let rest = value.strip_prefix("//").or_else(|| value.strip_prefix('/'));
    let stepped = rest.is_some_and(|rest| {
        rest.is_empty() || rest.split('/').all(|step| !matches!(step, "" | "." | ".."))
    });
    let sized = value.chars().count() <= limits.path;
    let plain = !value.contains(['\0', '\\']);
    require(stepped && sized && plain, "rpm_notice_path")?;
    let name = value.rsplit('/').next().unwrap_or_default();
    let documented = value.starts_with("/usr/share/doc/") && notice_name(name);
    Ok(value.starts_with("/usr/share/licenses/") || documented)
}

/// Text with every `%` and two hexadecimal digits as the byte they spell.
fn unquoted(text: &str) -> Option<String> {
    let mut bytes = Vec::with_capacity(text.len());
    let mut rest = text.as_bytes();
    while let Some((first, after)) = rest.split_first() {
        if *first != b'%' {
            bytes.push(*first);
            rest = after;
            continue;
        }
        let digits = after
            .get(..2)
            .and_then(|digits| std::str::from_utf8(digits).ok());
        let digits = digits.filter(|digits| digits.bytes().all(|digit| digit.is_ascii_hexdigit()));
        bytes.push(u8::from_str_radix(digits?, 16).ok()?);
        rest = &after[2..];
    }
    String::from_utf8(bytes).ok()
}

/// Whether the package address `purl` is `pkg:` and `path`, with exactly
/// the qualifiers `expected`, of which there is always one, in any order.
/// The address is held as it is written: a host, a fragment, a qualifier
/// twice or without a value make it another address.
fn addressed(purl: &str, path: &str, expected: &[(&str, String)]) -> bool {
    let Some(rest) = purl.strip_prefix("pkg:").filter(|_| !purl.contains('#')) else {
        return false;
    };
    let (location, query) = rest.split_once('?').unwrap_or((rest, ""));
    let mut pairs = Vec::new();
    for field in query.split('&') {
        let pair = field.split_once('=').and_then(|(name, value)| {
            let spaced = |text: &str| unquoted(&text.replace('+', " "));
            Some((spaced(name)?, spaced(value)?))
        });
        let Some(pair) = pair else { return false };
        pairs.push(pair);
    }
    // As many pairs as are expected, and each expected one among them:
    // none is there twice.
    let held = |(name, value): &(&str, String)| {
        pairs
            .iter()
            .any(|(held, said)| held == name && said == value)
    };
    unquoted(location).as_deref() == Some(path)
        && pairs.len() == expected.len()
        && expected.iter().all(held)
}

/// The text field `name` of a package or of its record.
fn text<'a>(from: &'a Document, name: &str) -> Result<&'a str> {
    from.get(name).and_then(Document::text).ok_or(PACKAGE)
}

/// The identity of one package of the image's database, once its address
/// and its record are seen to name the same package for `architecture`.
pub fn package_identity(package: &Document, architecture: &str) -> Result<Document> {
    let kind = package.get("metadataType").and_then(Document::text);
    require(kind == Some("rpm-db-entry"), "rpm_notice_metadata_type")?;
    let metadata = package.get("metadata").ok_or(PACKAGE)?;
    let name = text(package, "name")?;
    let (version, release) = (text(metadata, "version")?, text(metadata, "release")?);
    let (source, arch) = (
        text(metadata, "sourceRpm")?,
        text(metadata, "architecture")?,
    );
    let epoch = metadata.get("epoch").ok_or(PACKAGE)?;
    let number = match epoch {
        Document::Null => Some(0),
        other => other
            .whole()
            .filter(|epoch| (0..=MAX_EPOCH).contains(epoch)),
    };
    let number = number.ok_or(Fault("rpm_notice_epoch"))?;
    let named = metadata.get("name").and_then(Document::text) == Some(name);
    let whole = named && !name.is_empty() && !version.is_empty() && !release.is_empty();
    require(whole, "rpm_notice_identity")?;
    let version_release = format!("{version}-{release}");
    let said = if number == 0 {
        version_release.clone()
    } else {
        format!("{number}:{version_release}")
    };
    let versioned = package.get("version").ok_or(PACKAGE)?;
    require(versioned.text() == Some(&said), "rpm_notice_version")?;
    let purl = text(package, "purl")?;
    let mut expected = vec![("distro", "fedora-44".to_owned())];
    if name == "gpg-pubkey" {
        let bare = source.is_empty() && arch.is_empty() && number == 0;
        require(bare, "rpm_notice_key_metadata")?;
    } else {
        let built = (arch == architecture || arch == "noarch") && source.ends_with(".src.rpm");
        require(built, "rpm_notice_origin")?;
        expected.push(("arch", arch.to_owned()));
        expected.push(("upstream", source.to_owned()));
        if number != 0 {
            expected.push(("epoch", number.to_string()));
        }
    }
    let path = format!("rpm/fedora/{name}@{version_release}");
    require(addressed(purl, &path, &expected), "rpm_notice_purl")?;
    Ok(Document::record([
        ("name", name.into()),
        ("version", versioned.clone()),
        ("purl", purl.into()),
        ("sourceRpm", source.into()),
        ("architecture", arch.into()),
        ("epoch", epoch.clone()),
    ]))
}

/// What a package declares of its licence: the records whole and in their
/// order, their digest, and the fingerprint that a review of them names.
pub fn license_identity(package: &Document, limits: &Limits) -> Result<Document> {
    let records = package
        .get("licenses")
        .and_then(Document::items)
        .ok_or(PACKAGE)?;
    let size = measured(&Document::List(records.to_vec()))?;
    require(size <= limits.license_bytes, "rpm_notice_license_size")?;
    let raw = raw_license_identity(records)?;
    let mut expressions = Vec::new();
    for record in records {
        let expression = record
            .get("spdxExpression")
            .map_or(Some(""), Document::text);
        expressions.push(expression.ok_or(PACKAGE)?);
    }
    let stated = expressions
        .iter()
        .filter(|expression| !expression.is_empty());
    let stated: Vec<String> = stated.map(|expression| format!("({expression})")).collect();
    let expression = stated.join(" AND ");
    let complete = !expressions.is_empty() && stated.len() == expressions.len();
    let fingerprint = if complete {
        format!("license:{}", digest(expression.as_bytes()))
    } else {
        raw.fingerprint
    };
    let expression = if expression.is_empty() {
        "UNKNOWN"
    } else {
        &expression
    };
    Ok(Document::record([
        ("records", Document::List(records.to_vec())),
        ("recordsSha256", raw.records_sha256.into()),
        ("reviewFingerprint", fingerprint.into()),
        ("expression", expression.into()),
    ]))
}

/// Why the image's own links do not lead to a file.
fn stopped(stop: elf::Stop) -> Fault {
    const KNOWN: [&str; 5] = [
        "image_path_invalid",
        "image_path_escape",
        "image_symlink_limit",
        "image_file_kind",
        "image_path_empty",
    ];
    match stop {
        elf::Stop::Refused(refusal) => {
            let known = KNOWN.iter().find(|known| **known == refusal.reason);
            Fault(known.copied().unwrap_or("image_path_invalid"))
        }
        elf::Stop::Missing => Fault("rpm_notice_file_missing"),
        elf::Stop::Unreadable => Fault("rpm_notice_file_unread"),
    }
}

/// One notice as the image has it: what the package lists, where the
/// image's links lead from there, and the size and digest of the file at
/// the end, of which at most `remaining` bytes are read. A regular file
/// is compared with what the package recorded of it; a link has nothing
/// recorded to compare with. Answers the observation, its bytes and
/// whether it differs from the record.
fn file_identity(
    rootfs: &Path,
    record: &Document,
    remaining: u64,
    limits: &Limits,
) -> Result<(Document, u64, bool)> {
    const BUDGET: Fault = Fault("rpm_notice_file_budget");
    const UNREAD: Fault = Fault("rpm_notice_file_unread");
    let name = record
        .get("path")
        .and_then(Document::text)
        .ok_or(METADATA)?;
    let mode = record
        .get("mode")
        .and_then(Document::whole)
        .ok_or(METADATA)?;
    let recorded = record
        .get("size")
        .and_then(Document::whole)
        .ok_or(METADATA)?;
    let checksum = record.get("digest").ok_or(METADATA)?;
    let expected = checksum
        .get("value")
        .and_then(Document::text)
        .ok_or(METADATA)?;
    let (regular, link) = (mode & KIND == 0o100_000, mode & KIND == 0o120_000);
    let algorithm = checksum.get("algorithm").and_then(Document::text) == Some("sha256");
    let sized = (0..=i128::from(limits.notice_bytes)).contains(&recorded);
    let digested = if regular {
        small_hex(expected, 64)
    } else {
        expected.is_empty()
    };
    require(
        (regular || link) && algorithm && sized && digested,
        METADATA.0,
    )?;
    let resolved = elf::image_path(rootfs, name).map_err(stopped)?;
    let below = resolved
        .strip_prefix(rootfs)
        .ok()
        .and_then(Path::to_str)
        .ok_or(UNREAD)?;
    let resolved_name = format!("/{below}");
    require(
        notice_path(&resolved_name, limits)?,
        "rpm_notice_link_scope",
    )?;
    let mut options = OpenOptions::new();
    let flags = rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK;
    options.read(true).custom_flags(flags.bits() as i32);
    let mut stream = options.open(&resolved).map_err(|_| UNREAD)?;
    let before = stream.metadata().map_err(|_| UNREAD)?;
    let most = limits.notice_bytes.min(remaining);
    require(before.is_file() && before.len() <= most, BUDGET.0)?;
    let (mut hash, mut length) = (Sha256::new(), 0_u64);
    let mut block = vec![0_u8; 65536];
    loop {
        let read = stream.read(&mut block).map_err(|_| UNREAD)?;
        if read == 0 {
            break;
        }
        length += read as u64;
        require(length <= most, BUDGET.0)?;
        hash.update(&block[..read]);
    }
    let after = stream.metadata().map_err(|_| UNREAD)?;
    let same = secrets::identity(&before) == secrets::identity(&after) && length == before.len();
    require(same, "rpm_notice_changed_during_read")?;
    let actual = hexadecimal(&hash.finalize());
    let matches = actual == expected && i128::from(length) == recorded;
    let observed = Document::record([
        ("path", name.into()),
        ("resolvedPath", resolved_name.into()),
        ("kind", if regular { "regular" } else { "symlink" }.into()),
        ("bytes", length.into()),
        ("sha256", actual.into()),
        ("rpmFile", record.clone()),
        ("matchesRpmDigest", regular.then_some(matches).into()),
    ]);
    Ok((observed, length, regular && !matches))
}

/// The report of the notices of the `packages` of the inventory in the
/// image whose final file system is at `rootfs`. It passes when no
/// regular notice differs from what its package recorded.
pub fn collect(
    rootfs: &Path,
    packages: &[Document],
    image: &Image<'_>,
    limits: &Limits,
) -> Result<Document> {
    let architecture = match image.platform {
        "linux/amd64" => Some("x86_64"),
        "linux/arm64" => Some("aarch64"),
        _ => None,
    };
    let identity = image.image_id.strip_prefix("sha256:");
    let identified = identity.is_some_and(|identity| small_hex(identity, 64))
        && small_hex(image.archive_sha256, 64)
        && small_hex(image.sbom_sha256, 64)
        && small_hex(image.revision, 40);
    let architecture = architecture.filter(|_| identified);
    let architecture = architecture.ok_or(Fault("rpm_notice_artifact_identity"))?;
    // Only an absolute path without a link is what it leads to.
    let real = rootfs.canonicalize().is_ok_and(|real| real == rootfs);
    require(real && rootfs.is_dir(), "rpm_notice_rootfs")?;
    let counted = (1..=limits.packages).contains(&packages.len());
    require(counted, "rpm_notice_package_budget")?;
    let (mut result, mut excluded) = (Vec::new(), Vec::new());
    let mut seen = BTreeSet::new();
    let (mut metadata_files, mut count, mut directories) = (0_usize, 0_usize, 0_usize);
    let (mut total, mut passed) = (0_u64, true);
    for package in packages {
        require(package.fields().is_some(), PACKAGE.0)?;
        if package.get("type").and_then(Document::text) != Some("rpm") {
            continue;
        }
        let found_by = package.get("foundBy").and_then(Document::text);
        let files = package
            .get("metadata")
            .and_then(|metadata| metadata.get("files"));
        let files = files.and_then(Document::items);
        if found_by == Some(NATIVE_CATALOGER) {
            let entry = package.get("metadataType").and_then(Document::text);
            let built = entry == Some("rpm-db-entry") && files.is_some_and(<[Document]>::is_empty);
            require(built, "rpm_notice_supplemental_origin")?;
            excluded.push(Document::record([
                ("purl", package.get("purl").cloned().ok_or(PACKAGE)?),
                ("reason", "static-build-input-not-runtime-files".into()),
            ]));
            continue;
        }
        require(found_by == Some(CATALOGER), "rpm_notice_cataloger")?;
        let identity = package_identity(package, architecture)?;
        let scope = identity
            .get("purl")
            .and_then(Document::text)
            .ok_or(PACKAGE)?;
        require(
            seen.insert(scope.to_owned()),
            "rpm_notice_duplicate_package",
        )?;
        let mut notices = Vec::new();
        let mut listed = BTreeSet::new();
        for record in files.ok_or(PACKAGE)? {
            metadata_files += 1;
            let within = metadata_files <= limits.metadata_files;
            require(within, "rpm_notice_metadata_budget")?;
            let name = record
                .get("path")
                .and_then(Document::text)
                .ok_or(METADATA)?;
            require(listed.insert(name), "rpm_notice_duplicate_path")?;
            if !notice_path(name, limits)? {
                continue;
            }
            let mode = record
                .get("mode")
                .and_then(Document::whole)
                .ok_or(METADATA)?;
            if mode & KIND == 0o040_000 {
                directories += 1;
                continue;
            }
            count += 1;
            require(count <= limits.notices, "rpm_notice_count_budget")?;
            let remaining = limits.total_bytes - total;
            let (observed, bytes, differs) = file_identity(rootfs, record, remaining, limits)?;
            total += bytes;
            passed &= !differs;
            notices.push(observed);
        }
        let mut entry = identity.fields().ok_or(PACKAGE)?.to_vec();
        entry.push(("licenses".to_owned(), license_identity(package, limits)?));
        entry.push(("notices".to_owned(), Document::List(notices)));
        result.push(Document::Record(entry));
    }
    require(!result.is_empty(), "rpm_notice_runtime_inventory_missing")?;
    let report = Document::record([
        ("schemaVersion", 1_usize.into()),
        ("passed", passed.into()),
        ("imageId", image.image_id.into()),
        ("archiveSha256", image.archive_sha256.into()),
        ("sbomSha256", image.sbom_sha256.into()),
        ("revision", image.revision.into()),
        ("platform", image.platform.into()),
        ("runtimePackages", Document::List(result)),
        ("excludedStaticInputs", Document::List(excluded)),
        ("metadataFiles", metadata_files.into()),
        ("noticeFiles", count.into()),
        ("noticeBytes", total.into()),
        ("noticeDirectories", directories.into()),
        (
            "scope",
            "Runtime RPM notice-file identities; no license approval and no file contents".into(),
        ),
        (
            "limits",
            Document::record([
                ("packages", limits.packages.into()),
                ("metadataFiles", limits.metadata_files.into()),
                ("notices", limits.notices.into()),
                ("fileBytes", limits.notice_bytes.into()),
                ("totalBytes", limits.total_bytes.into()),
                ("reportBytes", limits.report_bytes.into()),
            ]),
        ),
    ]);
    // The size of the report as it is published: spread over lines, in
    // whatever order its fields are written.
    let size = report.pretty().len();
    require(size <= limits.report_bytes, "rpm_notice_report_budget")?;
    Ok(report)
}

#[cfg(test)]
#[path = "notices_tests.rs"]
mod tests;
