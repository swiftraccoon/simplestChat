//! `simplestchat-ops release admit-unsigned`: retain one release artifact
//! that no signature covers, because an operator explicitly asked for it.
//! The request names the exact bytes; the artifact must be a passed build of
//! the revision it names, with a valid manifest and image archive. Admission
//! publishes those bytes where the ordinary staging and deployment commands
//! read them and records that nothing attested them. It verifies no
//! signature, loads no image and replaces nothing that is already retained.
//!
//! `authorize` is the controller's half: it checks an export against the
//! checkout that is about to be deployed and writes the request.

#[cfg(any(test, feature = "testing"))]
#[path = "admit_fixture.rs"]
pub mod fixture;
#[cfg(test)]
#[path = "admit_tests.rs"]
mod tests;

use crate::host::command::Local;
use crate::host::fault::{DIRECTORY, Fault, Kind, Result, VALUE, decode, file, protected, require};
use crate::host::{Host, Paths, System, emit, hexadecimal, state};
use crate::json::Json;
use crate::release::archive::{MAX_ARCHIVE_BYTES, verify_archive};
use crate::release::assets::{self, Takeover, Workload};
use crate::release::manifest::{MAX_BYTES, Manifest};
use rustix::fs::{AtFlags, CWD, Mode, OFlags, linkat, open};
use serde::Serialize;
use sha2::{Digest, Sha256, Sha384};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::File;
use std::io::Read;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// What a build exports, in the order it is retained.
const FILES: [&str; 4] = ["image.tar", "release.json", "source.json", "outcome.json"];
/// The source files whose digests bind an export to its checkout.
const SOURCE_INPUTS: [&str; 5] = [
    "Dockerfile",
    ".dockerignore",
    "Cargo.lock",
    "web/package-lock.json",
    "build/pip-constraints.txt",
];
const REQUEST_KEYS: [&str; 9] = [
    "schemaVersion",
    "operation",
    "forced",
    "githubAttested",
    "ciVerification",
    "repository",
    "revision",
    "fileDigests",
    "sourceInputsSha256",
];
const REQUEST: &str = "force.json";
const UNVERIFIED: &str = "skipped-explicit-force";
const MAX_JSON: u64 = MAX_BYTES as u64;
const UNFINISHED: &str = "Inspect and recover the unfinished release before starting another";
const FAILED: &str = "{\"passed\":false,\"failure\":\"force_release_failed\"}\n";

/// The most bytes the file of this name may hold.
fn limit(name: &str) -> u64 {
    match name {
        "image.tar" => MAX_ARCHIVE_BYTES,
        _ => MAX_JSON,
    }
}

/// Require an ordinary path of `owner`. A link is refused as a link, whatever
/// mode its system gives it.
fn guarded(owner: u32, path: &Path, kind: Kind) -> Result<()> {
    if std::fs::symlink_metadata(path)?.is_symlink() {
        return Err(Fault::Refused(format!(
            "Unexpected link: {}",
            path.display()
        )));
    }
    protected(owner, path, kind)
}

/// The object in one file that was admitted as bounded and ordinary. An
/// empty file holds none.
fn read_object(path: &Path) -> Result<Json> {
    let empty = std::fs::symlink_metadata(path)?.len() == 0;
    require(!empty, "invalid_force_metadata")?;
    let value = decode(&state::read(path, MAX_BYTES)?)?;
    value.object().map(|_| value.clone()).ok_or(VALUE)
}

/// The SHA-256 of one file's bytes. A link is not followed.
fn sha256(path: &Path) -> Result<String> {
    let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut file = File::from(open(path, flags, Mode::empty())?);
    let mut hash = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    let digest = hash.finalize();
    Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
}

/// What an explicit unsigned request names.
struct Request {
    repository: String,
    revision: String,
    /// The digest of each exported file.
    files: BTreeMap<String, String>,
    /// The digest of each source input of the checkout that built them.
    inputs: BTreeMap<String, String>,
}

