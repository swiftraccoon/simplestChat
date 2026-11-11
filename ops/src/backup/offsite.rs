//! The offsite copy of the nightly archive and its restore drill. An upload
//! encrypts the newest validated archive and its receipt into the configured
//! repository and records the exact snapshot it became. A drill retrieves
//! exactly one recorded snapshot under byte ceilings and restores it in a
//! disposable database. Upload success and restore success are separate
//! evidence, and credentials stay in files that only commands read.

use super::nightly::stamped;
use super::receipts;
use super::restore::{self, Limits, hexadecimal, image_identity};
use crate::host::evidence::{Evidence, Streams};
use crate::host::failure::{Failure, Result, require};
use crate::host::{Host, state};
use crate::json::{InOrder, Json};
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};

const MIB: u64 = 1024 * 1024;
const GIB: u64 = 1024 * MIB;
const MAX_ARCHIVE_MIB: i64 = 4096;
const MAX_PORT: i64 = 65535;
const MAX_RECEIPT_BYTES: u64 = 16384;
const MAX_DOWNLOAD_FILES: usize = 2;
const KEYS: [&str; 8] = [
    "schemaVersion",
    "host",
    "user",
    "port",
    "repositoryPath",
    "archiveLimitMiB",
    "restoreMemoryMiB",
    "restoreDataMiB",
];

/// What the command was asked to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Action {
    Upload,
    Restore,
    Recover,
}

impl Action {
    fn name(self) -> &'static str {
        match self {
            Self::Upload => "upload",
            Self::Restore => "restore",
            Self::Recover => "recover",
        }
    }
}

/// One admitted invocation; an empty selector was not given.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Request {
    pub action: Action,
    pub snapshot: String,
    pub stamp: String,
}

/// A fixed SFTP destination and bounded restore resources.
#[derive(Debug, PartialEq, Eq)]
struct Settings {
    host: String,
    user: String,
    port: i64,
    repository_path: String,
    archive_mib: u64,
    limits: Limits,
}

/// One word as a POSIX shell reads it back unchanged.
fn quoted(word: &str) -> String {
    let safe = |byte: u8| byte.is_ascii_alphanumeric() || b"_@%+=:,./-".contains(&byte);
    if !word.is_empty() && word.bytes().all(safe) {
        return word.to_owned();
    }
    format!("'{}'", word.replace('\'', "'\"'\"'"))
}

impl Settings {
    /// The repository locator, without any credential.
    fn repository(&self) -> String {
        format!("sftp:{}@{}:{}", self.user, self.host, self.repository_path)
    }

    /// The repository client with dedicated files, strict host keys, and no
    /// agent, inherited configuration or password prompt.
    fn command(&self, configuration: &Path) -> Result<Vec<String>> {
        let file = |name: &str| {
            let path = configuration.join(name);
            path.to_str().map(str::to_owned).ok_or(Failure::Value)
        };
        let (port, known) = (
            self.port.to_string(),
            format!("UserKnownHostsFile={}", file("known_hosts")?),
        );
        let (identity, destination) = (file("identity")?, format!("{}@{}", self.user, self.host));
        let ssh = [
            "/usr/bin/ssh",
            "-F",
            "/dev/null",
            "-T",
            "-p",
            &port,
            "-i",
            &identity,
            "-o",
            "BatchMode=yes",
            "-o",
            "IdentitiesOnly=yes",
            "-o",
            "IdentityAgent=none",
            "-o",
            "PasswordAuthentication=no",
            "-o",
            "KbdInteractiveAuthentication=no",
            "-o",
            "StrictHostKeyChecking=yes",
            "-o",
            &known,
            "-o",
            "GlobalKnownHostsFile=/dev/null",
            "-o",
            "ClearAllForwardings=yes",
            "-o",
            "PermitLocalCommand=no",
            "-o",
            "ConnectTimeout=15",
            "-o",
            "ServerAliveInterval=15",
            "-o",
            "ServerAliveCountMax=3",
            "-s",
            &destination,
            "sftp",
        ];
        let transport: Vec<String> = ssh.iter().map(|word| quoted(word)).collect();
        Ok(vec![
            "/usr/bin/restic".to_owned(),
            "--no-cache".to_owned(),
            "--repo".to_owned(),
            self.repository(),
            "--password-file".to_owned(),
            file("password")?,
            "-o".to_owned(),
            format!("sftp.command={}", transport.join(" ")),
        ])
    }
}

