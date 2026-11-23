//! The runtime proof: that the shipped server, under the managed execution
//! profile, never loads the OpenSSL of the image's rpms that the reviewed
//! advisory covers. The proof binds the exact archive, the native build's
//! facts, the loader's profile in the image and a source review that
//! expires and never renews itself; only then is that one advisory
//! disposed of for that server, in an OpenVEX statement, while the rpm
//! stays affected and visible. No code of the image runs and no statement
//! from outside is trusted.

use super::{bindings, policy::Moment, small_hex};
use crate::{
    Fault, Result, commit, digest,
    document::Document,
    files,
    policy::Date,
    require,
    vendor::diff::{ends, lines},
};
use std::{collections::BTreeSet, path::Path};

/// The source review, below the checkout.
pub const REVIEW: &str = "security/runtime-source-review.json";
/// The name switch of the managed profile, below the checkout.
pub const NSS: &str = "security/runtime/nsswitch.conf";
/// The runtime profile's own checks, below the checkout: the source of
/// `simplestchat-ops release profile`, whose image defaults the proof applies
/// and whose rules a launcher applies to the image and its deployment.
pub const PROFILE: &str = "ops/src/release/profile.rs";
/// The source trees that the review covers.
pub const SOURCE_TREES: [&str; 5] = [
    "src",
    "vendor/mediasoup-0.29.0/src",
    "vendor/mediasoup-sys-0.19.0/src",
    "vendor/mediasoup-sys-0.19.0/include",
    "vendor/mediasoup-sys-0.19.0/deps",
];
/// The source files that the review covers.
pub const SOURCE_FILES: [&str; 4] = [
    "Cargo.toml",
    "Cargo.lock",
    "vendor/integrity.json",
    "vendor/native-components.json",
];
/// The one advisory that a proof disposes of.
const CVE: &str = "CVE-2026-84782";
/// Who makes the statements.
const REPOSITORY: &str = "https://github.com/swiftraccoon/simplestChat";
/// The one execution profile that a proof covers.
const SCOPE: &str = "managed-default-server-dtls";
/// What made a statement of the proven server.
const PROVEN: &str = "simplestchat-security image; explicit source review and exact artifact proof";
/// What made the statement that no claim is needed.
const UNCLAIMED: &str = "simplestchat-security image; no affected reviewed RPM, no VEX claim";
/// What a statement of the proven server says of itself.
const IMPACT: &str = "Applies only to this exact image's shipped server under the enforced \
    managed runtime profile. DTLS uses authenticated static OpenSSL 4.1.0 with external \
    DSO/module/engine loading disabled. The installed Fedora RPM remains affected; other \
    programs and operator-selected execution profiles are outside this statement.";
/// A review, a report or a record of the image that lacks a field or has
/// one of another kind.
const SHAPE: Fault = Fault("runtime_evidence_shape");

/// What git answers to its arguments: at most the given number of bytes of
/// its output, or a refusal.
pub type Git<'a> = dyn FnMut(&[String], u64) -> Result<Vec<u8>> + 'a;

/// The bounds of a proof.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// The most bytes of the review.
    pub review: u64,
    /// The most bytes of a source file, an image's configuration or a record.
    pub source: u64,
    /// The most bytes of a published report.
    pub report: u64,
    /// The most bytes of the name switch.
    pub nss: u64,
    /// The most entries of the image's tree that are looked at.
    pub entries: usize,
    /// The deepest directory of the image's tree.
    pub depth: usize,
    /// The most assignments of the image's environment.
    pub environment: usize,
    /// The most bytes of the image's environment, written as JSON.
    pub environment_bytes: usize,
    /// The most days that a review may cover.
    pub review_days: i64,
}

impl Limits {
    /// The reviewed bounds.
    pub const REVIEWED: Self = Self {
        review: 65_536,
        source: 1024 * 1024,
        report: 8 * 1024 * 1024,
        nss: 8192,
        entries: 200_000,
        depth: 128,
        environment: 256,
        environment_bytes: 65_536,
        review_days: 60,
    };
}

fn record(value: Option<&Document>) -> Result<&Document> {
    value.filter(|value| value.fields().is_some()).ok_or(SHAPE)
}