/// Whether `text` is an owner and a repository name as GitHub spells them.
fn repository(text: &str) -> bool {
    let plain = |byte: &u8| byte.is_ascii_alphanumeric();
    let Some((owner, name)) = text.split_once('/') else {
        return false;
    };
    let (owner, name) = (owner.as_bytes(), name.as_bytes());
    owner.len() <= 39
        && owner.first().is_some_and(plain)
        && owner.iter().all(|byte| plain(byte) || *byte == b'-')
        && name.len() <= 100
        && name.first().is_some_and(plain)
        && name.iter().all(|byte| plain(byte) || b"_.-".contains(byte))
}

/// The digests of exactly `names`, or the refusal for anything else.
fn digests(value: &Json, names: &[&str]) -> Result<BTreeMap<String, String>> {
    let entries = value.object().ok_or(VALUE)?;
    let named =
        entries.len() == names.len() && names.iter().all(|name| entries.contains_key(*name));
    let digest = |value: &Json| value.string().is_some_and(|text| hexadecimal(text, 64));
    require(
        named && entries.values().all(digest),
        "invalid_force_digests",
    )?;
    let pairs = entries.iter().filter_map(|(name, value)| {
        let digest = value.string()?;
        Some((name.clone(), digest.to_owned()))
    });
    Ok(pairs.collect())
}

/// Require the complete explicit authorization of unsigned bytes: nothing
/// more, nothing less, and no claim that anything verified them.
fn validate(request: &Json) -> Result<Request> {
    let fields = request.object().ok_or(VALUE)?;
    let text = |name: &str| fields.get(name).and_then(Json::string);
    let exact = fields.len() == REQUEST_KEYS.len()
        && REQUEST_KEYS.iter().all(|key| fields.contains_key(*key));
    let current =
        matches!(fields.get("schemaVersion"), Some(Json::Integer(number)) if number == "1");
    let unsigned = matches!(fields.get("forced"), Some(Json::Bool(true)))
        && matches!(fields.get("githubAttested"), Some(Json::Bool(false)))
        && text("ciVerification") == Some(UNVERIFIED);
    require(
        exact && current && text("operation") == Some("force-release") && unsigned,
        "invalid_force_authorization",
    )?;
    let revision = text("revision").ok_or(VALUE)?;
    require(hexadecimal(revision, 40), "invalid_force_revision")?;
    let named = text("repository").ok_or(VALUE)?;
    require(repository(named), "invalid_force_repository")?;
    let files = digests(&fields["fileDigests"], &FILES)?;
    let inputs = digests(&fields["sourceInputsSha256"], &SOURCE_INPUTS)?;
    Ok(Request {
        repository: named.to_owned(),
        revision: revision.to_owned(),
        files,
        inputs,
    })
}

/// The digest of each exported file, each a bounded ordinary file with
/// content. A link is not followed.
fn member_digests(directory: &Path) -> Result<BTreeMap<String, String>> {
    let mut digests = BTreeMap::new();
    for name in FILES {
        let path = directory.join(name);
        let information = std::fs::symlink_metadata(&path)?;
        let bounded = information.is_file() && (1..=limit(name)).contains(&information.len());
        require(bounded, "invalid_force_artifact_file")?;
        digests.insert(name.to_owned(), sha256(&path)?);
    }
    Ok(digests)
}

