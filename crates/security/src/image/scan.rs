//! The scans of the image gate, each over what the gate itself staged: the
//! saved image bound to what the trusted producer exported, the selected
//! image held to its archive, the vulnerability database fetched online
//! with nothing of the image mounted, the archive's catalog and its
//! conversion, every layer projected to what a secret scanner reads and
//! scanned, the secret detector proved on canaries it must find, the
//! native evidence bound to the audited executable, the disposition the
//! runtime proof decides, and a receipt of each phase.

use crate::{
    Fault, Result, digest,
    document::Document,
    elf, files,
    image::{
        archive, bindings, cache, enrich,
        outputs::{self, readable},
        policy::{self, Moment},
        runtime,
        sandbox::{Sandbox, Scan},
        secrets as verdicts, small_hex, spans, stopped,
    },
    policy::Exception,
    require, secrets,
};
use sha2::Digest as _;
use std::{
    fs::{self, DirBuilder},
    os::unix::fs::DirBuilderExt,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::Instant,
};

/// The secret scanner's arguments, for the canaries and the image alike.
pub const SECRET_ARGUMENTS: [&str; 12] = [
    "dir",
    "/layers",
    "--no-banner",
    "--redact=100",
    "--config=/tools/gitleaks.toml",
    "--gitleaks-ignore-path=/tools/empty.ignore",
    "--ignore-gitleaks-allow",
    "--max-decode-depth=3",
    "--report-format=json",
    "--report-path=/output/gitleaks.json",
    "--exit-code=10",
    "--max-target-megabytes=0",
];
/// The exit status of a secret scan that found something.
const FINDINGS: i32 = 10;
/// The rule a canary must be found by.
const CANARY_RULE: &str = "github-pat";
/// What the canary is derived from: nothing a credential was ever issued for.
const CANARY_SEED: &[u8] = b"inert scanner coverage only";

/// The bounds of the scans.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// The most bytes of the database.
    pub database: u64,
    /// The most regular files projected for the secret scanner.
    pub projected_files: u64,
    /// The most bytes of every projection together, prefixes included.
    pub projected_bytes: u64,
    /// The space left free on the disk after the projections.
    pub reserve: u64,
    /// The most bytes of one file of a layer.
    pub member: u64,
    /// The most bytes of a scanner's report.
    pub report: u64,
    /// The most bytes of the producer's record of the export.
    pub export: u64,
    /// The most bytes of the native evidence.
    pub native: u64,
}

impl Limits {
    /// The reviewed bounds.
    pub const REVIEWED: Self = Self {
        database: 4 << 30,
        projected_files: 200_001,
        projected_bytes: 4 << 30,
        reserve: 256 << 20,
        member: 256 << 20,
        report: 64 << 20,
        export: 65_536,
        native: 8 << 20,
    };
}

/// Make the new directory `path`, which only its owner may use.
fn private(path: &Path) -> Result<()> {
    let made = DirBuilder::new().mode(0o700).create(path);
    made.map_err(|error| files::unusable(&error))
}

/// Publish `value` as the new private evidence file `path`, sorted as the
/// reference writes it.
pub(crate) fn written(path: &Path, value: &Document) -> Result<()> {
    archive::new_file(path, value.clone().sorted().pretty().as_bytes())
}

/// The digest of the regular file `path`, which is not reached through a
/// link.
pub(crate) fn evidence_digest(path: &Path) -> Result<String> {
    let kind = path.symlink_metadata().ok();
    require(
        kind.is_some_and(|kind| kind.is_file()),
        "image_evidence_file",
    )?;
    let bytes = fs::read(path).map_err(|error| files::unusable(&error))?;
    Ok(digest(&bytes))
}

/// Standard base64 of `bytes`, with its padding.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut text = String::new();
    for chunk in bytes.chunks(3) {
        let group = chunk
            .iter()
            .fold(0_u32, |held, byte| held << 8 | u32::from(*byte));
        let group = group << (8 * (3 - chunk.len()));
        for index in 0..4 {
            if index <= chunk.len() {
                let symbol = (group >> (18 - 6 * index)) & 0x3f;
                text.push(char::from(ALPHABET[symbol as usize]));
            } else {
                text.push('=');
            }
        }
    }
    text
}