fn field<'a>(item: &'a Document, name: &str) -> Result<&'a Document> {
    item.get(name).ok_or(SHAPE)
}

fn words(value: &Document) -> Result<&str> {
    value.text().ok_or(SHAPE)
}

/// The JSON record that `content` holds.
fn parsed(content: &[u8]) -> Result<Document> {
    let read = Document::read(content).ok();
    read.filter(|value| value.fields().is_some()).ok_or(SHAPE)
}

/// One deterministic spelling of a value that a binding hashes: its
/// records' fields in the order of their names, nothing between items.
fn encoded(value: &Document) -> String {
    value.clone().sorted().compact()
}

/// A moment as the reference wrote one: the day and the time in UTC, the
/// microseconds only when there are any, and the offset.
fn written(moment: Moment) -> String {
    const DAY: i64 = 86_400_000_000;
    let (days, within) = (moment.0.div_euclid(DAY), moment.0.rem_euclid(DAY));
    let day = Date::of_epoch_day(u64::try_from(days).unwrap_or_default()).unwrap_or(Date::MIN);
    let (seconds, micros) = (within / 1_000_000, within % 1_000_000);
    let (hour, minute, second) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    let fraction = if micros == 0 {
        String::new()
    } else {
        format!(".{micros:06}")
    };
    format!("{day}T{hour:02}:{minute:02}:{second:02}{fraction}+00:00")
}

/// Whether `text` is shaped as a day: four, two and two digits of ASCII.
fn dated(text: &str) -> bool {
    let bytes = text.as_bytes();
    let digit = |at: usize| bytes.get(at).is_some_and(u8::is_ascii_digit);
    bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && [0, 1, 2, 3, 5, 6, 8, 9].iter().all(|at| digit(*at))
}

/// Check the explicit, expiring review of the release's sources against
/// what `git` answers for `revision`: every reviewed tree and file must be
/// the release's own, and the profile's files must also be the checkout's.
/// The review is read at `today`; it never renews itself.
pub fn source_review(
    root: &Path,
    revision: &str,
    today: Date,
    limits: &Limits,
    git: &mut Git<'_>,
) -> Result<Document> {
    require(commit(revision), "runtime_source_revision")?;
    let body = files::bounded(&root.join(REVIEW), limits.review, false)?;
    let review = parsed(&body)?;
    let fields = BTreeSet::from([
        "schemaVersion",
        "reviewedOn",
        "expires",
        "scope",
        "assessment",
        "trees",
        "files",
        "profileFiles",
    ]);
    let schema = review.names() == fields
        && review.get("schemaVersion").and_then(Document::whole) == Some(1)
        && review.get("scope") == Some(&SCOPE.into())
        && !words(field(&review, "assessment")?)?.is_empty();
    require(schema, "runtime_source_review_schema")?;
    let mut shaped = true;
    for key in ["reviewedOn", "expires"] {
        shaped = shaped && dated(words(field(&review, key)?)?);
    }
    require(shaped, "runtime_source_review_date")?;
    let start = Date::parse(words(field(&review, "reviewedOn")?)?).ok_or(SHAPE)?;
    let end = Date::parse(words(field(&review, "expires")?)?).ok_or(SHAPE)?;
    let covered = end.epoch_day() - start.epoch_day();
    let current = start <= today && today < end && covered <= limits.review_days;
    require(current, "runtime_source_review_expired")?;
    let trees = record(review.get("trees"))?;
    let reviewed = record(review.get("files"))?;
    let profiles = record(review.get("profileFiles"))?;
    let scoped = trees.names() == BTreeSet::from(SOURCE_TREES)
        && reviewed.names() == BTreeSet::from(SOURCE_FILES)
        && profiles.names() == BTreeSet::from([PROFILE, NSS]);
    require(scoped, "runtime_source_review_scope")?;
    let mut expected = Vec::new();
    for path in SOURCE_TREES {
        expected.push(words(field(trees, path)?)?);
    }
    require(
        expected.iter().all(|tree| commit(tree)),
        "runtime_source_tree_identity",
    )?;
    let mut asked = vec!["rev-parse".to_owned()];
    asked.extend(SOURCE_TREES.map(|path| format!("{revision}:{path}")));
    let answered = git(&asked, 4096)?;
    let answered = std::str::from_utf8(&answered).map_err(|_| SHAPE)?;
    let actual: Vec<&str> = lines(answered)
        .into_iter()
        .map(|line| line.trim_end_matches(ends))
        .collect();
    require(actual == expected, "runtime_source_tree_changed")?;
    let every = reviewed.fields().unwrap_or_default().iter();
    for (path, value) in every.chain(profiles.fields().unwrap_or_default()) {
        let expected = words(value)?;
        require(small_hex(expected, 64), "runtime_source_file_identity")?;
        let shown = git(
            &["show".to_owned(), format!("{revision}:{path}")],
            limits.source,
        )?;
        require(digest(&shown) == expected, "runtime_source_file_changed")?;
        if profiles.get(path).is_some() {
            let working = files::bounded(&root.join(path), limits.source, false)?;
            require(digest(&working) == expected, "runtime_profile_changed")?;
        }
    }
    Ok(Document::record([
        ("sha256", digest(&body).into()),
        ("expires", field(&review, "expires")?.clone()),
        ("scope", field(&review, "scope")?.clone()),
        ("trees", trees.clone()),
        ("files", reviewed.clone()),
        ("profileFiles", profiles.clone()),
    ]))
}