/// Require the exported files to be the named bytes of a passed build of the
/// named revision from the named source inputs, with a valid manifest and an
/// image archive that manifest describes. Returns that manifest.
fn validate_artifact(directory: &Path, request: &Request) -> Result<Manifest> {
    let actual = member_digests(directory)?;
    require(actual == request.files, "force_artifact_digest_mismatch")?;
    let source = read_object(&directory.join("source.json"))?;
    let inputs = source.get("inputsSha256").and_then(Json::object);
    let same_inputs = inputs.is_some_and(|inputs| {
        let equal = |(name, digest): (&String, &String)| {
            inputs.get(name).and_then(Json::string) == Some(digest.as_str())
        };
        inputs.len() == request.inputs.len() && request.inputs.iter().all(equal)
    });
    let revision = |value: &Json| {
        value.get("revision").and_then(Json::string) == Some(request.revision.as_str())
    };
    require(
        source.object().is_some_and(|fields| fields.len() == 2) && revision(&source) && same_inputs,
        "force_source_evidence_mismatch",
    )?;
    let outcome = read_object(&directory.join("outcome.json"))?;
    let current =
        matches!(outcome.get("schemaVersion"), Some(Json::Integer(number)) if number == "1");
    let passed = matches!(outcome.get("passed"), Some(Json::Bool(true)))
        && matches!(outcome.get("error"), Some(Json::Null));
    require(
        current && revision(&outcome) && passed,
        "force_build_not_passed",
    )?;
    let described = Fault::Kind("ArtifactError");
    let manifest = state::read(&directory.join("release.json"), MAX_BYTES)?;
    let manifest = Manifest::decode(&manifest).map_err(|_| described.clone())?;
    require(
        manifest.fields().revision == request.revision,
        "force_revision_mismatch",
    )?;
    verify_archive(&directory.join("image.tar"), &manifest).map_err(|_| described)?;
    Ok(manifest)
}

/// Whether anything bears this name, a dangling link included.
fn present(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok()
}

/// Retain the artifact and this request under the revision's name. Bytes
/// already retained there are never replaced: they must be these bytes, and
/// an earlier request stays the retained one.
fn publish(host: &Host<'_>, directory: &Path, revision: &str) -> Result<()> {
    let parent = host.paths.root.join("releases");
    guarded(host.owner, &parent, DIRECTORY)?;
    let destination = parent.join(revision);
    match std::fs::DirBuilder::new().mode(0o700).create(&destination) {
        Err(error) if !destination.is_dir() => return Err(error.into()),
        _ => {}
    }
    guarded(host.owner, &destination, DIRECTORY)?;
    for name in FILES {
        let target = destination.join(name);
        if present(&target) {
            guarded(host.owner, &target, file(0o600, u64::MAX))?;
            let source = directory.join(name);
            let same = std::fs::metadata(&target)?.len() == std::fs::metadata(&source)?.len()
                && sha256(&target)? == sha256(&source)?;
            require(same, "retained_artifact_differs")?;
        }
    }
    if present(&destination.join(REQUEST)) {
        guarded(
            host.owner,
            &destination.join(REQUEST),
            file(0o600, u64::MAX),
        )?;
    }
    for name in FILES.into_iter().chain([REQUEST]) {
        let target = destination.join(name);
        if !target.exists() {
            linkat(CWD, directory.join(name), CWD, &target, AtFlags::empty())?;
        }
    }
    File::open(&destination)?.sync_all()?;
    Ok(())
}

/// What one admission did, as it is retained and printed.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Report<'a> {
    action: &'static str,
    revision: &'a str,
    repository: &'a str,
    forced: bool,
    github_attested: bool,
    ci_verification: &'static str,
    file_digests: &'a BTreeMap<String, String>,
    started_at: String,
    passed: bool,
    phase: &'static str,
    evidence: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    failure: Option<&'static str>,
    finished_at: String,
}

/// The present second, as retained records spell it.
fn timestamp(host: &Host<'_>) -> String {
    let now = host.clock.now().format("%Y-%m-%dT%H:%M:%SZ");
    now.to_string()
}

/// Validate and publish under the held workload lock, and retain what
/// happened. Returns the line to print for an admitted artifact.
fn admit(host: &Host<'_>, directory: &Path, request: &Request, started: String) -> Result<String> {
    let results = host.paths.root.join("results");
    guarded(host.owner, &results, DIRECTORY)?;
    let evidence = state::private_directory(&results, "force.")?;
    let admitted = validate_artifact(directory, request)
        .and_then(|_| publish(host, directory, &request.revision));
    let report = Report {
        action: "force-admit",
        revision: &request.revision,
        repository: &request.repository,
        forced: true,
        github_attested: false,
        ci_verification: UNVERIFIED,
        file_digests: &request.files,
        started_at: started,
        passed: admitted.is_ok(),
        phase: match admitted {
            Ok(()) => "complete",
            Err(_) => "validate",
        },
        evidence: evidence.display().to_string(),
        failure: admitted.as_ref().err().map(|_| "force_release_failed"),
        finished_at: timestamp(host),
    };
    let retained = state::atomic_json(&evidence.join("outcome.json"), &report);
    admitted?;
    retained?;
    let line = serde_json::to_string(&report).map_err(|_| VALUE)?;
    Ok(format!("{line}\n"))
}