fn all(text: &str, accept: impl Fn(u8) -> bool) -> bool {
    text.bytes().all(accept)
}

/// Require the current strict configuration and private, non-empty
/// credential files.
fn settings(host: &Host<'_>) -> Result<Settings> {
    let configuration = &host.paths.offsite;
    state::protected(configuration, host.owner, true, &[0o700], None)?;
    let path = configuration.join("settings.json");
    state::protected(&path, host.owner, false, &[0o600], Some(16384))?;
    for name in ["identity", "known_hosts", "password"] {
        let secret = configuration.join(name);
        state::protected(&secret, host.owner, false, &[0o600], Some(65536))?;
        let filled = std::fs::symlink_metadata(&secret)?.len() > 0;
        require(filled, "Empty offsite credential or host-key file")?;
    }
    let value = state::decode(&state::read(&path, 16384)?)?;
    let fields = value.object().ok_or(Failure::Value)?;
    let whole = |key: &str| match value.get(key) {
        Some(Json::Integer(_)) => Some(value.get(key).and_then(Json::integer)),
        _ => None,
    };
    let exact = fields.len() == KEYS.len() && KEYS.iter().all(|key| fields.contains_key(*key));
    require(
        exact && whole("schemaVersion") == Some(Some(1)),
        "Invalid offsite schema",
    )?;
    let text = |key: &str| value.get(key).and_then(Json::string).ok_or(Failure::Value);
    let (name, user, repository) = (text("host")?, text("user")?, text("repositoryPath")?);
    let edge = |byte: u8| byte.is_ascii_lowercase() || byte.is_ascii_digit();
    let ends = name.bytes().next().is_some_and(edge) && name.bytes().last().is_some_and(edge);
    let named = ends && name.len() <= 253 && all(name, |byte| edge(byte) || b".-".contains(&byte));
    require(named, "Invalid offsite host")?;
    let first = user
        .bytes()
        .next()
        .is_some_and(|byte| byte.is_ascii_lowercase() || byte == b'_');
    let login = first && user.len() <= 32 && all(user, |byte| edge(byte) || b"_-".contains(&byte));
    require(login, "Invalid offsite user")?;
    let segment = |part: &str| {
        !part.is_empty()
            && all(part, |byte| {
                byte.is_ascii_alphanumeric() || b"_-".contains(&byte)
            })
    };
    let rooted = repository
        .strip_prefix('/')
        .is_some_and(|rest| rest.split('/').all(segment));
    require(rooted, "Invalid offsite repository path")?;
    let mut integers = [0_i64; 4];
    let names = [
        "port",
        "archiveLimitMiB",
        "restoreMemoryMiB",
        "restoreDataMiB",
    ];
    for (slot, key) in integers.iter_mut().zip(names) {
        let item = whole(key);
        require(item.is_some(), "Offsite resource values must be integers")?;
        // A whole number too large to hold is outside every range below.
        *slot = item.flatten().unwrap_or(i64::MAX);
    }
    let [port, archive, memory_mib, data_mib] = integers;
    require((1..=MAX_PORT).contains(&port), "Invalid offsite port")?;
    require(
        (1..=MAX_ARCHIVE_MIB).contains(&archive),
        "Invalid restore archive bound",
    )?;
    let limits = Limits {
        memory_mib,
        data_mib,
    };
    limits.validate()?;
    Ok(Settings {
        host: name.to_owned(),
        user: user.to_owned(),
        port,
        repository_path: repository.to_owned(),
        archive_mib: u64::try_from(archive).map_err(|_| Failure::Value)?,
        limits,
    })
}

