//! The image gate as a whole: the saved image bound to its export and
//! extracted, its executable audited, the scanners prepared in their
//! sandbox and proved, the three independent branches scanned side by side
//! and the database matched against the catalog, then every verdict and
//! the evidence each one rests on recorded in one honest outcome, written
//! whatever failed; and the database's own preparation, of no image.

use crate::{
    Fault, Result,
    document::Document,
    files,
    image::{
        advisories, bindings, cache, licenses, notices,
        policy::{self, Moment},
        runtime,
        sandbox::{self, Cache, Engine, Platform, Sandbox, Scan, Setup},
        scan::{self, Disposition, Limits, Pins, Reviewed},
    },
    policy::{Date, Exception},
    require, secrets,
};
use std::{
    ffi::OsString,
    fs::DirBuilder,
    os::unix::fs::DirBuilderExt,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

/// The lock of the scanners, below the checkout.
pub const LOCK: &str = "build/security-tools.lock.json";
/// The image policy, below the checkout.
pub const IMAGE_POLICY: &str = "security/image-policy.json";
/// The reviews of findings, below the checkout.
pub const EXCEPTIONS: &str = "security/exceptions.json";
/// The reviewed record of the native components, below the checkout.
pub const NATIVE: &str = "vendor/native-components.json";
/// The lock of the Rust dependencies, below the checkout.
pub const CARGO_LOCK: &str = "Cargo.lock";

/// What the gate is asked to judge, and where its evidence goes.
#[derive(Clone, Debug)]
pub struct Options {
    /// The image, `sha256:` and its digest.
    pub image_id: String,
    /// The directory the image was saved to, with the producer's record.
    pub artifact: PathBuf,
    /// The new directory of the evidence.
    pub output: PathBuf,
    /// The directory of the verified scanners.
    pub tools: PathBuf,
    /// The platform the image is for.
    pub platform: Platform,
    /// The database cache that seeds the update, when there is one.
    pub database_cache: Option<PathBuf>,
}

/// What the database's preparation is asked for.
#[derive(Clone, Debug)]
pub struct Preparation {
    /// The new directory of the evidence.
    pub output: PathBuf,
    /// The directory the scanners are installed in.
    pub tools: PathBuf,
    /// The platform whose scanners are installed.
    pub platform: Platform,
    /// The database cache that is published.
    pub database_cache: Option<PathBuf>,
}

/// What the checkout holds that the gate is held to, read before it starts.
#[derive(Clone, Debug)]
pub struct Checkout {
    /// The checkout itself, which the runtime proof reads.
    pub root: PathBuf,
    /// The pins of the scanners, with the digest of their lock.
    pub pins: Pins,
    /// The image policy.
    pub image_policy: Document,
    /// Its SHA-256.
    pub policy_sha256: String,
    /// The reviews of findings, every one current.
    pub exceptions: Vec<Exception>,
    /// The SHA-256 of their file.
    pub exceptions_sha256: String,
    /// The reviewed record of the native components.
    pub native: Document,
    /// Its SHA-256.
    pub native_sha256: String,
    /// The SHA-256 of the lock of the Rust dependencies.
    pub cargo_lock_sha256: String,
    /// The reviewed advisory policy.
    pub advisory: PathBuf,
    /// Its SHA-256.
    pub advisory_sha256: String,
    /// The pin the database cache is kept under.
    pub cache_pin: String,
}

/// The pin of the vulnerability scanner in the lock below `root`, once the
/// lock is one, which separates its database downloads from another
/// scanner's.
fn cache_pin(root: &Path) -> Result<String> {
    let path = root.join(LOCK);
    crate::tools::locked(&path, "grype")?;
    let lock = policy::report(&path, Limits::REVIEWED.report)?;
    let grype = lock.get("tools").and_then(|tools| tools.get("grype"));
    Ok(cache::pin(grype.ok_or(Fault("unknown_tool"))?))
}

impl Checkout {
    /// What the checkout at `root` holds, its reviews current on `today`.
    pub fn read(root: &Path, today: Date) -> Result<Self> {
        let lock = root.join(LOCK);
        let pinned = |name: &str| crate::tools::locked(&lock, name).map(|(version, _)| version);
        let pins = Pins {
            lock: scan::evidence_digest(&lock)?,
            syft: pinned("syft")?,
            grype: pinned("grype")?,
            gitleaks: pinned("gitleaks")?,
        };
        let native = root.join(NATIVE);
        let advisory = root.join(advisories::POLICY_PATH);
        Ok(Self {
            root: root.to_path_buf(),
            pins,
            image_policy: policy::load_policy(&root.join(IMAGE_POLICY))?,
            policy_sha256: scan::evidence_digest(&root.join(IMAGE_POLICY))?,
            exceptions: crate::policy::read(&root.join(EXCEPTIONS), today, false)?,
            exceptions_sha256: scan::evidence_digest(&root.join(EXCEPTIONS))?,
            native: policy::report(&native, Limits::REVIEWED.report)?,
            native_sha256: scan::evidence_digest(&native)?,
            cargo_lock_sha256: scan::evidence_digest(&root.join(CARGO_LOCK))?,
            advisory_sha256: scan::evidence_digest(&advisory)?,
            advisory,
            cache_pin: cache_pin(root)?,
        })
    }
}

/// What the gate asks of the machine it runs on: the two steps it runs
/// beside its scans, the engine and its scanners, the runtime proof, the
/// clock, the caller's environment and the cancellation it observes.
pub trait Host {
    /// Extract the saved image `archive`, which the release manifest at
    /// `manifest` names, into the new directory `tree`, with its report.
    fn extract(&self, archive: &Path, manifest: &Path, tree: &Path) -> Result<()>;

    /// Audit the executable of the image whose files are below `rootfs`,
    /// for `platform`, into the new file `report`, which is written whether
    /// or not the audit passed; refused when it did not.
    fn audit(&self, rootfs: &Path, platform: Platform, report: &Path) -> Result<()>;

    /// The command that runs the engine, found before anything of the
    /// sandbox is made.
    fn prefix(&self) -> Result<Vec<OsString>>;

    /// What runs the engine through `prefix`, in `environment`, with its
    /// command log in `logs`.
    fn engine(
        &self,
        prefix: &[OsString],
        environment: Vec<(&'static str, OsString)>,
        logs: PathBuf,
    ) -> Box<dyn Engine>;

    /// Install the image tier's scanners for the lock's platform `platform`
    /// as the directory `directory`, or verify the one there, and answer it.
    fn install(&self, platform: &'static str, directory: &Path) -> Result<PathBuf>;

    /// Where the verified executable of the scanner `name` is in
    /// `directory`, for the lock's platform `platform`.
    fn locate(
        &self,
        name: &'static str,
        platform: &'static str,
        directory: &Path,
    ) -> Result<PathBuf>;

    /// The runtime proof of what `evidence` binds.
    fn prove(&self, evidence: &runtime::Evidence<'_>) -> Result<Document>;

    /// The caller's environment variable `name`.
    fn ambient(&self, name: &str) -> Option<OsString>;

    /// What the clock says now.
    fn now(&self) -> Moment;

    /// The cancellation the gate observes: set, every scanner it starts is
    /// stopped and removed by its identity.
    fn cancelled(&self) -> Arc<AtomicBool>;
}

/// Make the new directory `path`, which only its owner may use.
fn private(path: &Path) -> Result<()> {
    let made = DirBuilder::new().mode(0o700).create(path);
    made.map_err(|error| files::unusable(&error))
}

/// The path `path` resolved, through every link it names.
fn resolved(path: &Path) -> Result<PathBuf> {
    path.canonicalize().map_err(|error| files::unusable(&error))
}

/// What the three branches scan.
pub struct Branches<'a> {
    /// The saved image's directory.
    pub artifact: &'a Path,
    /// The extracted archive.
    pub tree: &'a Path,
    /// The native evidence.
    pub native: &'a Document,
    /// The audit of the executable.
    pub elf: &'a Document,
    /// The image policy.
    pub image_policy: &'a Document,
    /// The pins of the scanners.
    pub pins: &'a Pins,
    /// The bounds of the scans.
    pub limits: &'a Limits,
    /// When the database's freshness is judged.
    pub now: Moment,
    /// The space left on the disk that holds a path.
    pub free: &'a (dyn Fn(&Path) -> Result<u64> + Sync),
}

/// Where the reports of the scans are, and the database's status.
#[derive(Debug)]
pub struct Reports {
    /// The enriched catalog.
    pub sbom: PathBuf,
    /// The vulnerability match.
    pub grype: PathBuf,
    /// The secret scan.
    pub secrets: PathBuf,
    /// The database's status.
    pub database: Document,
}

/// The answer of a branch that ended: a panic cancels the others.
fn joined<T>(answer: std::thread::Result<Result<T>>, cancelled: &AtomicBool) -> Result<T> {
    answer.unwrap_or_else(|_| {
        cancelled.store(true, Ordering::SeqCst);
        Err(Fault("image_parallel_scan_failed"))
    })
}

/// Scan the three independent branches side by side within the sandbox's
/// slots — the database, the catalog and the secrets, each with its own
/// command log, evidence and share of the scanners — then match the
/// database against the catalog. A failure of one branch does not stop the
/// others, and no match is made after any; a cancellation is named first.
pub fn scan_reports(sandbox: &mut Sandbox, branches: &Branches<'_>) -> Result<Reports> {
    // The eight scanners are shared out: the detector's self-test and the
    // final match run on this sandbox, five more on the branches.
    sandbox.allowance = sandbox.limits.scanners.saturating_sub(5);
    require(sandbox.sequence < sandbox.allowance, "image_scanner_count")?;
    let mut database = sandbox.branch("database", 2)?;
    let mut sbom = sandbox.branch("sbom", 2)?;
    let mut secrets = sandbox.branch("secrets", 1)?;
    let output = sandbox.output.clone();
    let cancelled = Arc::clone(&sandbox.cancelled);
    let (prepared, cataloged, found) = std::thread::scope(|scope| {
        let prepared = scope.spawn(|| {
            scan::scan_phase(&output, &cancelled, "database", || {
                let (policy, now) = (branches.image_policy, branches.now);
                scan::prepare_database(&mut database, policy, now, branches.limits)
            })
        });
        let found = scope.spawn(|| {
            scan::scan_phase(&output, &cancelled, "secrets", || {
                scan::scan_secrets(&mut secrets, branches.tree, branches.limits, branches.free)
            })
        });
        let cataloged = scope.spawn(|| {
            scan::scan_phase(&output, &cancelled, "sbom", || {
                let Branches {
                    artifact,
                    native,
                    elf,
                    image_policy,
                    pins,
                    limits,
                    ..
                } = *branches;
                scan::scan_sbom(&mut sbom, artifact, native, elf, image_policy, pins, limits)
            })
        });
        (
            joined(prepared.join(), &cancelled),
            joined(cataloged.join(), &cancelled),
            joined(found.join(), &cancelled),
        )
    });
    require(!sandbox.cancelled(), "image_scan_cancelled")?;
    let failed = prepared.is_err() || cataloged.is_err() || found.is_err();
    require(!failed, "image_parallel_scan_failed")?;
    let (database, catalog, secret) = (prepared?, cataloged?, found?);
    let grype = sandbox.output.join("vulnerabilities");
    let fetched = sandbox.output.join("database");
    let mounts = [("/input", catalog.as_path()), ("/db", fetched.as_path())];
    let (status, _) = sandbox.run(&Scan {
        tool: "grype",
        arguments: &[
            "sbom:/input/sbom.syft.json",
            "-o",
            "json",
            "--file",
            "/output/grype.json",
        ],
        destination: &grype,
        mounts: &mounts,
        online: false,
        variables: &[("GRYPE_DB_CACHE_DIR", "/db/cache")],
    })?;
    require(status == 0, "image_vulnerability_scan_failed")?;
    Ok(Reports {
        sbom: catalog,
        grype,
        secrets: secret,
        database,
    })
}

/// The sandbox of the scanners for `output`: the engine found, its scanners
/// located in `tools` for `platform`, with the database cache when there
/// is one.
fn sandboxed(
    host: &dyn Host,
    output: &Path,
    tools: &Path,
    platform: Platform,
    base: &str,
    cache: Option<Cache>,
) -> Result<Sandbox> {
    let prefix = host.prefix()?;
    let mut locate =
        |name: &'static str, platform: &'static str| host.locate(name, platform, tools);
    let setup = Setup {
        output,
        ambient: &|name: &str| host.ambient(name),
        locate: &mut locate,
        platform,
        base,
        cancelled: host.cancelled(),
        cache,
        limits: sandbox::Limits::REVIEWED,
    };
    sandbox::prepare(setup, &mut |environment, logs| {
        host.engine(&prefix, environment, logs)
    })
}

/// The reference of the scanner base that `image_policy` reviewed.
fn scanner_base(image_policy: &Document) -> Result<&str> {
    let base = image_policy.get("scannerBase").and_then(Document::text);
    base.ok_or(Fault("image_policy_fields"))
}

/// What the evidence says the secret scan covers and how far.
fn coverage(limits: &Limits) -> Document {
    let limitations = secrets::LIMITATIONS.iter().map(|item| (*item).into());
    Document::record([
        ("format", secrets::FORMAT.into()),
        ("limitations", Document::List(limitations.collect())),
        ("maximumSourceFileBytes", limits.member.into()),
        ("maximumProjectionBytes", limits.projected_bytes.into()),
        ("maximumRegularFiles", limits.projected_files.into()),
        ("diskReserveBytes", limits.reserve.into()),
        ("projectionPrefixBytes", secrets::PREFIX_BYTES.into()),
    ])
}

/// What the evidence says the scan of the native components covers.
fn native_coverage() -> Document {
    Document::record([
        (
            "inventory",
            "Authenticated static build inputs included in SBOM".into(),
        ),
        (
            "matching",
            "No inferred CPE applicability for adapted or static components".into(),
        ),
        (
            "limit",
            "Package scanning does not prove native libraries are vulnerability-free".into(),
        ),
    ])
}

/// One judgement of the gate: what it was asked, what it is held to, the
/// manifest of the image, where the evidence goes and the machine.
struct Judged<'a> {
    options: &'a Options,
    checkout: &'a Checkout,
    manifest: &'a Document,
    output: &'a Path,
    host: &'a dyn Host,
}

/// The text the manifest has under `name`.
fn manifested<'a>(manifest: &'a Document, name: &str) -> Result<&'a str> {
    let text = manifest.get(name).and_then(Document::text);
    text.ok_or(Fault("release_manifest_invalid"))
}

