//! Fixed engine/HTTP argument plans. Dynamic identities occupy literal argv slots.

use crate::ci::command::{Command, Program};
use std::time::Duration;

pub(super) const POSTGRES_IMAGE: &str = concat!(
    "docker.io/library/postgres:18.6-bookworm@sha256:",
    "3725f4e2499eef5134592b3b4ab79a543ed7f8e533b05b5b637af926630f6650",
);
pub(super) const OWNER: &str = "com.simplestchat.container-smoke";
pub(super) const REGISTER: &str = concat!(
    r#"{"email":"container-smoke@example.test","password":"Disposable-container-password-2026!","#,
    r#""display_name":"Container smoke"}"#,
);
pub(super) const MIGRATIONS: &str = "SELECT COUNT(*) FROM _sqlx_migrations WHERE success";
pub(super) const REGISTERED: &str =
    "SELECT COUNT(*) FROM users WHERE email = 'container-smoke@example.test'";
pub(super) const WATCHDOG: Duration = Duration::from_secs(300);
pub(super) const RESERVE: Duration = Duration::from_secs(60);
// Scheduled terminal observation, armed-group fallback and combined EOF/stop,
// including Darwin's bounded zombie-only proof. Not an OS/filesystem wall bound.
pub(super) const SETTLEMENT_ALLOWANCE: Duration = Duration::from_secs(20);
pub(super) const POLL: Duration = Duration::from_millis(500);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Asset {
    Manifest,
    Worker,
    Icon192,
    Icon512,
}
impl Asset {
    pub const ALL: [Self; 4] = [Self::Manifest, Self::Worker, Self::Icon192, Self::Icon512];
    pub fn name(self) -> &'static str {
        match self {
            Self::Manifest => "manifest.webmanifest",
            Self::Worker => "sw.js",
            Self::Icon192 => "icon-192.png",
            Self::Icon512 => "icon-512.png",
        }
    }
    pub fn accepts(self, value: &str) -> bool {
        match self {
            Self::Manifest => value == "application/manifest+json",
            Self::Worker => matches!(value, "application/javascript" | "text/javascript"),
            Self::Icon192 | Self::Icon512 => value == "image/png",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Role {
    Database,
    App,
}
impl Role {
    pub fn label(self) -> &'static str {
        match self {
            Self::Database => "database",
            Self::App => "app",
        }
    }
}

pub(super) fn docker(endpoint: &str, args: &[&str], cleanup: bool) -> Command {
    let mut command = Command::capture(Program::Docker, &["--host", endpoint]);
    command
        .arguments
        .extend(args.iter().map(std::ffi::OsString::from));
    command.capture = Some(64 * 1024);
    command.cleanup = cleanup;
    command
}

pub(super) fn create_database(endpoint: &str, name: &str, token: &str, image: &str) -> Command {
    docker(
        endpoint,
        &[
            "container",
            "create",
            "--name",
            name,
            "--label",
            &format!("{OWNER}={token}:database"),
            "--publish",
            "127.0.0.1::3000",
            "--tmpfs",
            "/var/lib/postgresql:rw,nosuid,nodev,size=512m",
            "--env",
            "POSTGRES_USER=test_owner",
            "--env",
            "POSTGRES_PASSWORD=disposable-container-password",
            "--env",
            "POSTGRES_DB=simplestchat_test",
            image,
            "postgres",
            "-c",
            "listen_addresses=127.0.0.1",
        ],
        false,
    )
}

pub(super) fn create_app(
    endpoint: &str,
    name: &str,
    token: &str,
    image: &str,
    database: &str,
    origin: &str,
) -> Command {
    docker(
        endpoint,
        &[
            "container",
            "create",
            "--name",
            name,
            "--label",
            &format!("{OWNER}={token}:app"),
            "--network",
            &format!("container:{database}"),
            "--read-only",
            "--cap-drop",
            "ALL",
            "--security-opt",
            "no-new-privileges",
            "--tmpfs",
            "/tmp:rw,nosuid,nodev,noexec,size=16m",
            "--env",
            concat!(
                "DATABASE_URL=postgres://test_owner:disposable-container-password@",
                "127.0.0.1:5432/simplestchat_test?sslmode=disable"
            ),
            "--env",
            "JWT_SECRET=disposable-container-jwt-secret-at-least-32-bytes",
            "--env",
            "RUN_MIGRATIONS=true",
            "--env",
            "REGISTRATION_ENABLED=true",
            "--env",
            "BIND_ADDR=0.0.0.0",
            "--env",
            "PORT=3000",
            "--env",
            "ANNOUNCE_IP=127.0.0.1",
            "--env",
            &format!("ALLOWED_ORIGINS={origin}"),
            "--env",
            "MEDIA_WORKERS=1",
            image,
        ],
        false,
    )
}

pub(super) fn psql(endpoint: &str, database: &str, sql: &str) -> Command {
    docker(
        endpoint,
        &[
            "exec",
            database,
            "psql",
            "-X",
            "-U",
            "test_owner",
            "-d",
            "simplestchat_test",
            "-At",
            "-c",
            sql,
        ],
        false,
    )
}

pub(super) fn curl(origin: &str, path: &str, seconds: u64, maximum: u64) -> Command {
    let mut command = Command::capture(
        Program::Curl,
        &[
            "--disable",
            "--fail",
            "--silent",
            "--show-error",
            "--proxy",
            "",
            "--proto",
            "=http",
            "--max-redirs",
            "0",
            "--max-time",
            &seconds.to_string(),
            &format!("{origin}{path}"),
        ],
    );
    command.timeout = Duration::from_secs(seconds);
    command.capture = Some(maximum);
    command
}