/// The canary every format must be found to hold: shaped as a GitHub
/// token, derived from a fixed text, never one that was issued. It is made
/// when it is asked for, so no scan of the sources meets it written out.
pub(crate) fn canary() -> String {
    let derived = base64(&sha2::Sha512::digest(CANARY_SEED));
    let derived = derived.replace('+', "x").replace('/', "y");
    format!("ghp_{}", &derived[..36])
}

/// Every regular file below `layers` and its size, in the order of their
/// paths, once each is held to the bounds of `limits` and their projections
/// to the space `free` says is left beside `output`: refused at once when
/// `cancelled` says the scan was cancelled, at the first entry that is no
/// directory and no regular file, at a file past its bound, and at the first
/// file past the count or the bytes of the projections together.
pub fn secret_inventory(
    layers: &Path,
    output: &Path,
    cancelled: &dyn Fn() -> bool,
    limits: &Limits,
    free: &dyn Fn(&Path) -> Result<u64>,
) -> Result<Vec<(PathBuf, u64)>> {
    let mut sources = Vec::new();
    let mut total = 0_u64;
    let mut pending = vec![layers.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let listing = fs::read_dir(&directory).map_err(|error| files::unusable(&error))?;
        let mut entries = Vec::new();
        for entry in listing {
            entries.push(entry.map_err(|error| files::unusable(&error))?.path());
        }
        entries.sort();
        for source in entries {
            require(!cancelled(), "image_scan_cancelled")?;
            let kind = source
                .symlink_metadata()
                .map_err(|error| files::unusable(&error))?;
            if kind.is_dir() {
                pending.push(source);
                continue;
            }
            require(kind.is_file(), "image_secret_input_kind")?;
            require(kind.len() <= limits.member, "image_secret_input_size")?;
            sources.push((source, kind.len()));
            total = total.saturating_add(kind.len() + secrets::PREFIX_BYTES);
            require(
                sources.len() as u64 <= limits.projected_files,
                "image_secret_input_count",
            )?;
            require(
                total <= limits.projected_bytes,
                "image_secret_projection_budget",
            )?;
        }
    }
    require(!sources.is_empty(), "image_secret_input_empty")?;
    let room = free(output)?.checked_sub(limits.reserve);
    require(
        room.is_some_and(|room| room >= total),
        "image_secret_projection_disk",
    )?;
    sources.sort();
    Ok(sources)
}

/// Project every regular file below `layers` into the new directory
/// `secret-input` below `output`, each under a neutral name in the order of
/// their paths, and keep the map from those names to each original path and
/// the digests of both as `secret-paths.json`. The projections are opened to
/// the scanner, read only.
pub fn secret_bundle(
    layers: &Path,
    output: &Path,
    cancelled: &dyn Fn() -> bool,
    limits: &Limits,
    free: &dyn Fn(&Path) -> Result<u64>,
) -> Result<PathBuf> {
    let sources = secret_inventory(layers, output, cancelled, limits, free)?;
    let inputs = output.join("secret-input");
    private(&inputs)?;
    let mut paths = Vec::new();
    for (index, (source, expected)) in sources.iter().enumerate() {
        require(!cancelled(), "image_scan_cancelled")?;
        let name = format!("content-{index:06}");
        let view = secrets::project(source, &inputs.join(&name), limits.member)?;
        require(view.source_bytes == *expected, "image_secret_input_changed")?;
        let relative = source
            .strip_prefix(layers)
            .map_err(|_| Fault("image_secret_input_kind"))?;
        let relative = relative.to_str().ok_or(Fault("image_secret_input_kind"))?;
        paths.push((
            name,
            Document::record([
                ("path", relative.into()),
                ("sha256", view.source_sha256.into()),
                ("sourceBytes", view.source_bytes.into()),
                ("projectionSha256", view.projection_sha256.into()),
                ("projectionBytes", view.projection_bytes.into()),
                ("projectionFormat", secrets::FORMAT.into()),
            ]),
        ));
    }
    require(!paths.is_empty(), "image_secret_input_empty")?;
    written(&output.join("secret-paths.json"), &Document::Record(paths))?;
    readable(&inputs)?;
    Ok(inputs)
}

