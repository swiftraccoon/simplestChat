//! Literal libpq commands. Connection secrets stay in the closed environment;
//! neither child output nor private command diagnostics enter the report.

use super::{
    config::{Config, Environment},
    policy,
};
use crate::bench::{
    clock::Clock,
    process::{self, Command, Host, Sink},
};
use anyhow::{Result, ensure};
use serde_json::Value;
use std::{os::unix::fs::PermissionsExt, path::PathBuf, time::Duration};

pub(super) const VERSION: &str = "SELECT to_json(current_setting('server_version_num')::integer)";
pub(super) const PROBE: &str = concat!(
    "SELECT json_build_object('connections',count(*),'active',count(*) FILTER ",
    "(WHERE state='active' AND pid<>pg_backend_pid()),'waiting',count(*) FILTER ",
    "(WHERE wait_event_type='Lock')) FROM pg_stat_activity WHERE datname=current_database()"
);

pub(super) fn find(
    environment: &Environment,
    directory: &std::path::Path,
    name: &str,
) -> Result<PathBuf> {
    let path = super::config::text(environment, "PATH")?;
    for root in std::env::split_paths(path) {
        let root = if root.is_absolute() {
            root
        } else {
            directory.join(root)
        };
        let path = root.join(name);
        if let Ok(metadata) = std::fs::metadata(&path)
            && metadata.is_file()
            && metadata.permissions().mode() & 0o111 != 0
        {
            return Ok(path);
        }
    }
    anyhow::bail!("database_client_missing")
}

fn decoded(value: &str) -> Result<String> {
    let mut result = Vec::with_capacity(value.len());
    let mut bytes = value.bytes();
    while let Some(byte) = bytes.next() {
        if byte == b'%' {
            let digit = |v: Option<u8>| v.and_then(|v| char::from(v).to_digit(16));
            let high = digit(bytes.next());
            let low = digit(bytes.next());
            let (Some(high), Some(low)) = (high, low) else {
                anyhow::bail!("invalid_database_userinfo");
            };
            result.push((high * 16 + low) as u8);
        } else {
            result.push(byte);
        }
    }
    let result =
        String::from_utf8(result).map_err(|_| anyhow::anyhow!("invalid_database_userinfo"))?;
    ensure!(!result.contains('\0'), "invalid_database_userinfo");
    Ok(result)
}

pub(super) fn environment(config: &Config) -> Result<Environment> {
    let mut result = config.path_environment();
    let host = config
        .database
        .host_str()
        .unwrap_or("")
        .trim_matches(['[', ']']);
    for (name, value) in [
        ("LC_ALL", "C".into()),
        ("PGHOST", host.into()),
        ("PGPORT", config.database.port().unwrap_or(5432).to_string()),
        ("PGDATABASE", config.database.path()[1..].into()),
        ("PGUSER", decoded(config.database.username())?),
        (
            "PGPASSWORD",
            decoded(config.database.password().unwrap_or(""))?,
        ),
        ("PGSSLMODE", "disable".into()),
        ("PGPASSFILE", "/dev/null".into()),
        ("PGCONNECT_TIMEOUT", "3".into()),
        ("PGOPTIONS", "-c statement_timeout=3000".into()),
    ] {
        result.insert(name.into(), value.into());
    }
    Ok(result)
}

#[derive(Debug)]
struct QueryExit(crate::bench::process::Exit);
impl std::fmt::Display for QueryExit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("database_command_failed")
    }
}
impl std::error::Error for QueryExit {}

pub(super) fn failure_status(error: &anyhow::Error) -> Option<u8> {
    error
        .downcast_ref::<QueryExit>()
        .map(|v| super::controller::exit_status(&v.0))
}
pub(super) fn failure_code(error: &anyhow::Error) -> Option<String> {
    if error.is::<CleanupUnconfirmed>() {
        return Some("database_cleanup_unconfirmed".into());
    }
    error.downcast_ref::<QueryExit>().map(|v| {
        v.0.code.and_then(|v| u8::try_from(v).ok()).map_or_else(
            || "database_command_failed".into(),
            |code| format!("database_command_exit_{code}"),
        )
    })
}