/// Extract the saved image into `tree`, audit its executable and bind the
/// native evidence to it; answer the audit and the evidence.
fn audited(gate: &Judged<'_>, tree: &Path, outcome: &mut Document) -> Result<(Document, Document)> {
    let Judged {
        options,
        checkout,
        output,
        host,
        ..
    } = *gate;
    let artifact = &options.artifact;
    host.extract(
        &artifact.join("image.tar"),
        &artifact.join("release.json"),
        tree,
    )?;
    let report = output.join("elf.json");
    host.audit(&tree.join("rootfs"), options.platform, &report)?;
    let elf = policy::report(&report, Limits::REVIEWED.report)?;
    let reviewed = Reviewed {
        native: &checkout.native,
        native_sha256: &checkout.native_sha256,
        lock_sha256: &checkout.cargo_lock_sha256,
    };
    let native = scan::native_binding(tree, &elf, &reviewed, &Limits::REVIEWED)?;
    scan::written(&output.join("native.json"), &native)?;
    let binary = elf.get("binarySha256").cloned();
    outcome.put(
        "binarySha256",
        binary.ok_or(Fault("image_native_binary_mismatch"))?,
    );
    Ok((elf, native))
}

/// Every verdict on the scanned image and the evidence each rests on.
fn concluded(
    gate: &Judged<'_>,
    tree: &Path,
    (elf, native): (&Document, &Document),
    reports: &Reports,
    outcome: &mut Document,
) -> Result<()> {
    let Judged {
        options,
        checkout,
        manifest,
        output,
        host,
    } = *gate;
    let limits = Limits::REVIEWED;
    let image_policy = &checkout.image_policy;
    policy::database_status(&reports.database, image_policy, host.now())?;
    let catalog = policy::report(&reports.sbom.join("sbom.syft.json"), limits.report)?;
    let bounds = policy::Limits::REVIEWED;
    let packages = policy::inventory(&catalog, image_policy, &checkout.pins.syft, &bounds)?;
    let conversion = output.join("spdx/sbom.spdx.json");
    let image = notices::Image {
        image_id: &options.image_id,
        archive_sha256: manifested(manifest, "archiveSha256")?,
        sbom_sha256: &scan::evidence_digest(&conversion)?,
        revision: manifested(manifest, "revision")?,
        platform: options.platform.name(),
    };
    let rootfs = tree.join("rootfs");
    let notices = notices::collect(&rootfs, &packages, &image, &notices::Limits::REVIEWED)?;
    let evidence = output.join("runtime-license-evidence.json");
    scan::written(&evidence, &notices)?;
    let digest = scan::evidence_digest(&evidence)?;
    outcome.put("runtimeLicenseEvidenceSha256", digest.into());
    let intact = notices.get("passed") == Some(&Document::Bool(true));
    require(intact, "image_runtime_notice_integrity")?;
    let grype = policy::report(&reports.grype.join("grype.json"), limits.report)?;
    policy::vulnerability_database_binding(&grype, &reports.database)?;
    let rpms = bindings::runtime_rpm_bindings(&packages, elf, &rootfs)?;
    outcome.put("runtimeRpms", Document::List(rpms));
    let exceptions = &checkout.exceptions;
    let pins = &checkout.pins;
    let vulnerabilities = policy::vulnerability_verdict(
        &grype,
        exceptions,
        &packages,
        &pins.grype,
        &checkout.advisory,
        &bounds,
    )?;
    let disposition = Disposition {
        manifest,
        image_id: &options.image_id,
        output,
        policy: &checkout.advisory_sha256,
        now: host.now(),
    };
    let mut prove = |sbom_sha256: &str| {
        host.prove(&runtime::Evidence {
            tree,
            elf,
            native,
            manifest,
            root: &checkout.root,
            sbom_sha256,
            advisory_policy_sha256: &checkout.advisory_sha256,
        })
    };
    let vulnerabilities =
        scan::runtime_disposition(vulnerabilities, &disposition, outcome, &mut prove)?;
    let licenses = licenses::license_verdict(&packages, image_policy, exceptions)?;
    let secrets = scan::secret_checks(&reports.secrets, output, exceptions, pins, &limits)?;
    let checks = Document::record([
        ("vulnerabilities", vulnerabilities),
        ("licenses", licenses),
        ("secrets", secrets),
    ]);
    scan::written(&output.join("checks.json"), &checks)?;
    let passed = checks.fields().is_some_and(|fields| {
        fields
            .iter()
            .all(|(_, verdict)| verdict.get("passed") == Some(&Document::Bool(true)))
    });
    outcome.put("checks", checks);
    for (name, file) in [
        ("secretPathMapSha256", "secret-paths.json"),
        ("databaseEvidenceSha256", "database-status.json"),
        ("sbomSha256", "spdx/sbom.spdx.json"),
        ("nativeSha256", "native.json"),
        ("elfSha256", "elf.json"),
    ] {
        outcome.put(name, scan::evidence_digest(&output.join(file))?.into());
    }
    outcome.put("nativeVulnerabilityCoverage", native_coverage());
    outcome.put("passed", passed.into());
    Ok(())
}