/// The version and SHA-384 of every migration a checkout carries, as a
/// release manifest lists them.
fn migrations(checkout: &Path) -> Result<BTreeMap<String, String>> {
    let directory = checkout.join("migrations");
    // A checkout without the directory carries none.
    let entries = std::fs::read_dir(&directory).into_iter().flatten();
    let names = entries.map(|entry| Ok(entry?.file_name()));
    let mut names: Vec<OsString> = names.collect::<std::io::Result<_>>()?;
    names.sort();
    let mut found = BTreeMap::new();
    let refused = |code: &str| Fault::Refused(code.to_owned());
    let digits = |text: &str| !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit());
    let word = |text: &str| {
        let ordinary = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'_';
        !text.is_empty() && text.bytes().all(ordinary)
    };
    for name in names {
        if !name.as_encoded_bytes().ends_with(b".sql") {
            continue;
        }
        let path = directory.join(&name);
        let ordinary = std::fs::symlink_metadata(&path)?.is_file();
        let stem = name.to_str().and_then(|name| name.strip_suffix(".sql"));
        let split = stem.and_then(|stem| stem.split_once('_'));
        let split = split.filter(|(number, rest)| ordinary && digits(number) && word(rest));
        let (number, _) = split.ok_or_else(|| refused("invalid_migration_filename"))?;
        let version = number.parse::<i64>().ok().filter(|version| *version > 0);
        let version = version.map(|version| version.to_string());
        let version = version.filter(|version| !found.contains_key(version));
        let version = version.ok_or_else(|| refused("invalid_migration_version"))?;
        let digest = Sha384::digest(std::fs::read(&path)?);
        let digest = digest.iter().map(|byte| format!("{byte:02x}")).collect();
        found.insert(version, digest);
    }
    require((1..=1000).contains(&found.len()), "invalid_migration_count")?;
    Ok(found)
}

/// The request as it is written for the host: every field, in name order.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Spelled<'a> {
    ci_verification: &'static str,
    file_digests: &'a BTreeMap<String, String>,
    forced: bool,
    github_attested: bool,
    operation: &'static str,
    repository: &'a str,
    revision: &'a str,
    schema_version: u8,
    source_inputs_sha256: &'a BTreeMap<String, String>,
}

/// An export the controller checked, and the request it sends with it.
pub struct Authorized {
    /// The explicit unsigned request, as the host reads it.
    pub request: Vec<u8>,
    /// The digest of each exported file.
    pub digests: BTreeMap<String, String>,
}

/// Check `artifact` as an export of `checkout` at `revision` and spell the
/// request for it.
fn prepared(artifact: &Path, checkout: &Path, named: &str, revision: &str) -> Result<Authorized> {
    let information = std::fs::symlink_metadata(artifact);
    let ordinary = information.is_ok_and(|information| information.is_dir())
        && std::fs::canonicalize(artifact).is_ok_and(|resolved| resolved == artifact);
    require(ordinary, "invalid_force_artifact_directory")?;
    let files = member_digests(artifact)?;
    let mut inputs = BTreeMap::new();
    for name in SOURCE_INPUTS {
        inputs.insert(name.to_owned(), sha256(&checkout.join(name))?);
    }
    require(hexadecimal(revision, 40), "invalid_force_revision")?;
    require(repository(named), "invalid_force_repository")?;
    let request = Request {
        repository: named.to_owned(),
        revision: revision.to_owned(),
        files,
        inputs,
    };
    let manifest = validate_artifact(artifact, &request)?;
    let same = manifest.fields().migrations == migrations(checkout)?;
    require(same, "force_migrations_differ_from_source")?;
    let spelled = Spelled {
        ci_verification: UNVERIFIED,
        file_digests: &request.files,
        forced: true,
        github_attested: false,
        operation: "force-release",
        repository: named,
        revision,
        schema_version: 1,
        source_inputs_sha256: &request.inputs,
    };
    let mut text = serde_json::to_string_pretty(&spelled).map_err(|_| VALUE)?;
    text.push('\n');
    Ok(Authorized {
        request: text.into_bytes(),
        digests: request.files,
    })
}