fn target(host: &Host<'_>) -> PathBuf {
    host.paths.root.join("backups/nightly")
}

fn seconds(host: &Host<'_>) -> f64 {
    let now: std::time::SystemTime = host.clock.now().into();
    now.duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |after| after.as_secs_f64())
}

fn timestamp(host: &Host<'_>) -> String {
    host.clock.now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// The exact database image and migration ledger recorded with an archive,
/// the ledger in the receipt's own order.
fn archive_identity(record: &Json) -> Result<(String, Vec<(String, String)>)> {
    let image = record
        .get("postgresImage")
        .and_then(Json::string)
        .ok_or(Failure::Value)?;
    require(
        image_identity(image),
        "Nightly receipt lacks its PostgreSQL image",
    )?;
    let Some(Json::Object(entries, order)) = record.get("backupMigrations") else {
        return Err(Failure::Value);
    };
    let mut migrations = Vec::new();
    for version in order {
        let checksum = entries
            .get(version)
            .and_then(Json::string)
            .ok_or(Failure::Value)?;
        migrations.push((version.clone(), checksum.to_owned()));
    }
    let exact = !migrations.is_empty()
        && migrations.iter().all(|(version, checksum)| {
            !version.starts_with('0')
                && !version.is_empty()
                && all(version, |byte| byte.is_ascii_digit())
                && hexadecimal(checksum, 96)
        });
    require(exact, "Nightly receipt lacks its exact migration ledger")?;
    Ok((image.to_owned(), migrations))
}

/// The newest validated nightly archive, no older than a day and a half,
/// with its receipt.
fn current_archive(host: &Host<'_>) -> Result<(PathBuf, PathBuf, Json)> {
    let (directory, now) = (target(host), seconds(host));
    let mut newest: Option<(f64, PathBuf, PathBuf, Json)> = None;
    let mut names = Vec::new();
    for entry in std::fs::read_dir(&directory)? {
        names.push(
            entry?
                .file_name()
                .into_string()
                .map_err(|_| Failure::Value)?,
        );
    }
    names.sort();
    for name in names.iter().filter(|name| name.ends_with(".receipt.json")) {
        let receipt = directory.join(name);
        let validated = receipts::inspected(&receipt, host.owner, now, None)?;
        if newest
            .as_ref()
            .is_none_or(|(completed, ..)| validated.completed > *completed)
        {
            newest = Some((
                validated.completed,
                validated.dump,
                receipt,
                validated.record,
            ));
        }
    }
    let Some((completed, archive, receipt, record)) = newest else {
        return Err(Failure::Refused("No verified nightly backup is available"));
    };
    let recent = now - completed <= 36.0 * 3600.0;
    require(recent, "Nightly backup is too old for offsite upload")?;
    archive_identity(&record)?;
    Ok((archive, receipt, record))
}

fn words(prefix: &[String], rest: &[&str]) -> Vec<String> {
    prefix
        .iter()
        .cloned()
        .chain(rest.iter().map(|word| (*word).to_owned()))
        .collect()
}

/// What an upload became; also the selection of a later drill.
#[derive(serde::Serialize)]
struct Uploaded<'a> {
    #[serde(rename = "schemaVersion")]
    schema_version: u8,
    repository: String,
    snapshot: &'a str,
    stamp: &'a str,
    sha256: &'a str,
    #[serde(rename = "finishedAt")]
    finished_at: String,
}