/// Check that the image at `root` has no loader cache, preload list or
/// directory of hardware capabilities anywhere, without following a link
/// or looking at more of the tree than `limits` allow, and that its name
/// switch is a regular file of exactly the bytes `expected_nss`.
pub fn filesystem_profile(root: &Path, expected_nss: &[u8], limits: &Limits) -> Result<Document> {
    let directory = |path: &Path| path.symlink_metadata().is_ok_and(|entry| entry.is_dir());
    require(directory(root), "runtime_root")?;
    let settings = root.join("etc");
    for name in ["ld.so.cache", "ld.so.preload"] {
        require(directory(&settings), "runtime_etc_directory")?;
        let present = settings.join(name).symlink_metadata().is_ok();
        require(!present, "runtime_loader_hook")?;
    }
    let mut pending = vec![(root.to_path_buf(), 0)];
    let mut count = 0;
    while let Some((path, depth)) = pending.pop() {
        require(depth <= limits.depth, "runtime_directory_depth")?;
        for entry in std::fs::read_dir(&path).map_err(|_| SHAPE)? {
            let entry = entry.map_err(|_| SHAPE)?;
            count += 1;
            require(count <= limits.entries, "runtime_entry_limit")?;
            require(entry.file_name() != "glibc-hwcaps", "runtime_hwcaps")?;
            if entry.file_type().map_err(|_| SHAPE)?.is_dir() {
                pending.push((entry.path(), depth + 1));
            }
        }
    }
    let nss = settings.join("nsswitch.conf");
    let kind = nss.symlink_metadata().map_err(|_| SHAPE)?;
    require(kind.is_file(), "runtime_nss_kind")?;
    let content = files::bounded(&nss, limits.nss, false)?;
    require(content == expected_nss, "runtime_nss_profile")?;
    Ok(Document::record([
        ("cacheAbsent", true.into()),
        ("preloadAbsent", true.into()),
        ("hwcapsAbsent", true.into()),
        ("nssSha256", digest(&content).into()),
        ("entriesInspected", count.into()),
    ]))
}

/// The digest of the published report at `path`, whose bytes must hold
/// exactly the value `expected`, whatever the order of its fields.
pub fn report_digest(path: &Path, expected: &Document, limit: u64) -> Result<String> {
    let body = files::bounded(path, limit, false)?;
    let value = parsed(&body)?;
    require(
        value.sorted() == expected.clone().sorted(),
        "runtime_report_changed",
    )?;
    Ok(digest(&body))
}

/// What a proof binds: the image's unpacked archive at `tree` (its report,
/// configuration and root, beside the published `native.json` and
/// `elf.json`), the binary's and the native build's records, the release's
/// manifest, the checkout at `root`, the inventory's digest and the
/// reviewed advisory policy's.
pub struct Evidence<'a> {
    /// The unpacked archive.
    pub tree: &'a Path,
    /// The binary's record.
    pub elf: &'a Document,
    /// The native build's record.
    pub native: &'a Document,
    /// The release's manifest.
    pub manifest: &'a Document,
    /// The checkout.
    pub root: &'a Path,
    /// The digest of the image's inventory.
    pub sbom_sha256: &'a str,
    /// The digest of the reviewed advisory policy.
    pub advisory_policy_sha256: &'a str,
}