/// Authorize one unsigned export for a host: it must be a passed build of
/// `checkout` at `revision` — the same source inputs, the same migrations, a
/// valid manifest and an image archive that manifest describes. Returns the
/// request to send with it, or a fixed code that names why not. Nothing is
/// written and no signature is verified.
pub fn authorize(
    artifact: &Path,
    checkout: &Path,
    repository: &str,
    revision: &str,
) -> std::result::Result<Authorized, String> {
    prepared(artifact, checkout, repository, revision).map_err(|fault| fault.text())
}

/// Admit the caller, the request's place and the request itself, take the
/// workload lock through `lock` and release it whatever happens under it.
fn execute(
    host: &Host<'_>,
    request: &Path,
    privileged: bool,
    lock: impl FnOnce(Takeover<'_>) -> std::result::Result<Workload, assets::Failure>,
) -> Result<String> {
    require(privileged, "explicit_root_force_required")?;
    let named = request.file_name().is_some_and(|name| name == REQUEST);
    require(named, "invalid_force_request_path")?;
    let forced = host.paths.root.join("forced");
    let given = request.parent().unwrap_or(Path::new(""));
    let identity = given.file_name().and_then(|name| name.to_str());
    let identity = identity.filter(|name| hexadecimal(name, 32));
    let identity = identity.filter(|_| given.parent() == Some(forced.as_path()));
    let directory =
        forced.join(identity.ok_or_else(|| Fault::Refused("invalid_force_directory".into()))?);
    for parent in [&host.paths.root, &forced, &directory] {
        guarded(host.owner, parent, DIRECTORY)?;
        let resolved = std::fs::canonicalize(parent)?;
        require(resolved.as_path() == parent.as_path(), "force_path_symlink")?;
    }
    for name in FILES.into_iter().chain([REQUEST]) {
        guarded(host.owner, &directory.join(name), file(0o600, limit(name)))?;
    }
    let request = validate(&read_object(&directory.join(REQUEST))?)?;
    let started = timestamp(host);
    let workload = match lock(Takeover::Nothing) {
        Ok(workload) => workload,
        Err(failure) if failure.busy() => return Err(Fault::Kind("BlockingIOError")),
        Err(_) => return Err(Fault::Refused(UNFINISHED.to_owned())),
    };
    let outcome = admit(host, &directory, &request, started);
    let released = workload.finish().map_err(|_| Fault::Kind("OSError"));
    let line = outcome?;
    released.map(|()| line)
}

/// `--request PATH` or `--request=PATH`, or nothing for any other command
/// line.
fn parse(arguments: &[OsString]) -> Option<PathBuf> {
    let value = match arguments {
        [option, value] if option == "--request" => value.to_str()?,
        [joined] => joined.to_str()?.strip_prefix("--request=")?,
        _ => return None,
    };
    (!value.starts_with('-')).then(|| PathBuf::from(value))
}

/// Run `release admit-unsigned --request PATH` on the prepared host. A
/// syntax error returns 2 before any effect; any refusal is one fixed line.
pub fn run(arguments: &[OsString]) -> ExitCode {
    let Some(request) = parse(arguments) else {
        return ExitCode::from(2);
    };
    rustix::process::umask(Mode::from_raw_mode(0o077));
    let clock = System::new();
    let host = Host {
        commands: &Local,
        clock: &clock,
        paths: Paths::fixed(),
        owner: 0,
    };
    let privileged = rustix::process::geteuid().is_root();
    match execute(&host, &request, privileged, Workload::acquire_for) {
        Ok(line) if emit(std::io::stdout(), &line) => ExitCode::SUCCESS,
        _ => {
            let _ = emit(std::io::stderr(), FAILED);
            ExitCode::FAILURE
        }
    }
}