/// Judge the image as `gate` says, recording in `outcome` what it found.
fn judge(gate: &Judged<'_>, outcome: &mut Document) -> Result<()> {
    let Judged {
        options,
        checkout,
        output,
        host,
        ..
    } = *gate;
    let tree = output.join("archive");
    let (elf, native) = audited(gate, &tree, outcome)?;
    let cache = options.database_cache.as_ref().map(|directory| Cache {
        directory: directory.clone(),
        pin: checkout.cache_pin.clone(),
        limits: cache::Limits::REVIEWED,
    });
    let base = scanner_base(&checkout.image_policy)?;
    let mut sandbox = sandboxed(host, output, &options.tools, options.platform, base, cache)?;
    let limits = Limits::REVIEWED;
    let selected = scan::selected_image(
        &mut sandbox,
        &options.image_id,
        options.platform,
        &tree,
        &limits,
    )?;
    outcome.put("selectedImage", selected);
    scan::secret_selftest(&mut sandbox, &checkout.pins, &limits, &secrets::free)?;
    outcome.put("secretDetectorSelfTest", true.into());
    outcome.put("secretCoverage", coverage(&limits));
    let branches = Branches {
        artifact: &options.artifact,
        tree: &tree,
        native: &native,
        elf: &elf,
        image_policy: &checkout.image_policy,
        pins: &checkout.pins,
        limits: &limits,
        now: host.now(),
        free: &secrets::free,
    };
    let reports = scan_reports(&mut sandbox, &branches)?;
    concluded(gate, &tree, (&elf, &native), &reports, outcome)
}