/// The image's own execution defaults, as the managed runtime holds them.
fn defaults(runtime: &Document) -> Result<()> {
    let checked = simplestchat_ops::release::profile::image_defaults(runtime.compact().as_bytes());
    checked.map_err(|code| code.map_or(SHAPE, Fault))
}

/// The proof that the exact image's server runs under the managed profile
/// with the authenticated static OpenSSL, against a source review that
/// `review` reads for the release's revision.
pub fn proof(
    evidence: &Evidence<'_>,
    limits: &Limits,
    review: &mut dyn FnMut(&str) -> Result<Document>,
) -> Result<Document> {
    let Evidence {
        tree,
        elf,
        native,
        manifest,
        root,
        ..
    } = *evidence;
    let report = files::bounded(&tree.join("report.json"), limits.source, false)?;
    let archive = parsed(&report)?;
    let configuration =
        files::bounded(&tree.join("layers/image-config.json"), limits.source, false)?;
    let config = parsed(&configuration)?;
    let identity = digest(&configuration);
    let bound = archive.get("passed") == Some(&Document::Bool(true))
        && field(&archive, "configSha256")?.text() == Some(identity.as_str())
        && field(&archive, "imageId")?.text() == Some(format!("sha256:{identity}").as_str())
        && field(&archive, "archiveSha256")? == field(manifest, "archiveSha256")?
        && field(&archive, "revision")? == field(manifest, "revision")?
        && field(&archive, "platform")? == field(manifest, "platform")?;
    require(bound, "runtime_archive_binding")?;
    let runtime = record(config.get("config"))?;
    let environment = field(runtime, "Env")?.items().ok_or(SHAPE)?;
    let assignments = encoded(&Document::List(environment.to_vec()));
    let bounded =
        environment.len() <= limits.environment && assignments.len() <= limits.environment_bytes;
    require(bounded, "runtime_environment_bounds")?;
    defaults(runtime)?;
    let placed = field(&config, "os")? == &Document::from("linux")
        && {
            let machine = words(field(&config, "architecture")?)?;
            field(manifest, "platform")?.text() == Some(format!("linux/{machine}").as_str())
        }
        && field(elf, "platform")? == field(manifest, "platform")?
        && field(
            record(runtime.get("Labels"))?,
            "org.opencontainers.image.revision",
        )? == field(manifest, "revision")?;
    require(placed, "runtime_platform_revision")?;
    let holds = |name: &str| elf.get(name) == Some(&Document::Bool(true));
    let profiled = holds("passed")
        && holds("standardDirectoryCandidatesEqual")
        && holds("noLibraryLoaderHooks")
        && record(elf.get("checks"))?.get("noLoaderHooks") == Some(&Document::Bool(true));
    require(profiled, "runtime_elf_profile")?;
    let binary = field(record(native.get("binary"))?, "sha256")?;
    require(
        binary == field(elf, "binarySha256")?,
        "runtime_native_binary",
    )?;
    let openssl = record(native.get("openssl"))?;
    let disabled = Document::List(bindings::DISABLED.map(Document::from).to_vec());
    let built = field(openssl, "version")? == &Document::from("4.1.0")
        && field(record(openssl.get("build"))?, "disabled_options")? == &disabled;
    require(built, "runtime_static_openssl")?;
    let components = files::bounded(
        &root.join("vendor/native-components.json"),
        limits.source,
        false,
    )?;
    let reviewed = parsed(&components)?;
    bindings::openssl_build_binding(native, record(reviewed.get("openssl"))?)?;
    let manifested =
        field(native, "native_manifest_sha256")?.text() == Some(digest(&components).as_str());
    let sourced = manifested && {
        let recorded = field(native, "cargo_lock_sha256")?;
        let lock = files::bounded(&root.join("Cargo.lock"), limits.source, false)?;
        recorded.text() == Some(digest(&lock).as_str())
    };
    require(sourced, "runtime_native_source")?;
    let review = review(words(field(manifest, "revision")?)?)?;
    let nss = files::bounded(&root.join(NSS), limits.nss, false)?;
    let filesystem = filesystem_profile(&tree.join("rootfs"), &nss, limits)?;
    let published = tree.parent().unwrap_or(tree);
    Ok(Document::record([
        ("schemaVersion", 1_u64.into()),
        ("required", true.into()),
        ("passed", true.into()),
        ("scope", SCOPE.into()),
        ("archiveSha256", field(manifest, "archiveSha256")?.clone()),
        ("imageId", field(&archive, "imageId")?.clone()),
        ("revision", field(manifest, "revision")?.clone()),
        ("platform", field(manifest, "platform")?.clone()),
        ("binarySha256", field(elf, "binarySha256")?.clone()),
        (
            "nativeSha256",
            report_digest(&published.join("native.json"), native, limits.report)?.into(),
        ),
        (
            "elfSha256",
            report_digest(&published.join("elf.json"), elf, limits.report)?.into(),
        ),
        ("sbomSha256", evidence.sbom_sha256.into()),
        ("imageConfigSha256", identity.into()),
        ("environmentSha256", digest(assignments.as_bytes()).into()),
        ("filesystem", filesystem),
        ("sourceReview", review),
        (
            "advisoryPolicySha256",
            evidence.advisory_policy_sha256.into(),
        ),
    ]))
}

