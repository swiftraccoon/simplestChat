//! Closed hosted-only experiment profile. Selected paths and credentials never
//! enter diagnostic text; the controller derives its own PID/origin/token.

use anyhow::{Result, ensure};
use std::{collections::BTreeMap, ffi::OsString, path::PathBuf, time::Duration};
use url::Url;

pub(super) type Environment = BTreeMap<OsString, OsString>;
pub(super) const ACTIVE_MS: f64 = 33.0 * 60_000.0;
pub(super) const FINAL_MS: f64 = 35.0 * 60_000.0;
pub(super) const GENERATOR_MS: f64 = 1_470_000.0;
pub(super) const CLIENTS: usize = 30;

pub(super) struct Config {
    pub environment: Environment,
    pub directory: PathBuf,
    pub server: PathBuf,
    pub generator: PathBuf,
    pub output: PathBuf,
    pub temporary: PathBuf,
    pub database: Url,
    pub revision: String,
    pub port: u16,
    pub media_port: u16,
    pub origin: String,
    pub metrics_token: String,
}

pub(super) fn text<'a>(environment: &'a Environment, name: &str) -> Result<&'a str> {
    environment
        .get(&OsString::from(name))
        .and_then(|v| v.to_str())
        .ok_or_else(|| anyhow::anyhow!("invalid_account_room_environment"))
}

fn path(environment: &Environment, name: &str) -> Result<PathBuf> {
    let value = PathBuf::from(text(environment, name)?);
    ensure!(value.is_absolute(), "absolute_account_room_path_required");
    ensure!(value.components().count() <= 20, "account_room_path_limit");
    Ok(value)
}

impl Config {
    pub fn parse(environment: Environment, directory: PathBuf, platform: &str) -> Result<Self> {
        ensure!(
            platform == "linux"
                && text(&environment, "GITHUB_ACTIONS")? == "true"
                && text(&environment, "RUNNER_OS")? == "Linux",
            "hosted_linux_only"
        );
        ensure!(
            text(&environment, "ACCOUNT_ROOM_E2E")? == "1"
                && text(&environment, "DISPOSABLE_TEST_DATABASE")? == "1",
            "explicit_disposable_opt_in_required"
        );
        let database_text = text(&environment, "DATABASE_URL")?;
        let database = Url::parse(database_text)
            .map_err(|_| anyhow::anyhow!("disposable_loopback_database_required"))?;
        ensure!(
            matches!(database.scheme(), "postgres" | "postgresql")
                && matches!(
                    database.host_str(),
                    Some("127.0.0.1" | "localhost" | "[::1]")
                )
                && database.fragment().is_none()
                && matches!(database.query(), None | Some("sslmode=disable")),
            "disposable_loopback_database_required"
        );
        let name = database.path().strip_prefix('/').unwrap_or("");
        ensure!(
            name.ends_with("_test")
                && name.len() > 5
                && name.len() <= 128
                && name.bytes().all(|v| v.is_ascii_alphanumeric() || v == b'_'),
            "disposable_loopback_database_required"
        );
        if let Some(other) = environment.get(&OsString::from("TEST_DATABASE_URL")) {
            ensure!(other == database_text, "database_selection_mismatch");
        }
        let revision = text(&environment, "SERVER_REVISION")?.to_owned();
        ensure!(
            revision.len() == 40
                && revision
                    .bytes()
                    .all(|v| v.is_ascii_digit() || (b'a'..=b'f').contains(&v)),
            "exact_revision_required"
        );
        let port = |name: &str, fallback: u16| -> Result<u16> {
            let Some(value) = environment.get(&OsString::from(name)) else {
                return Ok(fallback);
            };
            let value = value.to_str().and_then(|v| v.parse::<u16>().ok());
            value
                .filter(|v| *v >= 1000)
                .ok_or_else(|| anyhow::anyhow!("invalid_account_room_port"))
        };
        let port_number = port("TEST_SERVER_PORT", 3119)?;
        let media_port = port("TEST_MEDIA_PORT", 41010)?;
        let mut random = [0_u8; 32];
        rand::RngExt::fill(&mut rand::rng(), &mut random);
        Ok(Self {
            server: path(&environment, "TEST_SERVER_BINARY")?,
            generator: path(&environment, "TEST_GENERATOR_BINARY")?,
            output: path(&environment, "E2E_ARTIFACTS")?,
            temporary: path(&environment, "RUNNER_TEMP")?
                .join(format!("account-room-private.{}", uuid::Uuid::new_v4())),
            origin: format!("http://127.0.0.1:{port_number}"),
            metrics_token: hex::encode(random),
            environment,
            directory,
            database,
            revision,
            port: port_number,
            media_port,
        })
    }

    pub fn bound(&self, elapsed: f64, maximum: Duration) -> Result<Duration> {
        ensure!(
            elapsed.is_finite() && (0.0..ACTIVE_MS).contains(&elapsed),
            "account_room_deadline"
        );
        Ok(maximum.min(Duration::from_secs_f64((ACTIVE_MS - elapsed) / 1000.0)))
    }

    pub fn path_environment(&self) -> Environment {
        self.environment
            .get_key_value(&OsString::from("PATH"))
            .map(|(k, v)| BTreeMap::from([(k.clone(), v.clone())]))
            .unwrap_or_default()
    }

    pub fn server_environment(&self) -> Environment {
        let mut result = self.path_environment();
        for (key, value) in [
            ("DATABASE_URL", self.database.as_str().to_owned()),
            ("BIND_ADDR", "127.0.0.1".into()),
            ("PORT", self.port.to_string()),
            ("ANNOUNCE_IP", "127.0.0.1".into()),
            ("MEDIA_WORKERS", "1".into()),
            ("WEBRTC_SERVER_PORT_BASE", self.media_port.to_string()),
            ("ALLOWED_ORIGINS", self.origin.clone()),
            ("REGISTRATION_ENABLED", "true".into()),
            ("ALLOW_AD_HOC_ROOMS", "true".into()),
            ("REGISTRATIONS_PER_IP_PER_HOUR", "100".into()),
            ("AUTH_REQUESTS_PER_MINUTE", "300".into()),
            (
                "JWT_SECRET",
                "disposable-test-only-jwt-secret-at-least-32-bytes".into(),
            ),
            ("RUN_MIGRATIONS", "true".into()),
            ("METRICS_TOKEN", self.metrics_token.clone()),
        ] {
            result.insert(key.into(), value.into());
        }
        result
    }

    pub fn generator_arguments(&self, credentials: &std::path::Path, room: &str) -> Vec<String> {
        let mut args: Vec<String> = [
            "--server",
            &format!("ws://127.0.0.1:{}/ws", self.port),
            "--room",
            room,
            "--clients",
            "30",
            "--rooms",
            "1",
            "--source-addresses",
            "30",
            "--ramp-up",
            "60",
            "--warmup",
            "30",
            "--duration",
            "1200",
            "--profile",
            "browser",
            "--receive-mode",
            "balanced",
            "--chat-interval-ms",
            "30000",
            "--diagnostics",
            "--departure",
            "explicit-leave",
            "--accounts-file",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        args.push(credentials.to_string_lossy().into_owned());
        args.extend([
            "--output-dir".into(),
            self.output.join("generator").to_string_lossy().into_owned(),
            "--run-label".into(),
            "account-room".into(),
            "--server-revision".into(),
            format!("git:{}", self.revision),
            "--generator-revision".into(),
            format!("git:{}", self.revision),
        ]);
        args
    }
}
