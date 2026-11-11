//! Verification of one archive in a disposable database: a network-isolated
//! container of the archive's own PostgreSQL image, on memory filesystems
//! only, restored completely, compared with the recorded migration ledger,
//! checked and removed. The public database is never a target.

use super::nightly::LEDGER;
use crate::host::evidence::{Evidence, Streams};
use crate::host::failure::{Failure, Result, require};
use crate::host::{Clock, state};
use crate::json::{InOrder, Json};
use std::path::Path;
use std::time::Duration;

/// The label every disposable restore container carries, valued with its name.
pub(super) const LABEL: &str = "clinic.research.simplestchat.restore";
const SOCKET: &str = "/var/run/postgresql";
const DATABASE: &str = "restorecheck";
const ROLES: &str = "\
CREATE ROLE simplestchat_app NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE;\n\
CREATE ROLE simplestchat_migrate NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE;\n";
const READY: &str = "test \"$(cat /proc/1/comm)\" = postgres && \
                     pg_isready -q -h /var/run/postgresql -U postgres -d restorecheck";
const INSPECTION: &str = concat!(
    r#"{"id":{{json .Id}},"name":{{json .Name}},"image":{{json .Image}},"#,
    r#""owner":{{json (index .Config.Labels "clinic.research.simplestchat.restore")}}}"#,
);
const MIN_MEMORY_MIB: i64 = 512;
const MAX_MEMORY_MIB: i64 = 65536;
const PROCESS_RESERVE_MIB: i64 = 256;
const BOUNDED: [&str; 3] = ["timeout", "--signal=TERM", "--kill-after=2s"];

/// Explicit resource ceilings of the disposable database; exceeding them
/// fails the drill.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Limits {
    pub memory_mib: i64,
    pub data_mib: i64,
}

impl Limits {
    /// Keep the selected memory bounded and the data within it with room for
    /// the server's own processes.
    pub(super) fn validate(self) -> Result<()> {
        let bounded = (MIN_MEMORY_MIB..=MAX_MEMORY_MIB).contains(&self.memory_mib);
        require(bounded, "Invalid restore memory ceiling")?;
        let fits = PROCESS_RESERVE_MIB <= self.data_mib
            && self.data_mib <= self.memory_mib - PROCESS_RESERVE_MIB;
        let headroom = "Restore data must fit within its memory ceiling with process headroom";
        require(fits, headroom)
    }
}

pub(super) use crate::host::{hexadecimal, image_id as image_identity};

fn text(bytes: Vec<u8>) -> Result<String> {
    String::from_utf8(bytes).map_err(|_| Failure::Value)
}

/// Only memory filesystems and the image already present: no pull, no
/// network, no capability, no log driver.
fn create_arguments(name: &str, image: &str, limits: Limits) -> Result<Vec<String>> {
    limits.validate()?;
    let memory = format!("{}m", limits.memory_mib);
    let data = format!(
        "/var/lib/postgresql:rw,nosuid,nodev,noexec,size={}m,uid=999,gid=999,mode=0700",
        limits.data_mib
    );
    let label = format!("{LABEL}={name}");
    let socket = format!("unix_socket_directories={SOCKET}");
    let database = format!("POSTGRES_DB={DATABASE}");
    let words = [
        "create",
        "--name",
        name,
        "--label",
        &label,
        "--pull",
        "never",
        "--network",
        "none",
        "--read-only",
        "--user",
        "999:999",
        "--cap-drop",
        "ALL",
        "--security-opt",
        "no-new-privileges:true",
        "--memory",
        &memory,
        "--memory-swap",
        &memory,
        "--cpus",
        "0.5",
        "--pids-limit",
        "64",
        "--shm-size",
        "16m",
        "--log-driver",
        "none",
        "--tmpfs",
        &data,
        "--tmpfs",
        "/var/run/postgresql:rw,nosuid,nodev,noexec,size=1m,uid=999,gid=999,mode=0700",
        "--tmpfs",
        "/tmp:rw,nosuid,nodev,noexec,size=16m,mode=1777",
        "--env",
        "PGDATA=/var/lib/postgresql/data",
        "--env",
        &database,
        "--env",
        "POSTGRES_HOST_AUTH_METHOD=trust",
        "--env",
        "POSTGRES_INITDB_ARGS=--auth-local=peer --auth-host=reject",
        "--entrypoint",
        "docker-entrypoint.sh",
        image,
        "postgres",
        "-c",
        "listen_addresses=",
        "-c",
        &socket,
        "-c",
        "shared_buffers=32MB",
        "-c",
        "work_mem=4MB",
        "-c",
        "maintenance_work_mem=32MB",
        "-c",
        "max_connections=10",
        "-c",
        "statement_timeout=120000",
    ];
    Ok(words.iter().map(|word| (*word).to_owned()).collect())
}