/// The exact rpms of the image that the reviewed advisory found affected:
/// the verdict must be of the reviewed policy whose digest is `policy`.
pub fn affected_scopes(verdict: &Document, policy: &str) -> Result<BTreeSet<String>> {
    let reviewed = record(verdict.get("reviewedAdvisories"))?;
    let identity = field(reviewed, "policySha256")?.text() == Some(policy);
    require(identity, "runtime_advisory_identity")?;
    let mut scopes = BTreeSet::new();
    for finding in field(reviewed, "blocked")?.items().ok_or(SHAPE)? {
        let finding = record(Some(finding))?;
        let named = field(finding, "id")? == &Document::from(CVE)
            && field(finding, "source")? == &Document::from("reviewed-openssl-advisory");
        require(named, "runtime_advisory_finding")?;
        scopes.insert(words(field(finding, "scope")?)?.to_owned());
    }
    Ok(scopes)
}

/// The verdict with the advisory's findings of the proven server's rpms
/// disposed of, beside the OpenVEX statement that says so at `now`: the
/// findings stay in the verdict as affected, and the proof at `path` must
/// be the published bytes of `evidence`.
pub fn apply(
    verdict: &Document,
    evidence: &Document,
    path: &Path,
    policy: &str,
    now: Moment,
    limits: &Limits,
) -> Result<(Document, Document)> {
    let required = evidence.get("passed") == Some(&Document::Bool(true))
        && evidence.get("required") == Some(&Document::Bool(true))
        && evidence.get("scope") == Some(&SCOPE.into());
    require(required, "runtime_proof_required")?;
    let scopes = affected_scopes(verdict, policy)?;
    require(!scopes.is_empty(), "runtime_no_affected_rpm")?;
    let reviewed = record(verdict.get("reviewedAdvisories"))?;
    let advisory = field(reviewed, "policySha256")?;
    require(
        field(evidence, "advisoryPolicySha256")? == advisory,
        "runtime_proof_advisory",
    )?;
    let affected: Vec<Document> = scopes.iter().map(|scope| scope.as_str().into()).collect();
    let binding = Document::record([
        (
            "proofSha256",
            report_digest(path, evidence, limits.report)?.into(),
        ),
        ("advisoryPolicySha256", advisory.clone()),
        ("affectedScopes", Document::List(affected)),
    ]);
    let subject = format!(
        "urn:simplestchat:managed-server:sha256:{}",
        digest(encoded(&binding).as_bytes())
    );
    let issued = written(now);
    let stamped = Document::record([
        ("binding", binding.clone()),
        ("issued", issued.as_str().into()),
    ]);
    let parts: Vec<Document> = scopes
        .iter()
        .map(|scope| {
            Document::record([
                ("@id", scope.as_str().into()),
                (
                    "identifiers",
                    Document::record([("purl", scope.as_str().into())]),
                ),
            ])
        })
        .collect();
    let product = Document::record([
        ("@id", subject.as_str().into()),
        (
            "hashes",
            Document::record([("sha-256", field(evidence, "archiveSha256")?.clone())]),
        ),
        ("subcomponents", Document::List(parts)),
    ]);
    let proof = words(field(&binding, "proofSha256")?)?;
    let expires = words(field(record(evidence.get("sourceReview"))?, "expires")?)?;
    let statement = Document::record([
        ("vulnerability", Document::record([("name", CVE.into())])),
        ("products", Document::List(vec![product])),
        ("status", "not_affected".into()),
        (
            "justification",
            "vulnerable_code_not_in_execute_path".into(),
        ),
        ("impact_statement", IMPACT.into()),
        (
            "status_notes",
            format!("Proof SHA-256: {proof}; source review expires {expires}").into(),
        ),
    ]);
    let document = Document::record([
        ("@context", "https://openvex.dev/ns/v0.2.0".into()),
        (
            "@id",
            format!(
                "urn:simplestchat:vex:sha256:{}",
                digest(encoded(&stamped).as_bytes())
            )
            .into(),
        ),
        ("author", REPOSITORY.into()),
        ("role", "Project security automation".into()),
        ("timestamp", issued.as_str().into()),
        ("version", 1_u64.into()),
        ("tooling", PROVEN.into()),
        ("statements", Document::List(vec![statement])),
    ]);
    let original = field(verdict, "blocked")?.items().ok_or(SHAPE)?;
    let (mut remaining, mut disposed) = (Vec::new(), Vec::new());
    for finding in original {
        let finding = record(Some(finding))?;
        let target = finding.get("id") == Some(&CVE.into())
            && finding
                .get("scope")
                .and_then(Document::text)
                .is_some_and(|scope| scopes.contains(scope));
        if target {
            disposed.push(finding.clone());
        } else {
            remaining.push(finding.clone());
        }
    }
    require(!disposed.is_empty(), "runtime_finding_missing")?;
    let mut disposition = binding;
    disposition.put("subject", subject.into());
    let mut result = verdict.clone();
    result.put("passed", remaining.is_empty().into());
    result.put("blocked", Document::List(remaining));
    result.put("affectedFindings", Document::List(original.to_vec()));
    result.put("notAffectedForManagedServer", Document::List(disposed));
    result.put("runtimeDisposition", disposition);
    Ok((result, document))
}

