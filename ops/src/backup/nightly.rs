//! The nightly archive: a private custom-format dump that must list before
//! it is published, a receipt that binds its bytes to the database image and
//! the migration ledger, and bounded retention of the archives before it.

use super::receipts;
use crate::host::evidence::{Evidence, Streams};
use crate::host::failure::{Failure, Result, require};
use crate::host::{Host, hexadecimal, state};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

const MAX_KEEP_DAYS: i64 = 3650;
const MAX_KEEP_COUNT: i64 = 365;
/// How long daemon-side work of an interrupted run may still be alive.
const DAEMON_CLEANUP_SECONDS: f64 = 480.0;
const GIB: u64 = 1024 * 1024 * 1024;
const SOCKET: &str = "/run/simplestchat-postgres";
const CONTAINER: &str = "name=^simplestchat-public-postgres-1$";
const SIZE: &str = "SELECT pg_database_size(current_database())";
pub(super) const LEDGER: &str = "SELECT version, success, encode(checksum, 'hex') \
                      FROM public._sqlx_migrations ORDER BY version";

/// What the command was asked to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Action {
    /// Take, validate and record tonight's archive, then apply retention.
    Nightly,
    /// Only remove recognized orphan partials and settle the journal.
    Cleanup,
}

/// One admitted invocation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Request {
    pub action: Action,
    pub days: i64,
    pub at_least: i64,
}

/// Refuse zero, negative and excessive retention before any write.
pub(super) fn retention(days: i64, at_least: i64) -> Result<()> {
    require(
        (1..=MAX_KEEP_DAYS).contains(&days),
        "Backup retention days must be an integer between 1 and 3650",
    )?;
    require(
        (1..=MAX_KEEP_COUNT).contains(&at_least),
        "Backup minimum count must be an integer between 1 and 365",
    )
}

/// Whether `name` is an archive timestamp: eight digits, `T`, six, `Z`.
pub(super) fn stamped(name: &str) -> bool {
    let bytes = name.as_bytes();
    let digits = |range: std::ops::Range<usize>| bytes[range].iter().all(u8::is_ascii_digit);
    bytes.len() == 16 && digits(0..8) && bytes[8] == b'T' && digits(9..15) && bytes[15] == b'Z'
}

/// The names in `directory`, in a fixed order.
fn names(directory: &Path) -> Result<Vec<String>> {
    let mut names = Vec::new();
    for entry in std::fs::read_dir(directory)? {
        names.push(
            entry?
                .file_name()
                .into_string()
                .map_err(|_| Failure::Value)?,
        );
    }
    names.sort();
    Ok(names)
}

/// Remove only recognized private orphan partials. Anything else that ends
/// like a partial is somebody else's and stays.
pub(super) fn cleanup_partials(host: &Host<'_>, target: &Path) -> Result<()> {
    state::protected(target, host.owner, true, &[0o700], None)?;
    for name in names(target)? {
        let Some(stem) = name.strip_suffix(".partial") else {
            continue;
        };
        let ours = [".dump", ".receipt.json"]
            .iter()
            .any(|kind| stem.strip_suffix(kind).is_some_and(stamped));
        if ours {
            let path = target.join(&name);
            state::protected(&path, host.owner, false, &[0o600], None)?;
            std::fs::remove_file(path)?;
        }
    }
    Ok(())
}

fn seconds(time: std::time::SystemTime) -> f64 {
    match time.duration_since(std::time::UNIX_EPOCH) {
        Ok(after) => after.as_secs_f64(),
        Err(before) => -before.duration().as_secs_f64(),
    }
}

/// Keep the newest validated archives: beyond the minimum count, one older
/// than the boundary goes with its receipt. Command output of earlier nights
/// follows the same age. No link is followed and any invalid receipt stops
/// the pruning.
fn prune(host: &Host<'_>, target: &Path, days: i64, at_least: i64) -> Result<()> {
    retention(days, at_least)?;
    let now = seconds(host.clock.now().into());
    let listed = names(target)?;
    let mut records = Vec::new();
    for name in listed.iter().filter(|name| name.ends_with(".receipt.json")) {
        let receipt = target.join(name);
        let validated = receipts::inspected(&receipt, host.owner, now, None)?;
        records.push((validated.completed, validated.dump, receipt));
    }
    #[expect(clippy::cast_precision_loss, reason = "at most 3650 days")]
    let boundary = now - days as f64 * 86400.0;
    records.sort_by(|a, b| {
        b.0.total_cmp(&a.0)
            .then_with(|| (&b.1, &b.2).cmp(&(&a.1, &a.2)))
    });
    let kept = usize::try_from(at_least).map_err(|_| Failure::Value)?;
    for (completed, dump, receipt) in records.into_iter().skip(kept) {
        if completed < boundary {
            std::fs::remove_file(receipt)?;
            std::fs::remove_file(dump)?;
        }
    }
    for name in &listed {
        if name.strip_suffix(".logs").is_some_and(stamped) {
            let directory = target.join(name);
            state::protected(&directory, host.owner, true, &[0o700], None)?;
            if seconds(std::fs::symlink_metadata(&directory)?.modified()?) < boundary {
                std::fs::remove_dir_all(directory)?;
            }
        }
    }
    Ok(())
}