#[derive(Debug)]
struct CleanupUnconfirmed;
impl std::fmt::Display for CleanupUnconfirmed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("database_cleanup_unconfirmed")
    }
}
impl std::error::Error for CleanupUnconfirmed {}

pub(super) fn fatal_probe(error: &anyhow::Error) -> bool {
    let code = super::controller::known_status(error);
    error.is::<CleanupUnconfirmed>()
        || matches!(code, Some(130 | 143))
        || (code == Some(124) && !error.is::<crate::owned_process::child::CommandTimedOut>())
}

pub(super) struct Database<'a> {
    pub config: &'a Config,
    pub host: &'a dyn Host,
    pub clock: &'a dyn Clock,
    pub executable: PathBuf,
    pub environment: Environment,
}

impl Database<'_> {
    pub async fn query(&self, sql: &str, timing: bool) -> Result<Value> {
        let mut arguments = vec!["-X".into(), "-qAt".into(), "--set=ON_ERROR_STOP=1".into()];
        if timing {
            arguments.extend(["--command".into(), "\\timing on".into()]);
        }
        arguments.extend(["--command".into(), sql.into()]);
        let completion = process::run_observed(
            self.host,
            Command {
                executable: self.executable.clone(),
                arguments,
                directory: self.config.directory.clone(),
                environment: self.environment.clone(),
                timeout: self
                    .config
                    .bound(self.clock.elapsed_ms(), Duration::from_secs(5))?,
                stdout_limit: 1024 * 1024,
                stderr_limit: 1024 * 1024,
                input: Vec::new(),
                stdout: Sink::Capture,
                stderr: Sink::Capture,
                cleanup: false,
            },
        )
        .await;
        let proved = completion.stop_succeeded
            && completion
                .retirement
                .as_ref()
                .is_some_and(|v| v.group_cleanup_passed && v.streams_closed);
        let fallback = completion
            .retirement
            .as_ref()
            .and_then(super::controller::observed_exit)
            .filter(|exit| !exit.success());
        let mut result = completion.result.and_then(|output| {
            if !output.exit.success() {
                return Err(QueryExit(output.exit).into());
            }
            let text = std::str::from_utf8(&output.stdout)
                .map_err(|_| anyhow::anyhow!("invalid_database_json_response"))?;
            if timing {
                policy::parse_database_probe(text)
            } else {
                serde_json::from_str(text)
                    .map_err(|_| anyhow::anyhow!("invalid_database_json_response"))
            }
        });
        if result
            .as_ref()
            .err()
            .is_some_and(|error| super::controller::known_status(error).is_none())
            && let Some(exit) = fallback
        {
            result = result.map_err(|error| error.context(QueryExit(exit)));
        }
        if !proved {
            return Err(result
                .err()
                .unwrap_or_else(|| anyhow::anyhow!("database_cleanup_unconfirmed"))
                .context(CleanupUnconfirmed));
        }
        result
    }

    pub async fn retention(&self, room: &str) -> Result<Value> {
        ensure!(
            room.strip_prefix("account-room-")
                .and_then(|v| uuid::Uuid::parse_str(v).ok())
                .is_some(),
            "invalid_room_identity"
        );
        self.query(&format!(concat!(
            "SELECT COALESCE(json_agg(json_build_object('messageId',id,'senderAccountId',",
            "sender_account,'clientMessageId',client_message_id,'content',body->>'content') ",
            "ORDER BY id),'[]'::json) FROM chat_messages WHERE room_id='{}' ",
            "AND expires_at>clock_timestamp()"
        ), room), false).await
    }
    pub async fn retention_days(&self, room: &str) -> Result<Value> {
        ensure!(
            room.strip_prefix("account-room-")
                .and_then(|v| uuid::Uuid::parse_str(v).ok())
                .is_some(),
            "invalid_room_identity"
        );
        self.query(
            &format!("SELECT to_json(history_retention_days) FROM rooms WHERE id='{room}'"),
            false,
        )
        .await
    }
}