/// The prefixes of the canaries' formats: an executable, a disc image, a
/// document and plain text, each a different kind to the scanner's own
/// recognition of a file.
fn canary_formats() -> [(&'static str, Vec<u8>); 4] {
    let mut executable = b"\x7fELF".to_vec();
    executable.extend([0_u8; 64]);
    executable.push(b'\n');
    let mut disc = vec![0_u8; 32_769];
    disc.extend(b"CD001\n");
    [
        ("canary-elf", executable),
        ("canary-iso", disc),
        ("canary-pdf", b"%PDF-1.7\n".to_vec()),
        ("canary-text", Vec::new()),
    ]
}

/// What the scans need of the lock of the scanners: its digest, which the
/// evidence records, and the pinned version of each scanner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pins {
    /// The SHA-256 of the lock.
    pub lock: String,
    /// The catalog's version.
    pub syft: String,
    /// The vulnerability scanner's version.
    pub grype: String,
    /// The secret scanner's version.
    pub gitleaks: String,
}

/// Prove the pinned secret scanner, configured as it scans the image, on a
/// canary in each of four formats: it must report every one by the GitHub
/// token rule at the place the projection holds it, and nothing else.
pub fn secret_selftest(
    sandbox: &mut Sandbox,
    pins: &Pins,
    limits: &Limits,
    free: &dyn Fn(&Path) -> Result<u64>,
) -> Result<()> {
    let work = sandbox.output.join("secret-canary");
    private(&work)?;
    let originals = work.join("originals");
    private(&originals)?;
    let canary = canary();
    let payload = format!("TOKEN={canary} # gitleaks:allow\n");
    let formats = canary_formats();
    for (name, prefix) in &formats {
        let mut bytes = prefix.clone();
        bytes.extend(payload.as_bytes());
        archive::new_file(&originals.join(name), &bytes)?;
    }
    let source = secret_bundle(&originals, &work, &|| false, limits, free)?;
    let identity = policy::report(&work.join("secret-paths.json"), limits.report)?;
    readable(&source)?;
    let output = sandbox.output.join("secret-canary-output");
    let mounts = [("/layers", source.as_path())];
    let (status, _) = sandbox.run(&Scan {
        tool: "gitleaks",
        arguments: &SECRET_ARGUMENTS,
        destination: &output,
        mounts: &mounts,
        online: false,
        variables: &[],
    })?;
    let report = policy::report(&output.join("gitleaks.json"), limits.report)?;
    let bounds = spans::Limits::REVIEWED;
    let regions = spans::collect(&report, &identity, &source, &pins.gitleaks, &bounds)?;
    let verdict = verdicts::secret_verdict(&report, &[], &identity, &regions, &bounds)?;
    let blocked = verdict
        .get("blocked")
        .and_then(Document::items)
        .unwrap_or_default();
    let by_rule = blocked
        .iter()
        .all(|item| item.get("rule").and_then(Document::text) == Some(CANARY_RULE));
    let found: std::collections::BTreeSet<&str> = blocked
        .iter()
        .filter_map(|item| item.get("path").and_then(Document::text))
        .collect();
    let named: std::collections::BTreeSet<&str> = formats.iter().map(|(name, _)| *name).collect();
    let detected =
        status == FINDINGS && blocked.len() == formats.len() && by_rule && found == named;
    require(detected, "image_secret_detector_selftest")?;
    let expected = digest(canary.as_bytes());
    let placed = regions
        .get("findings")
        .and_then(Document::items)
        .unwrap_or_default();
    let resolved = placed.iter().all(|item| {
        item.get("status").and_then(Document::text) == Some("resolved")
            && item.get("spanSha256").and_then(Document::text) == Some(expected.as_str())
    });
    require(resolved, "image_secret_coordinate_selftest")?;
    let formats: Vec<Document> = formats.iter().map(|(name, _)| (*name).into()).collect();
    let evidence = Document::record([
        ("passed", true.into()),
        ("rule", CANARY_RULE.into()),
        ("formats", Document::List(formats)),
        ("projectionFormat", secrets::FORMAT.into()),
        ("toolLockSha256", pins.lock.as_str().into()),
    ]);
    written(&sandbox.output.join("secret-selftest.json"), &evidence)
}