/// Judge the image that `options` names against `checkout` on `host`, and
/// write the outcome to the new directory of the evidence whatever failed
/// once it exists: its verdicts, the evidence each rests on, and the code of
/// the first failure. Answers whether every check passed; refused before
/// any evidence when the image is not the exact export the producer
/// recorded as passing.
pub fn execute(options: &Options, checkout: &Checkout, host: &dyn Host) -> Result<bool> {
    let manifest = scan::bind_archive(
        &options.image_id,
        &options.artifact,
        options.platform,
        &Limits::REVIEWED,
    )?;
    private(&options.output)?;
    let output = resolved(&options.output)?;
    let field = |name: &str| manifest.get(name).cloned().unwrap_or(Document::Null);
    let mut outcome = Document::record([
        ("schemaVersion", 1_u64.into()),
        ("passed", false.into()),
        ("imageId", options.image_id.as_str().into()),
        ("revision", field("revision")),
        ("archiveSha256", field("archiveSha256")),
        ("platform", options.platform.name().into()),
        ("toolLockSha256", checkout.pins.lock.as_str().into()),
        ("policySha256", checkout.policy_sha256.as_str().into()),
        (
            "exceptionsSha256",
            checkout.exceptions_sha256.as_str().into(),
        ),
    ]);
    let gate = Judged {
        options,
        checkout,
        manifest: &manifest,
        output: &output,
        host,
    };
    if let Err(fault) = judge(&gate, &mut outcome) {
        outcome.put("error", fault.0.into());
    }
    scan::written(&output.join("outcome.json"), &outcome)?;
    Ok(outcome.get("passed") == Some(&Document::Bool(true)))
}