/// The evidence that no proof is needed, because the reviewed advisory
/// found no rpm of the image affected, beside an OpenVEX document of no
/// statement, at `now`.
pub fn not_required(
    manifest: &Document,
    image_id: &str,
    verdict: &Document,
    policy: &str,
    now: Moment,
) -> Result<(Document, Document)> {
    require(
        affected_scopes(verdict, policy)?.is_empty(),
        "runtime_affected_rpm_requires_proof",
    )?;
    let evidence = Document::record([
        ("schemaVersion", 1_u64.into()),
        ("required", false.into()),
        ("passed", true.into()),
        ("reason", "no_affected_reviewed_rpm".into()),
        ("archiveSha256", field(manifest, "archiveSha256")?.clone()),
        ("imageId", image_id.into()),
        ("revision", field(manifest, "revision")?.clone()),
        ("platform", field(manifest, "platform")?.clone()),
        (
            "advisoryPolicySha256",
            field(record(verdict.get("reviewedAdvisories"))?, "policySha256")?.clone(),
        ),
    ]);
    let issued = written(now);
    let stamped = Document::record([
        ("evidence", evidence.clone()),
        ("issued", issued.as_str().into()),
    ]);
    let document = Document::record([
        ("@context", "https://openvex.dev/ns/v0.2.0".into()),
        (
            "@id",
            format!(
                "urn:simplestchat:vex:sha256:{}",
                digest(encoded(&stamped).as_bytes())
            )
            .into(),
        ),
        ("author", REPOSITORY.into()),
        ("role", "Project security automation".into()),
        ("timestamp", issued.as_str().into()),
        ("version", 1_u64.into()),
        ("tooling", UNCLAIMED.into()),
        ("statements", Document::List(Vec::new())),
    ]);
    Ok((evidence, document))
}

#[cfg(test)]
#[path = "runtime_tests.rs"]
mod tests;