/// Encrypt the newest validated archive and its receipt, and keep the exact
/// identity of the snapshot they became.
fn upload(runner: &mut Evidence<'_>, configured: &Settings) -> Result<()> {
    let host = runner.host;
    let (archive, receipt, record) = current_archive(host)?;
    let path = |path: &Path| path.to_str().map(str::to_owned).ok_or(Failure::Value);
    let (sent, proof) = (path(&archive)?, path(&receipt)?);
    let command = configured.command(&host.paths.offsite)?;
    let rest = [
        "backup",
        "--json",
        "--tag",
        "simplestchat-nightly",
        "--",
        &sent,
        &proof,
    ];
    let output = runner.run(words(&command, &rest), 900.0, Streams::default())?;
    let mut snapshot = None;
    for line in output.split(|byte| matches!(byte, b'\n' | b'\r')) {
        if line.trim_ascii().is_empty() {
            continue;
        }
        let message = state::decode(line)?;
        message.object().ok_or(Failure::Value)?;
        if message.get("message_type").and_then(Json::string) == Some("summary") {
            snapshot = Some(message.get("snapshot_id").cloned());
        }
    }
    // A stream without a summary, or a summary without text, names nothing.
    let snapshot = snapshot.flatten().ok_or(Failure::Value)?;
    let snapshot = snapshot.string().ok_or(Failure::Value)?;
    require(
        hexadecimal(snapshot, 64),
        "Missing exact uploaded snapshot identity",
    )?;
    let stamp = archive
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or(Failure::Value)?;
    let result = Uploaded {
        schema_version: 1,
        repository: configured.repository(),
        snapshot,
        stamp,
        sha256: record
            .get("sha256")
            .and_then(Json::string)
            .ok_or(Failure::Key)?,
        finished_at: timestamp(host),
    };
    state::atomic_json(&host.paths.state.join("offsite-latest.json"), &result)?;
    state::atomic_json(&runner.attempt.join("outcome.json"), &result)
}

/// An explicit exact snapshot, or the last confirmed upload to this very
/// repository.
fn selection(
    host: &Host<'_>,
    configured: &Settings,
    request: &Request,
) -> Result<(String, String)> {
    let (mut snapshot, mut stamp) = (request.snapshot.clone(), request.stamp.clone());
    if snapshot.is_empty() && stamp.is_empty() {
        let path = host.paths.state.join("offsite-latest.json");
        state::protected(&path, host.owner, false, &[0o600], Some(16384))?;
        let value = state::decode(&state::read(&path, 16384)?)?;
        value.object().ok_or(Failure::Value)?;
        let current = matches!(value.get("schemaVersion"), Some(Json::Integer(one)) if one == "1");
        let repository = configured.repository();
        let ours = value.get("repository").and_then(Json::string) == Some(repository.as_str());
        require(current && ours, "Offsite selection differs")?;
        let text = |key: &str| value.get(key).and_then(Json::string).ok_or(Failure::Value);
        (snapshot, stamp) = (text("snapshot")?.to_owned(), text("stamp")?.to_owned());
    }
    let exact = hexadecimal(&snapshot, 64) && stamped(&stamp);
    require(
        exact,
        "Select an exact offsite snapshot and archive timestamp",
    )?;
    Ok((snapshot, stamp))
}

/// Stream one exact file of a snapshot into `file`, stopping at `maximum`.
fn retrieve(
    runner: &mut Evidence<'_>,
    command: &[String],
    snapshot: &str,
    file: &Path,
    maximum: u64,
) -> Result<()> {
    let name = file.file_name().ok_or(Failure::Value)?;
    let recorded = target(runner.host).join(name);
    let recorded = recorded.to_str().ok_or(Failure::Value)?;
    let download = Streams {
        archive: Some(file),
        limit: Some(maximum),
        ..Streams::default()
    };
    runner
        .run(
            words(command, &["dump", snapshot, recorded]),
            900.0,
            download,
        )
        .map(|_| ())
}

fn unique() -> Result<String> {
    let mut random = [0_u8; 4];
    getrandom::fill(&mut random).map_err(|_| Failure::Os)?;
    Ok(random.iter().map(|byte| format!("{byte:02x}")).collect())
}

/// A new private directory `prefix.<unique>` in `parent`.
fn private_directory(parent: &Path, prefix: &str) -> Result<PathBuf> {
    let path = parent.join(format!("{prefix}.{}", unique()?));
    std::fs::DirBuilder::new().mode(0o700).create(&path)?;
    Ok(path)
}