/// Install the scanners and prepare the database alone, as `preparation`
/// says, recording in `outcome` the digest of its evidence.
fn prepared(
    preparation: &Preparation,
    root: &Path,
    output: &Path,
    host: &dyn Host,
    outcome: &mut Document,
) -> Result<()> {
    let cancelled = host.cancelled();
    let tools = host.install(preparation.platform.tools(), &preparation.tools)?;
    require(!cancelled.load(Ordering::SeqCst), "image_scan_cancelled")?;
    let image_policy = policy::load_policy(&root.join(IMAGE_POLICY))?;
    let cache = match &preparation.database_cache {
        Some(directory) => Some(Cache {
            directory: directory.clone(),
            pin: cache_pin(root)?,
            limits: cache::Limits::REVIEWED,
        }),
        None => None,
    };
    let base = scanner_base(&image_policy)?;
    let mut sandbox = sandboxed(host, output, &tools, preparation.platform, base, cache)?;
    sandbox.allowance = 2;
    let now = host.now();
    scan::scan_phase(output, &cancelled, "database", || {
        scan::prepare_database(&mut sandbox, &image_policy, now, &Limits::REVIEWED)
    })?;
    require(!cancelled.load(Ordering::SeqCst), "image_scan_cancelled")?;
    let digest = scan::evidence_digest(&output.join("database-status.json"))?;
    outcome.put("databaseEvidenceSha256", digest.into());
    outcome.put("passed", true.into());
    Ok(())
}