/// The release manifest of the saved image in `artifact`, once the image
/// `image_id` is the exact export the trusted producer recorded as passing,
/// for the revision the manifest names and the platform scanned for. The
/// manifest is answered in its reviewed order, its migrations as written.
pub fn bind_archive(
    image_id: &str,
    artifact: &Path,
    platform: crate::image::sandbox::Platform,
    limits: &Limits,
) -> Result<Document> {
    let identity = image_id.strip_prefix("sha256:");
    require(
        identity.is_some_and(|hex| small_hex(hex, 64)),
        "immutable_image_required",
    )?;
    let outcome = policy::report(&artifact.join("outcome.json"), limits.export)?;
    let exported = outcome.get("passed") == Some(&Document::Bool(true))
        && outcome.get("exportedImageId").and_then(Document::text) == Some(image_id);
    require(exported, "image_export_identity")?;
    let read = simplestchat_ops::release::manifest::Manifest::read(&artifact.join("release.json"));
    let manifest = read.map_err(|_| Fault("release_manifest_invalid"))?;
    let fields = manifest.fields();
    let revision =
        outcome.get("revision").and_then(Document::text) == Some(fields.revision.as_str());
    require(
        revision && fields.platform == platform.name(),
        "image_export_revision_platform",
    )?;
    let original =
        Document::read(manifest.as_json()).map_err(|_| Fault("release_manifest_invalid"))?;
    let migrations = original
        .get("migrations")
        .cloned()
        .ok_or(Fault("release_manifest_invalid"))?;
    Ok(Document::record([
        ("schemaVersion", u64::from(fields.schema_version).into()),
        ("revision", fields.revision.as_str().into()),
        ("platform", fields.platform.as_str().into()),
        ("archiveSha256", fields.archive_sha256.as_str().into()),
        ("imageTag", fields.image_tag.as_str().into()),
        ("migrations", migrations),
        ("createdAt", fields.created_at.as_str().into()),
    ]))
}

/// What the engine says of the image `image_id` that was selected, once it
/// is the one the archive in `tree` holds: the same identity and
/// configuration, the same layers in their order, for `platform`. The
/// format quotes the identity itself, which is only digits, letters and a
/// colon: Podman answers the field to a template but cannot hand it to
/// `json`, and Docker answers it the same either way.
pub fn selected_image(
    sandbox: &mut Sandbox,
    image_id: &str,
    platform: crate::image::sandbox::Platform,
    tree: &Path,
    limits: &Limits,
) -> Result<Document> {
    let format = [
        r#"{"id":"{{.Id}}","layers":{{json .RootFS.Layers}},"#,
        r#""architecture":{{json .Architecture}}}"#,
    ]
    .concat();
    let asked: Vec<std::ffi::OsString> = ["image", "inspect", "--format", &format, image_id]
        .iter()
        .map(Into::into)
        .collect();
    let cancelled = std::sync::Arc::clone(&sandbox.cancelled);
    let mut healthy = move || !cancelled.load(Ordering::SeqCst);
    let (_, said) = sandbox.engine.command(&asked, 30, false, &mut healthy)?;
    let mismatch = Fault("image_selected_archive_mismatch");
    let mut value = Document::read(said.as_bytes()).map_err(|_| mismatch)?;
    let id = value.get("id").and_then(Document::text).ok_or(mismatch)?;
    let canonical = format!("sha256:{}", id.strip_prefix("sha256:").unwrap_or(id));
    let report = policy::report(&tree.join("report.json"), limits.report)?;
    let layers = report
        .get("layers")
        .and_then(Document::items)
        .ok_or(mismatch)?;
    let layers: Option<Vec<Document>> = layers
        .iter()
        .map(|layer| layer.get("diffId").cloned())
        .collect();
    let layers = Document::List(layers.ok_or(mismatch)?);
    let configuration = report
        .get("configSha256")
        .and_then(Document::text)
        .ok_or(mismatch)?;
    let architecture = value.get("architecture").and_then(Document::text);
    let matched = canonical == image_id
        && report.get("imageId").and_then(Document::text) == Some(image_id)
        && format!("sha256:{configuration}") == image_id
        && value.get("layers") == Some(&layers)
        && architecture
            .is_some_and(|architecture| format!("linux/{architecture}") == platform.name());
    require(matched, mismatch.0)?;
    value.put("archiveConfigSha256", configuration.into());
    Ok(value)
}