/// Run fixed SQL through the disposable container's own private socket.
fn sql(runner: &mut Evidence<'_>, container: &str, source: &[u8]) -> Result<Vec<u8>> {
    let arguments = [
        "exec",
        "-i",
        "--user",
        "999:999",
        container,
        "psql",
        "-X",
        "-q",
        "-t",
        "-A",
        "--set",
        "ON_ERROR_STOP=1",
        "-h",
        SOCKET,
        "-U",
        "postgres",
        "-d",
        DATABASE,
    ];
    let input = Streams {
        input: Some(source),
        ..Streams::default()
    };
    runner.docker(&arguments, 45.0, input)
}

/// Whether `name` is a restore container's: the fixed prefix and 32
/// hexadecimal digits.
fn owned_name(name: &str) -> bool {
    name.strip_prefix("scpub-restore-")
        .is_some_and(|unique| hexadecimal(unique, 32))
}

/// Remove only the one labeled container with this name, image and, when it
/// is known, identity, including one created just before a timeout. Nothing
/// is removed by label alone.
pub(super) fn cleanup(
    runner: &mut Evidence<'_>,
    name: &str,
    image: &str,
    expected: &str,
) -> Result<()> {
    let known = expected.is_empty() || hexadecimal(expected, 64);
    let owned = owned_name(name) && image_identity(image) && known;
    require(owned, "Invalid owned restore identity")?;
    let (label, exact) = (format!("label={LABEL}={name}"), format!("name=^/{name}$"));
    let selectors = [
        "ps",
        "--all",
        "--quiet",
        "--no-trunc",
        "--filter",
        &label,
        "--filter",
        &exact,
    ];
    let listed = text(runner.docker(&selectors, 30.0, Streams::default())?)?;
    let identity = listed.trim();
    if !identity.is_empty() {
        require(hexadecimal(identity, 64), "Ambiguous restore container")?;
        let inspection = ["inspect", "--format", INSPECTION, identity];
        let metadata = state::decode(&runner.docker(&inspection, 30.0, Streams::default())?)?;
        metadata.object().ok_or(Failure::Value)?;
        let field = |key: &str| metadata.get(key).and_then(Json::string);
        let unchanged = (expected.is_empty() || identity == expected)
            && field("id") == Some(identity)
            && field("name") == Some(format!("/{name}").as_str())
            && field("image") == Some(image)
            && field("owner") == Some(name);
        require(unchanged, "Restore container ownership changed")?;
        let removal = ["rm", "--force", "--volumes", identity];
        runner.docker(&removal, 30.0, Streams::default())?;
    }
    let left = runner.docker(&selectors, 30.0, Streams::default())?;
    require(
        left.trim_ascii().is_empty(),
        "Restore container cleanup incomplete",
    )
}

/// What one verification did; rewritten in the attempt as it proceeds, so an
/// interrupted one still names the container it may have left.
#[derive(serde::Serialize)]
pub(super) struct Report {
    pub container: String,
    #[serde(rename = "containerId")]
    pub container_id: String,
    #[serde(rename = "postgresImage")]
    pub image: String,
    #[serde(rename = "cleanupPassed")]
    pub cleanup_passed: bool,
    pub verified: bool,
    #[serde(skip_serializing_if = "Option::is_none", serialize_with = "ordered")]
    pub counts: Option<Json>,
}

fn ordered<S: serde::Serializer>(
    counts: &Option<Json>,
    serializer: S,
) -> std::result::Result<S::Ok, S::Error> {
    match counts {
        Some(counts) => serde::Serialize::serialize(&InOrder(counts), serializer),
        None => serializer.serialize_none(),
    }
}