/// Prepare only the vulnerability database, on an engine of its own with no
/// image and no verdict, and publish it to the cache: the scanners installed
/// for the checkout at `root` as `preparation` says, the database updated,
/// held to the image policy and saved. The record of the preparation is
/// written to the new directory of the evidence whatever failed, with
/// whether it was cancelled. Answers whether it passed uncancelled; refused
/// before any evidence unless every path is absolute.
pub fn execute_database(preparation: &Preparation, root: &Path, host: &dyn Host) -> Result<bool> {
    let cache = preparation.database_cache.as_ref();
    let absolute = preparation.output.is_absolute()
        && preparation.tools.is_absolute()
        && cache.is_some_and(|cache| cache.is_absolute());
    require(absolute, "image_database_preparation_paths")?;
    private(&preparation.output)?;
    let output = resolved(&preparation.output)?;
    let mut outcome = Document::record([("schemaVersion", 1_u64.into()), ("passed", false.into())]);
    if let Err(fault) = prepared(preparation, root, &output, host, &mut outcome) {
        outcome.put("error", fault.0.into());
    }
    let cancelled = host.cancelled().load(Ordering::SeqCst);
    outcome.put("cancelled", cancelled.into());
    if cancelled {
        outcome.put("passed", false.into());
    }
    scan::written(&output.join("database-preparation.json"), &outcome)?;
    Ok(outcome.get("passed") == Some(&Document::Bool(true)) && !cancelled)
}

#[cfg(test)]
#[path = "gate_tests.rs"]
mod tests;
