//! Fixed restore roles and passive backup-ledger admission.
use crate::json::Json;
use std::{collections::BTreeMap, ffi::OsString};

pub(super) type Result<T> = std::result::Result<T, &'static str>;
pub(super) const LABEL: &str = "clinic.research.simplestchat.restore";
pub(super) const SOCKET: &str = "/var/run/postgresql";
pub(super) const DATABASE: &str = "restorecheck";
pub(super) const ROLE_SQL: &[u8] = concat!(
    "CREATE ROLE simplestchat_app NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE;\n",
    "CREATE ROLE simplestchat_migrate NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE;\n",
)
.as_bytes();
pub(super) const READY_TEST: &str = concat!(
    "test \"$(cat /proc/1/comm)\" = postgres && ",
    "pg_isready -q -h /var/run/postgresql -U postgres -d restorecheck",
);
pub(super) const LEDGER: &str = concat!(
    "SELECT version, success, encode(checksum, 'hex') ",
    "FROM public._sqlx_migrations ORDER BY version",
);
pub(super) fn hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
pub(super) fn image(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|s| hex(s, 64))
}
pub(super) fn text<'a>(value: &'a Json, key: &str) -> Result<&'a str> {
    value
        .get(key)
        .and_then(Json::string)
        .ok_or("validation_failed")
}
pub(super) fn decode(data: &[u8]) -> Result<Json> {
    crate::json::decode(data).map_err(|_| "validation_failed")
}
pub(super) fn migrations(
    value: &Json,
    packaged: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>> {
    let entries = value
        .get("backupMigrations")
        .and_then(Json::object)
        .ok_or("Backup lacks its recorded migration ledger")?;
    if entries.is_empty() {
        return Err("Backup migration ledger differs from the reviewed release");
    }
    let mut result = BTreeMap::new();
    for (version, checksum) in entries {
        let checksum = checksum.string().ok_or("validation_failed")?;
        if version.starts_with('0')
            || version.is_empty()
            || !version.bytes().all(|b| b.is_ascii_digit())
            || !hex(checksum, 96)
            || packaged.get(version).map(String::as_str) != Some(checksum)
        {
            return Err("Backup migration ledger differs from the reviewed release");
        }
        result.insert(version.clone(), checksum.into());
    }
    Ok(result)
}
pub(super) fn ledger(migrations: &BTreeMap<String, String>) -> String {
    let mut entries: Vec<_> = migrations.iter().collect();
    entries.sort_by(|a, b| a.0.len().cmp(&b.0.len()).then_with(|| a.0.cmp(b.0)));
    entries
        .into_iter()
        .map(|(v, sha)| format!("{v}|t|{sha}"))
        .collect::<Vec<_>>()
        .join("\n")
}
pub(super) fn create(name: &str, image: &str) -> Vec<OsString> {
    [
        "create",
        "--name",
        name,
        "--label",
        &format!("{LABEL}={name}"),
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
        "512m",
        "--memory-swap",
        "512m",
        "--cpus",
        "0.5",
        "--pids-limit",
        "64",
        "--shm-size",
        "16m",
        "--log-driver",
        "none",
        "--tmpfs",
        "/var/lib/postgresql:rw,nosuid,nodev,noexec,size=256m,uid=999,gid=999,mode=0700",
        "--tmpfs",
        "/var/run/postgresql:rw,nosuid,nodev,noexec,size=1m,uid=999,gid=999,mode=0700",
        "--tmpfs",
        "/tmp:rw,nosuid,nodev,noexec,size=16m,mode=1777",
        "--env",
        "PGDATA=/var/lib/postgresql/data",
        "--env",
        "POSTGRES_DB=restorecheck",
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
        "unix_socket_directories=/var/run/postgresql",
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
    ]
    .iter()
    .map(OsString::from)
    .collect()
}
pub(super) fn sql(id: &str) -> Vec<OsString> {
    [
        "exec",
        "-i",
        "--user",
        "999:999",
        id,
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
    ]
    .iter()
    .map(OsString::from)
    .collect()
}