/// The successful steps between creating the container and removing it.
#[expect(
    clippy::too_many_arguments,
    reason = "one fixed sequence over its fixed inputs"
)]
fn restored(
    runner: &mut Evidence<'_>,
    clock: &dyn Clock,
    report: &mut Report,
    path: &Path,
    backup: &Path,
    migrations: &[(String, String)],
    limits: Limits,
    statements: &Path,
) -> Result<()> {
    let arguments = create_arguments(&report.container, &report.image, limits)?;
    let arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();
    let created = text(runner.docker(&arguments, 30.0, Streams::default())?)?;
    let identity = created.trim().to_owned();
    require(
        hexadecimal(&identity, 64),
        "Invalid restore container identity",
    )?;
    report.container_id.clone_from(&identity);
    state::atomic_json(path, report)?;
    runner.docker(&["start", &identity], 30.0, Streams::default())?;
    let deadline = clock.monotonic() + Duration::from_secs(30);
    let probe = [
        "exec", "--user", "999:999", &identity, "/bin/sh", "-c", READY,
    ];
    loop {
        match runner.docker(&probe, 3.0, Streams::default()) {
            Ok(_) => break,
            Err(Failure::Command | Failure::Timeout | Failure::Refused(_)) => {
                require(
                    clock.monotonic() < deadline,
                    "Isolated PostgreSQL was not ready",
                )?;
                clock.sleep(Duration::from_millis(500));
            }
            Err(other) => return Err(other),
        }
    }
    sql(runner, &identity, ROLES.as_bytes())?;
    let mut restore = vec!["exec", "-i", "--user", "999:999", &identity];
    restore.extend(BOUNDED);
    restore.extend([
        "120s",
        "pg_restore",
        "--exit-on-error",
        "--single-transaction",
        "-h",
        SOCKET,
        "-U",
        "postgres",
        "-d",
        DATABASE,
    ]);
    let archive = Streams {
        source: Some(backup),
        ..Streams::default()
    };
    runner.docker(&restore, 125.0, archive)?;
    let mut expected: Vec<&(String, String)> = migrations.iter().collect();
    expected.sort_by(|a, b| (a.0.len(), &a.0).cmp(&(b.0.len(), &b.0)));
    let lines: Vec<String> = expected
        .iter()
        .map(|(version, sum)| format!("{version}|t|{sum}"))
        .collect();
    let actual = text(sql(runner, &identity, LEDGER.as_bytes())?)?;
    let same = actual.trim() == lines.join("\n");
    require(same, "Restored migration ledger differs from release")?;
    let counts = state::decode(&sql(runner, &identity, &std::fs::read(statements)?)?)?;
    counts.object().ok_or(Failure::Value)?;
    report.counts = Some(counts);
    let mut check = vec!["exec", "--user", "999:999", &identity];
    check.extend(BOUNDED);
    check.extend([
        "120s",
        "pg_amcheck",
        "--install-missing",
        "--heapallindexed",
        "--parent-check",
        "-h",
        SOCKET,
        "-U",
        "postgres",
        "-d",
        DATABASE,
    ]);
    runner.docker(&check, 125.0, Streams::default())?;
    report.verified = true;
    Ok(())
}

/// Restore every section of `backup` into a new disposable database of
/// `image`, validate it and destroy the database. The report says whether
/// each of verification and cleanup passed; a failure of either is returned
/// only after the cleanup was attempted and the report rewritten.
pub(super) fn verify(
    runner: &mut Evidence<'_>,
    clock: &dyn Clock,
    image: &str,
    backup: &Path,
    migrations: &[(String, String)],
    limits: Limits,
    statements: &Path,
) -> Result<Report> {
    let mut unique = [0_u8; 16];
    getrandom::fill(&mut unique).map_err(|_| Failure::Os)?;
    let unique: String = unique.iter().map(|byte| format!("{byte:02x}")).collect();
    let mut report = Report {
        container: format!("scpub-restore-{unique}"),
        container_id: String::new(),
        image: image.to_owned(),
        cleanup_passed: false,
        verified: false,
        counts: None,
    };
    let path = runner.attempt.join("restore.json");
    state::atomic_json(&path, &report)?;
    let label = format!("label={LABEL}");
    let prior = runner.docker(
        &["ps", "--all", "--quiet", "--filter", &label],
        30.0,
        Streams::default(),
    )?;
    let none = prior.trim_ascii().is_empty();
    require(
        none,
        "Inspect the retained restore container before starting another",
    )?;
    let outcome = restored(
        runner,
        clock,
        &mut report,
        &path,
        backup,
        migrations,
        limits,
        statements,
    );
    let cleaned = cleanup(runner, &report.container, image, &report.container_id);
    report.cleanup_passed = cleaned.is_ok();
    state::atomic_json(&path, &report)?;
    cleaned?;
    outcome?;
    Ok(report)
}