/// Fetch a fresh vulnerability database with nothing of the image mounted,
/// hold it to its bound, read its status through the scanner and hold that
/// to `image_policy` at `now`, and keep it with the digests of the database
/// and its import record; then save it to the sandbox's cache, if it has
/// one.
pub fn prepare_database(
    sandbox: &mut Sandbox,
    image_policy: &Document,
    now: Moment,
    limits: &Limits,
) -> Result<Document> {
    let database = sandbox.output.join("database");
    let (status, _) = sandbox.run(&Scan {
        tool: "grype",
        arguments: &["db", "update"],
        destination: &database,
        mounts: &[],
        online: true,
        variables: &[("GRYPE_DB_CACHE_DIR", "/output/cache")],
    })?;
    require(status == 0, "image_database_update_failed")?;
    let usage = outputs::usage(&database, limits.database, sandbox.limits.entries);
    require(usage.within, "image_database_output_limit")?;
    readable(&database)?;
    let mounts = [("/db", database.as_path())];
    let (status, said) = sandbox.run(&Scan {
        tool: "grype",
        arguments: &["db", "status", "-o", "json"],
        destination: &sandbox.output.join("database-status"),
        mounts: &mounts,
        online: false,
        variables: &[("GRYPE_DB_CACHE_DIR", "/db/cache")],
    })?;
    require(status == 0, "image_database_status_failed")?;
    let unread = Fault("image_database_status_unread");
    let mut reported = Document::read(said.as_bytes()).map_err(|_| unread)?;
    require(reported.fields().is_some(), unread.0)?;
    policy::database_status(&reported, image_policy, now)?;
    let schema = database.join("cache/6");
    reported.put(
        "databaseSha256",
        evidence_digest(&schema.join("vulnerability.db"))?.into(),
    );
    reported.put(
        "importReceiptSha256",
        evidence_digest(&schema.join("import.json"))?.into(),
    );
    require(!sandbox.cancelled(), "image_scan_cancelled")?;
    written(&sandbox.output.join("database-status.json"), &reported)?;
    if let Some(seed) = &sandbox.cache {
        cache::save(&seed.directory, &database, &seed.pin, &seed.limits)?;
    }
    Ok(reported)
}

/// Copy the file at `source` to `destination` as the reference copied one:
/// the bytes of a regular file, the target of a link as a link.
fn copied(source: &Path, destination: &Path) -> Result<()> {
    let kind = source
        .symlink_metadata()
        .map_err(|error| files::unusable(&error))?;
    if kind.file_type().is_symlink() {
        let target = fs::read_link(source).map_err(|error| files::unusable(&error))?;
        let made = std::os::unix::fs::symlink(target, destination);
        return made.map_err(|error| files::unusable(&error));
    }
    let mut reader = fs::File::open(source).map_err(|error| files::unusable(&error))?;
    let mut options = fs::OpenOptions::new();
    std::os::unix::fs::OpenOptionsExt::mode(options.write(true).create_new(true), 0o600);
    let mut writer = options
        .open(destination)
        .map_err(|error| files::unusable(&error))?;
    std::io::copy(&mut reader, &mut writer).map_err(|error| files::unusable(&error))?;
    Ok(())
}

/// Catalog a copy of the saved image in `artifact` with nothing else
/// mounted, enrich the catalog with the native evidence and the audit of the
/// executable, hold it to `image_policy`, and convert it to SPDX; answer
/// where the enriched catalog is.
pub fn scan_sbom(
    sandbox: &mut Sandbox,
    artifact: &Path,
    native: &Document,
    elf: &Document,
    image_policy: &Document,
    pins: &Pins,
    limits: &Limits,
) -> Result<PathBuf> {
    let inputs = sandbox.output.join("scanner-input");
    private(&inputs)?;
    let (original, copy) = (artifact.join("image.tar"), inputs.join("image.tar"));
    copied(&original, &copy)?;
    let same = evidence_digest(&copy)? == evidence_digest(&original)?;
    require(same, "image_archive_copy_changed")?;
    readable(&inputs)?;
    let raw = sandbox.output.join("sbom-raw");
    let mounts = [("/input", inputs.as_path())];
    let (status, _) = sandbox.run(&Scan {
        tool: "syft",
        arguments: &[
            "scan",
            "docker-archive:/input/image.tar",
            "-o",
            "syft-json=/output/sbom.syft.json",
        ],
        destination: &raw,
        mounts: &mounts,
        online: false,
        variables: &[],
    })?;
    require(status == 0, "image_sbom_failed")?;
    let sbom = sandbox.output.join("sbom");
    private(&sbom)?;
    let value = policy::report(&raw.join("sbom.syft.json"), limits.report)?;
    let enriched = enrich::enrich_sbom(
        value,
        native,
        elf,
        image_policy,
        &pins.syft,
        &policy::Limits::REVIEWED,
    )?;
    written(&sbom.join("sbom.syft.json"), &enriched)?;
    readable(&sbom)?;
    let mounts = [("/input", sbom.as_path())];
    let (status, _) = sandbox.run(&Scan {
        tool: "syft",
        arguments: &[
            "convert",
            "/input/sbom.syft.json",
            "-o",
            "spdx-json=/output/sbom.spdx.json",
        ],
        destination: &sandbox.output.join("spdx"),
        mounts: &mounts,
        online: false,
        variables: &[],
    })?;
    require(status == 0, "image_spdx_conversion_failed")?;
    Ok(sbom)
}