/// A verified drill; also the freshness marker of the last one.
#[derive(serde::Serialize)]
struct Drilled<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    evidence: Option<&'a str>,
    #[serde(flatten)]
    report: &'a restore::Report,
    #[serde(rename = "schemaVersion")]
    schema_version: u8,
    snapshot: &'a str,
    #[serde(rename = "backupSha256")]
    sha256: &'a str,
    #[serde(rename = "finishedAt")]
    finished_at: &'a str,
    repository: &'a str,
}

/// Retrieve one recorded snapshot and restore it in a disposable database.
/// Success is recorded only after the database was verified and removed.
fn drill(runner: &mut Evidence<'_>, configured: &Settings, request: &Request) -> Result<()> {
    let host = runner.host;
    let (snapshot, stamp) = selection(host, configured, request)?;
    let ceiling = configured.archive_mib * MIB;
    let room = host.commands.free(&runner.attempt)? > ceiling + GIB;
    require(room, "Insufficient restore download headroom")?;
    let directory = private_directory(&runner.attempt, "download")?;
    let receipt = directory.join(format!("{stamp}.receipt.json"));
    let archive = directory.join(format!("{stamp}.dump"));
    let restored = (|| -> Result<(restore::Report, String)> {
        let command = configured.command(&host.paths.offsite)?;
        retrieve(runner, &command, &snapshot, &receipt, MAX_RECEIPT_BYTES)?;
        retrieve(runner, &command, &snapshot, &archive, ceiling)?;
        let validated = receipts::inspected(&receipt, host.owner, seconds(host), None)?;
        require(
            validated.dump == archive,
            "Offsite receipt names another archive",
        )?;
        let (image, migrations) = archive_identity(&validated.record)?;
        let statements = host.paths.sql.join("restore-verify.sql");
        let limits = configured.limits;
        let report = restore::verify(
            runner,
            host.clock,
            &image,
            &archive,
            &migrations,
            limits,
            &statements,
        )?;
        let digest = validated
            .record
            .get("sha256")
            .and_then(Json::string)
            .ok_or(Failure::Key)?;
        Ok((report, digest.to_owned()))
    })();
    // The plaintext copy never outlives the attempt that retrieved it.
    let removed = std::fs::remove_dir_all(&directory);
    let (report, digest) = restored?;
    removed?;
    let passed = report.verified && report.cleanup_passed;
    require(passed, "Offsite restore or cleanup failed")?;
    let (finished, repository) = (timestamp(host), configured.repository());
    let mut outcome = Drilled {
        evidence: None,
        report: &report,
        schema_version: 1,
        snapshot: &snapshot,
        sha256: &digest,
        finished_at: &finished,
        repository: &repository,
    };
    state::atomic_json(&runner.attempt.join("outcome.json"), &outcome)?;
    outcome.evidence = Some(runner.attempt.to_str().ok_or(Failure::Value)?);
    state::atomic_json(
        &host.paths.state.join("restore-verified.timestamp"),
        &outcome,
    )
}

/// Whether `name` is `prefix.` and eight characters a temporary name uses.
fn owned(name: &str, prefix: &str) -> bool {
    let unique = name
        .strip_prefix(prefix)
        .and_then(|rest| rest.strip_prefix('.'));
    let shaped = |unique: &str| {
        unique.len() == 8
            && all(unique, |byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_'
            })
    };
    unique.is_some_and(shaped)
}

