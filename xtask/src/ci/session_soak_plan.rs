//! Fixed hosted soak selection. Caller-supplied service identity is not a
//! discovery query or authority to delete a container owned by the runner.

use crate::tooling::{Environment, Failure, Result};
use std::{path::PathBuf, time::Duration};

pub(super) const IMAGE: &str = "docker.io/library/postgres:18.6-bookworm@sha256:\
3725f4e2499eef5134592b3b4ab79a543ed7f8e533b05b5b637af926630f6650";
pub(super) const ENDPOINT: &str = "unix:///var/run/docker.sock";
pub(super) const DATABASE: &str = "postgres://test_owner:disposable-ci-password@127.0.0.1:\
5432/simplestchat_test?sslmode=disable";
pub(super) const DELAY: Duration = Duration::from_secs(600);
pub(super) const OUTAGE: Duration = Duration::from_secs(8);
pub(super) const WATCHDOG: Duration = Duration::from_secs(37 * 60);
pub(super) const STEP: Duration = Duration::from_secs(40 * 60);
pub(super) const RESTORE: Duration = Duration::from_secs(90);
// Linux helper retirement: TERM grace2 + terminal5 + reader2 + final Drop5.
pub(super) const CLI_SETTLE: Duration = Duration::from_secs(14);
// Workload retirement <=10s grace+5s terminal+5s Drop; server <=5s+5s+5s.
// These are scheduling allowances, not a bound on OS/filesystem operations.
pub(super) const HARNESS_RESERVE: Duration = Duration::from_secs(40);

pub(super) fn failed(code: u8) -> Failure {
    Failure::new(code, "Session soak supervision failed.")
}
pub(super) fn identifier(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|v| v.is_ascii_digit() || (b'a'..=b'f').contains(&v))
}
pub(super) fn image_id(value: &str) -> Option<&str> {
    let value = value.strip_prefix("sha256:").unwrap_or(value);
    identifier(value).then_some(value)
}
pub(super) struct Context {
    pub root: PathBuf,
    pub environment: Environment,
    pub id: String,
    pub network: String,
    pub port: String,
}
impl Context {
    pub fn new(root: &std::path::Path, mut environment: Environment, system: &str) -> Result<Self> {
        let text = |name| super::super::runner::text(&environment, name);
        let id = text("SOAK_POSTGRES_ID");
        let network = text("SOAK_POSTGRES_NETWORK");
        let port = text("SOAK_POSTGRES_PORT");
        let artifacts = PathBuf::from(text("E2E_ARTIFACTS"));
        if !super::super::runner::authorized(&environment, system)
            || !root.is_absolute()
            || !identifier(id)
            || network.is_empty()
            || network.len() > 128
            || !network
                .bytes()
                .all(|v| v.is_ascii_alphanumeric() || b"_.-".contains(&v))
            || port != "5432"
            || text("SESSION_SOAK_E2E") != "1"
            || text("DISPOSABLE_TEST_DATABASE") != "1"
            || text("DATABASE_URL") != DATABASE
            || text("TEST_DATABASE_URL") != DATABASE
            || !matches!(text("DOCKER_HOST"), "" | ENDPOINT)
            || !matches!(text("DOCKER_CONTEXT"), "" | "default")
            || !text("DOCKER_TLS_VERIFY").is_empty()
            || !text("DOCKER_CERT_PATH").is_empty()
            || !matches!(text("SESSION_SOAK_PROFILE"), "" | "full")
            || ["TURN_E2E", "TEST_BROWSER_HTTPS", "PASSKEY_E2E"]
                .iter()
                .any(|name| matches!(text(name), "1" | "true"))
            || !artifacts.is_absolute()
        {
            return Err(failed(2));
        }
        let id = id.to_owned();
        let network = network.to_owned();
        let port = port.to_owned();
        environment.insert("SESSION_SOAK_PROFILE".into(), "full".into());
        environment.insert("DOCKER_HOST".into(), ENDPOINT.into());
        for key in ["DOCKER_CONTEXT", "DOCKER_TLS_VERIFY", "DOCKER_CERT_PATH"] {
            environment.remove(std::ffi::OsStr::new(key));
        }
        Ok(Self {
            root: root.into(),
            environment,
            id,
            network,
            port,
        })
    }
    pub fn workload(&self) -> Vec<String> {
        ["npm", "--prefix", "web/e2e", "run", "test:session-soak"]
            .map(str::to_owned)
            .into()
    }
}