/// Project every layer of the archive in `tree` and scan the projections
/// for secrets; answer where the report is. Findings are a verdict's to
/// judge, not a failure of the scan.
pub fn scan_secrets(
    sandbox: &mut Sandbox,
    tree: &Path,
    limits: &Limits,
    free: &dyn Fn(&Path) -> Result<u64>,
) -> Result<PathBuf> {
    let flag = std::sync::Arc::clone(&sandbox.cancelled);
    let cancelled = move || flag.load(Ordering::SeqCst);
    let inputs = secret_bundle(
        &tree.join("layers"),
        &sandbox.output,
        &cancelled,
        limits,
        free,
    )?;
    let secret = sandbox.output.join("secrets");
    let mounts = [("/layers", inputs.as_path())];
    let (status, _) = sandbox.run(&Scan {
        tool: "gitleaks",
        arguments: &SECRET_ARGUMENTS,
        destination: &secret,
        mounts: &mounts,
        online: false,
        variables: &[],
    })?;
    require(
        status == 0 || status == FINDINGS,
        "image_secret_scan_failed",
    )?;
    Ok(secret)
}

/// The secret verdict of the report in `secret`, its findings located in
/// the projections below `output` and judged against `exceptions`, with
/// the located regions themselves.
pub fn secret_checks(
    secret: &Path,
    output: &Path,
    exceptions: &[Exception],
    pins: &Pins,
    limits: &Limits,
) -> Result<Document> {
    let report = policy::report(&secret.join("gitleaks.json"), limits.report)?;
    let paths = policy::report(&output.join("secret-paths.json"), limits.report)?;
    let bounds = spans::Limits::REVIEWED;
    let inputs = output.join("secret-input");
    let regions = spans::collect(&report, &paths, &inputs, &pins.gitleaks, &bounds)?;
    let mut verdict = verdicts::secret_verdict(&report, exceptions, &paths, &regions, &bounds)?;
    verdict.put("projectionSpans", regions);
    Ok(verdict)
}

/// Do `work` as the phase `name` and keep a receipt of it in `output`:
/// whether it passed, the code it failed with, the time it took, and
/// whether `cancelled` was set by then. A receipt that cannot be kept is
/// answered in place of the work's own answer.
pub fn scan_phase<T>(
    output: &Path,
    cancelled: &AtomicBool,
    name: &str,
    work: impl FnOnce() -> Result<T>,
) -> Result<T> {
    let started = Instant::now();
    let answer = work();
    let failure = answer
        .as_ref()
        .err()
        .map_or(Document::Null, |fault| fault.0.into());
    let seconds = (started.elapsed().as_secs_f64() * 1000.0).round() / 1000.0;
    let seconds = serde_json::Number::from_f64(seconds).map_or(Document::Null, Document::Number);
    let receipt = Document::record([
        ("schemaVersion", 1_u64.into()),
        ("phase", name.into()),
        ("passed", answer.is_ok().into()),
        ("failure", failure),
        ("elapsedSeconds", seconds),
        ("cancelled", cancelled.load(Ordering::SeqCst).into()),
    ]);
    written(
        &output.join(format!("scanner-result-phase-{name}.json")),
        &receipt,
    )?;
    answer
}

/// What the native evidence is held to: the reviewed record of the native
/// components and the digests of that record and of the lock.
#[derive(Clone, Copy, Debug)]
pub struct Reviewed<'a> {
    /// The reviewed record of the native components.
    pub native: &'a Document,
    /// Its SHA-256.
    pub native_sha256: &'a str,
    /// The SHA-256 of the lock of the Rust dependencies.
    pub lock_sha256: &'a str,
}