fn text(bytes: Vec<u8>) -> Result<String> {
    String::from_utf8(bytes).map_err(|_| Failure::Value)
}

/// Require room for twice the current database and a gibibyte beside it.
fn headroom(runner: &mut Evidence<'_>, database: &str) -> Result<()> {
    let arguments = [
        "exec",
        "--user",
        "999:999",
        database,
        "psql",
        "-X",
        "-q",
        "-t",
        "-A",
        "-h",
        SOCKET,
        "-U",
        "postgres",
        "-d",
        "simplestchat",
        "-c",
        SIZE,
    ];
    let answer = text(runner.docker(&arguments, 15.0, Streams::default())?)?;
    let size = answer.trim();
    let whole = (1..=16).contains(&size.len())
        && !size.starts_with('0')
        && size.bytes().all(|byte| byte.is_ascii_digit());
    require(whole, "Invalid database size for backup reserve")?;
    let size: u64 = size.parse().map_err(|_| Failure::Value)?;
    let free = runner.host.commands.free(&runner.attempt)?;
    require(free > GIB + 2 * size, "Insufficient backup headroom")
}

/// The successful migration versions and their checksums, in ledger order.
fn ledger(runner: &mut Evidence<'_>, database: &str) -> Result<Vec<(String, String)>> {
    let arguments = [
        "exec",
        "--user",
        "999:999",
        database,
        "psql",
        "--no-psqlrc",
        "--set",
        "ON_ERROR_STOP=on",
        "--host",
        SOCKET,
        "--username",
        "postgres",
        "--dbname",
        "simplestchat",
        "--tuples-only",
        "--no-align",
        "--field-separator",
        " ",
        "--command",
        LEDGER,
    ];
    let answer = text(runner.docker(&arguments, 30.0, Streams::default())?)?;
    let mut result: Vec<(String, String)> = Vec::new();
    for line in answer.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        let valid = matches!(fields[..], [version, "t", checksum]
            if !version.starts_with('0')
                && !version.is_empty()
                && version.bytes().all(|byte| byte.is_ascii_digit())
                && hexadecimal(checksum, 96)
                && result.iter().all(|(seen, _)| seen != version));
        require(valid, "Invalid migration ledger")?;
        result.push((fields[0].to_owned(), fields[2].to_owned()));
    }
    require(!result.is_empty(), "Empty migration ledger")?;
    Ok(result)
}

/// Dump with a daemon-side lifetime shorter than the wait for it, and list
/// the archive before anything may name it.
fn dump_database(runner: &mut Evidence<'_>, database: &str, partial: &Path) -> Result<()> {
    let bounded = ["timeout", "--signal=TERM", "--kill-after=2s"];
    let mut dump = vec!["exec", "--user", "999:999", database];
    dump.extend(bounded);
    dump.extend([
        "120s",
        "pg_dump",
        "--host",
        SOCKET,
        "--username",
        "postgres",
        "--dbname",
        "simplestchat",
        "--format",
        "custom",
    ]);
    let archive = Streams {
        archive: Some(partial),
        ..Streams::default()
    };
    runner.docker(&dump, 130.0, archive)?;
    require(
        std::fs::metadata(partial)?.len() > 0,
        "Empty nightly backup",
    )?;
    let mut list = vec!["exec", "--interactive", "--user", "999:999", database];
    list.extend(bounded);
    list.extend(["30s", "pg_restore", "--list"]);
    let source = Streams {
        source: Some(partial),
        ..Streams::default()
    };
    runner.docker(&list, 40.0, source).map(|_| ())
}

/// Flush the complete archive before its name, or any receipt, can exist.
fn publish(partial: &Path, destination: &Path) -> Result<()> {
    let together = partial.parent() == destination.parent();
    require(together, "Backup publication must stay on one filesystem")?;
    let absent = std::fs::symlink_metadata(destination).is_err();
    require(absent, "Backup already exists")?;
    let mut options = std::fs::OpenOptions::new();
    let flags = rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK;
    let file = options
        .read(true)
        .custom_flags(flags.bits().cast_signed())
        .open(partial)?;
    let metadata = file.metadata()?;
    require(
        metadata.is_file() && metadata.len() > 0,
        "Empty or unsafe backup",
    )?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(partial, destination)?;
    let parent = destination.parent().ok_or(Failure::Os)?;
    std::fs::File::open(parent)?.sync_all()?;
    Ok(())
}