/// Remove only the private files this attempt's fixed download protocol
/// creates; anything else in a download directory refuses.
fn remove_downloads(host: &Host<'_>, attempt: &Path) -> Result<()> {
    let mut names = Vec::new();
    for entry in std::fs::read_dir(attempt)? {
        names.push(
            entry?
                .file_name()
                .into_string()
                .map_err(|_| Failure::Value)?,
        );
    }
    names.sort();
    for name in names.iter().filter(|name| name.starts_with("download.")) {
        require(owned(name, "download"), "Invalid download owner")?;
        let directory = attempt.join(name);
        state::protected(&directory, host.owner, true, &[0o700], None)?;
        let mut children = Vec::new();
        for entry in std::fs::read_dir(&directory)? {
            children.push(
                entry?
                    .file_name()
                    .into_string()
                    .map_err(|_| Failure::Value)?,
            );
        }
        let few = children.len() <= MAX_DOWNLOAD_FILES;
        require(few, "Unexpected restore download contents")?;
        for child in &children {
            let ours = [".dump", ".receipt.json"]
                .iter()
                .any(|kind| child.strip_suffix(kind).is_some_and(stamped));
            require(ours, "Unrecognized restore download")?;
            let most = Some(MAX_ARCHIVE_MIB.unsigned_abs() * MIB);
            state::protected(&directory.join(child), host.owner, false, &[0o600], most)?;
        }
        for child in &children {
            std::fs::remove_file(directory.join(child))?;
        }
        std::fs::remove_dir(&directory)?;
    }
    std::fs::File::open(attempt)?.sync_all()?;
    Ok(())
}

/// One journal as an object whose fields keep their order.
struct Fields(Vec<(String, Json)>);

impl Fields {
    fn of(value: &Json) -> Result<Self> {
        let Json::Object(entries, order) = value else {
            return Err(Failure::Value);
        };
        let field = |key: &String| entries.get(key).map(|item| (key.clone(), item.clone()));
        order
            .iter()
            .map(field)
            .collect::<Option<Vec<_>>>()
            .map(Self)
            .ok_or(Failure::Value)
    }

    /// Replace a field where it stands, or add it at the end.
    fn set(&mut self, key: &str, value: Json) {
        match self.0.iter_mut().find(|(name, _)| name == key) {
            Some((_, slot)) => *slot = value,
            None => self.0.push((key.to_owned(), value)),
        }
    }
}

impl serde::Serialize for Fields {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serializer.collect_map(self.0.iter().map(|(key, value)| (key, InOrder(value))))
    }
}

fn journal(host: &Host<'_>) -> PathBuf {
    host.paths.work.join("current.json")
}

/// Resolve only this workflow's own recorded attempt, under the workload
/// lock that is already held: remove the exact container a restore recorded,
/// refuse an unrecorded one, remove the attempt's downloads, and finalize the
/// journal as not passed. The interrupted command's evidence is kept; the
/// recovery's own goes to a new directory inside the attempt.
fn recover_interrupted(host: &Host<'_>) -> Result<()> {
    let path = journal(host);
    if std::fs::symlink_metadata(&path).is_err() {
        return Ok(());
    }
    state::protected(&path, host.owner, false, &[0o600], Some(16384))?;
    let current = state::decode(&state::read(&path, 16384)?)?;
    current.object().ok_or(Failure::Value)?;
    if current.get("finalized").and_then(Json::boolean) == Some(true) {
        return Ok(());
    }
    let text = |key: &str| current.get(key).and_then(Json::string);
    let action = text("action").filter(|action| ["upload", "restore"].contains(action));
    let ours = matches!(current.get("schemaVersion"), Some(Json::Integer(one)) if one == "1")
        && text("operation") == Some("offsite_backup");
    let (true, Some(action)) = (ours, action) else {
        return Err(Failure::Refused(
            "Unfinished ownership belongs to another operation",
        ));
    };
    let attempt = PathBuf::from(text("attempt").ok_or(Failure::Value)?);
    let attempts = host.paths.state.join("offsite");
    let name = attempt
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    let recorded = attempt.parent() == Some(attempts.as_path()) && owned(name, action);
    require(recorded, "Invalid offsite recovery attempt")?;
    state::protected(&attempts, host.owner, true, &[0o700], None)?;
    state::protected(&attempt, host.owner, true, &[0o700], None)?;
    let mut runner = Evidence::new(host, private_directory(&attempt, "recovery")?);
    let record = attempt.join("restore.json");
    if action == "restore" && std::fs::symlink_metadata(&record).is_ok() {
        state::protected(&record, host.owner, false, &[0o600], Some(65536))?;
        let report = state::decode(&state::read(&record, 65536)?)?;
        report.object().ok_or(Failure::Value)?;
        let field = |key: &str| report.get(key).and_then(Json::string).ok_or(Failure::Value);
        let (name, image, identity) = (
            field("container")?,
            field("postgresImage")?,
            field("containerId")?,
        );
        restore::cleanup(&mut runner, name, image, identity)?;
    } else if action == "restore" {
        let label = format!("label={}", restore::LABEL);
        let listing = ["ps", "--all", "--quiet", "--filter", &label];
        let listed = runner.docker(&listing, 30.0, Streams::default())?;
        let none = listed.trim_ascii().is_empty();
        require(
            none,
            "Unrecorded restore container requires ownership evidence",
        )?;
    }
    remove_downloads(host, &attempt)?;
    let finished = timestamp(host);
    let receipt = Fields(vec![
        ("schemaVersion".to_owned(), Json::Integer("1".to_owned())),
        ("recovered".to_owned(), Json::Bool(true)),
        ("finishedAt".to_owned(), Json::String(finished)),
    ]);
    state::atomic_json(&runner.attempt.join("recovery.json"), &receipt)?;
    let mut settled = Fields::of(&current)?;
    settled.set("finalized", Json::Bool(true));
    settled.set("passed", Json::Bool(false));
    for (key, value) in receipt.0 {
        settled.set(&key, value);
    }
    state::atomic_json(&path, &settled)
}