/// The build-time native evidence the image in `tree` ships, once it
/// describes the executable that `elf` audited, byte for byte, and was made
/// from the reviewed native record and lock, with every inventory it must
/// hold and the OpenSSL and AWS-LC builds that `reviewed` records.
pub fn native_binding(
    tree: &Path,
    elf: &Document,
    reviewed: &Reviewed<'_>,
    limits: &Limits,
) -> Result<Document> {
    let name = "/usr/share/simplestchat/native-components.json";
    let path = elf::image_path(&tree.join("rootfs"), name)
        .map_err(|stop| stopped(stop, "tool_entry_missing", "tool_entry_unavailable"))?;
    let native = policy::report(&path, limits.native)?;
    let shape = Fault("image_native_evidence_shape");
    let binary = native
        .get("binary")
        .filter(|binary| binary.fields().is_some())
        .ok_or(shape)?;
    let same = binary.get("sha256") == elf.get("binarySha256")
        && binary.get("size") == elf.get("binaryBytes");
    require(same, "image_native_binary_mismatch")?;
    let text = |name: &str| native.get(name).and_then(Document::text);
    let sources = text("native_manifest_sha256") == Some(reviewed.native_sha256)
        && text("cargo_lock_sha256") == Some(reviewed.lock_sha256);
    require(sources, "image_native_source_mismatch")?;
    let listed = |name: &str| {
        native
            .get(name)
            .and_then(Document::items)
            .is_some_and(|items| !items.is_empty())
    };
    let registry = native.get("registry_component").and_then(Document::fields);
    let held = listed("static_archives")
        && listed("wrap_components")
        && registry.is_some_and(|fields| !fields.is_empty());
    require(held, "image_native_inventory_missing")?;
    let expected = |name: &str| {
        reviewed
            .native
            .get(name)
            .filter(|value| value.fields().is_some())
            .ok_or(shape)
    };
    bindings::openssl_build_binding(&native, expected("openssl")?)?;
    bindings::aws_lc_build_binding(&native, expected("registry_component")?)?;
    Ok(native)
}

/// What a disposition is decided from and published with.
pub struct Disposition<'a> {
    /// The release manifest of the image.
    pub manifest: &'a Document,
    /// The image.
    pub image_id: &'a str,
    /// Where the evidence goes.
    pub output: &'a Path,
    /// The digest of the reviewed advisory policy.
    pub policy: &'a str,
    /// When it is decided.
    pub now: Moment,
}

/// Publish the disposition of `vulnerabilities` before any verdict passes:
/// the runtime proof that `prove` makes from the digest of the SPDX
/// conversion when a reviewed package is affected, or the explicit absence
/// of a claim when none is; and the VEX statement a proof that is required
/// supports. `outcome` records the findings, whether the proof was required
/// and the digests of both files. Answers the findings, disposed as the
/// proof allows.
pub fn runtime_disposition(
    vulnerabilities: Document,
    disposition: &Disposition<'_>,
    outcome: &mut Document,
    prove: &mut dyn FnMut(&str) -> Result<Document>,
) -> Result<Document> {
    outcome.put("vulnerabilityFindings", vulnerabilities.clone());
    let affected = runtime::affected_scopes(&vulnerabilities, disposition.policy)?;
    let (proof, mut vex) = if affected.is_empty() {
        runtime::not_required(
            disposition.manifest,
            disposition.image_id,
            &vulnerabilities,
            disposition.policy,
            disposition.now,
        )?
    } else {
        let conversion = evidence_digest(&disposition.output.join("spdx/sbom.spdx.json"))?;
        (prove(&conversion)?, Document::Record(Vec::new()))
    };
    let path = disposition.output.join("runtime-proof.json");
    written(&path, &proof)?;
    let mut vulnerabilities = vulnerabilities;
    let required = proof.get("required").cloned().unwrap_or(Document::Null);
    if required == Document::Bool(true) {
        let applied = runtime::apply(
            &vulnerabilities,
            &proof,
            &path,
            disposition.policy,
            disposition.now,
            &runtime::Limits::REVIEWED,
        )?;
        (vulnerabilities, vex) = applied;
    }
    let statement = disposition.output.join("vex.openvex.json");
    written(&statement, &vex)?;
    outcome.put("vexRequired", required);
    outcome.put("runtimeProofSha256", evidence_digest(&path)?.into());
    outcome.put("vexSha256", evidence_digest(&statement)?.into());
    Ok(vulnerabilities)
}

#[cfg(test)]
#[path = "scan_tests.rs"]
mod tests;