fn digest(path: &Path) -> Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut chunk = vec![0_u8; 1024 * 1024];
    loop {
        let count = file.read(&mut chunk)?;
        if count == 0 {
            break;
        }
        hasher.update(&chunk[..count]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

/// Versions and checksums as one object, in ledger order.
struct Migrations(Vec<(String, String)>);

impl serde::Serialize for Migrations {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serializer.collect_map(self.0.iter().map(|(version, checksum)| (version, checksum)))
    }
}

/// Everything an exact-format restore drill needs to know about one archive.
#[derive(serde::Serialize)]
struct Receipt {
    #[serde(rename = "schemaVersion")]
    schema_version: u8,
    dump: String,
    bytes: u64,
    sha256: String,
    #[serde(rename = "completedAt")]
    completed_at: String,
    #[serde(rename = "postgresImage")]
    image: String,
    #[serde(rename = "backupMigrations")]
    migrations: Migrations,
}

/// Publish the archive and its receipt only after validation, then apply
/// retention. Returns the archive.
fn nightly(host: &Host<'_>, target: &Path, days: i64, at_least: i64) -> Result<PathBuf> {
    retention(days, at_least)?;
    cleanup_partials(host, target)?;
    let stamp = host.clock.now().format("%Y%m%dT%H%M%SZ").to_string();
    let logs = target.join(format!("{stamp}.logs"));
    std::fs::DirBuilder::new().mode(0o700).create(&logs)?;
    let mut runner = Evidence::new(host, logs);
    let listing = ["ps", "--quiet", "--no-trunc", "--filter", CONTAINER];
    let listed = text(runner.docker(&listing, 30.0, Streams::default())?)?;
    let database = listed.trim().to_owned();
    require(hexadecimal(&database, 64), "Public database is not running")?;
    headroom(&mut runner, &database)?;
    let migrations = ledger(&mut runner, &database)?;
    let inspection = ["inspect", "--format", "{{.Image}}", &database];
    let image = text(runner.docker(&inspection, 30.0, Streams::default())?)?;
    let image = image.trim().to_owned();
    let identity = image
        .strip_prefix("sha256:")
        .is_some_and(|hash| hexadecimal(hash, 64));
    require(identity, "Invalid database image identity")?;
    let partial = target.join(format!("{stamp}.dump.partial"));
    let dump = target.join(format!("{stamp}.dump"));
    let published = (|| {
        dump_database(&mut runner, &database, &partial)?;
        publish(&partial, &dump)?;
        let receipt = Receipt {
            schema_version: 1,
            dump: format!("{stamp}.dump"),
            bytes: std::fs::metadata(&dump)?.len(),
            sha256: digest(&dump)?,
            completed_at: host.clock.now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
            image,
            migrations: Migrations(migrations),
        };
        state::atomic_json(&target.join(format!("{stamp}.receipt.json")), &receipt)
    })();
    // Whatever happened, no partial outlives the attempt that wrote it.
    let removed = match std::fs::remove_file(&partial) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error.into()),
        _ => Ok(()),
    };
    published.and(removed)?;
    prune(host, target, days, at_least)?;
    Ok(dump)
}

/// The ownership record every other operation refuses to work beside until
/// it is finalized, or until this owner's bounded daemon-side work has aged
/// out.
#[derive(serde::Serialize)]
struct Journal<'a> {
    #[serde(rename = "schemaVersion")]
    schema_version: u8,
    operation: &'static str,
    #[serde(rename = "bootId")]
    boot: &'a str,
    #[serde(rename = "untilMonotonic")]
    until: f64,
    finalized: bool,
}

/// Run one request under the already held workload lock. Returns the line to
/// print.
pub(super) fn run(host: &Host<'_>, boot: &str, request: Request) -> Result<Option<String>> {
    let target = host.paths.root.join("backups/nightly");
    state::protected(&target, host.owner, true, &[0o700], None)?;
    cleanup_partials(host, &target)?;
    let path = host.paths.work.join("current.json");
    let mut journal = Journal {
        schema_version: 1,
        operation: "nightly_backup",
        boot,
        until: host.clock.monotonic().as_secs_f64() + DAEMON_CLEANUP_SECONDS,
        finalized: request.action == Action::Cleanup,
    };
    state::atomic_json(&path, &journal)?;
    if request.action == Action::Cleanup {
        return Ok(None);
    }
    let dump = nightly(host, &target, request.days, request.at_least)?;
    journal.finalized = true;
    state::atomic_json(&path, &journal)?;
    let name = dump
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or(Failure::Value)?;
    Ok(Some(format!(
        "Nightly backup {name} validated and recorded.\n"
    )))
}