/// The ownership record of one upload or drill.
#[derive(serde::Serialize)]
struct Journal<'a> {
    #[serde(rename = "schemaVersion")]
    schema_version: u8,
    operation: &'static str,
    action: &'static str,
    attempt: &'a str,
    finalized: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    passed: Option<bool>,
}

/// Run one request under the already held workload lock. Returns the line to
/// print.
pub(super) fn run(host: &Host<'_>, request: &Request) -> Result<&'static str> {
    state::protected(&host.paths.state, host.owner, true, &[0o750], None)?;
    recover_interrupted(host)?;
    let selected = !request.snapshot.is_empty() || !request.stamp.is_empty();
    if request.action == Action::Recover {
        require(!selected, "Recovery selects its journal")?;
        return Ok("Offsite ownership recovered; original outcome retained.\n");
    }
    let configured = settings(host)?;
    let attempts = host.paths.state.join("offsite");
    match std::fs::DirBuilder::new().mode(0o700).create(&attempts) {
        Err(error) if error.kind() != std::io::ErrorKind::AlreadyExists => return Err(error.into()),
        _ => {}
    }
    state::protected(&attempts, host.owner, true, &[0o700], None)?;
    let attempt = private_directory(&attempts, request.action.name())?;
    let mut runner = Evidence::new(host, attempt.clone());
    let mut current = Journal {
        schema_version: 1,
        operation: "offsite_backup",
        action: request.action.name(),
        attempt: attempt.to_str().ok_or(Failure::Value)?,
        finalized: false,
        passed: None,
    };
    state::atomic_json(&journal(host), &current)?;
    let outcome = if request.action == Action::Upload {
        require(!selected, "Upload does not select a restore snapshot")
            .and_then(|()| upload(&mut runner, &configured))
    } else {
        drill(&mut runner, &configured, request)
    };
    if let Err(failure) = outcome {
        // Whatever the attempt left is resolved now, or stays this owner's
        // to recover later; the failure itself is what the caller hears.
        let _ = recover_interrupted(host);
        return Err(failure);
    }
    (current.finalized, current.passed) = (true, Some(true));
    state::atomic_json(&journal(host), &current)?;
    Ok("Offsite operation completed; private evidence retained.\n")
}
