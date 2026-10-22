//! A loopback server for one command: owned, configured from nothing but this
//! harness, proven ready, and stopped with a record of how it stopped. Opt-ins
//! add an owned TURN relay (`TURN_E2E`), an owned HTTPS proxy
//! (`TEST_BROWSER_HTTPS`) and the passkey origin (`PASSKEY_E2E`).

use super::postgres::{create_private_directory, find_on_path, parse_port, tail, temp_root};
use super::process;
use super::service::{Service, Termination};
use super::{Stop, helper};
use crate::owned_process::Signals;
use crate::tooling::Environment;
use anyhow::{Context, Result};
use serde::Serialize;
use std::ffi::OsStr;
use std::fs::{self, OpenOptions};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, UdpSocket};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;
use tokio::time::sleep;

const DEFAULT_SERVER_PORT: &str = "3119";
const DEFAULT_MEDIA_PORT: &str = "41010";
const DEFAULT_TURN_PORT: &str = "34790";
const TURN_VERSION: &str = "4.18.0";
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);
const CHILD_GRACE: Duration = Duration::from_secs(10);
const PRESENTATION_LOG_FILTER: &str = "simplestchat=info,mediasoup=warn,sqlx::pool=warn,\
simplestchat::signaling::connection=debug,simplestchat::media::transport_manager=debug";
/// Disposable, test-only; the owned server never reads a deployment's secret.
const TEST_JWT_SECRET: &str = "disposable-test-only-jwt-secret-at-least-32-bytes";

/// Everything the opt-ins decided before any process started.
struct Plan {
    environment: Environment,
    binary: PathBuf,
    workdir: PathBuf,
    database_url: String,
    port: u16,
    media_port: u16,
    announce: Ipv4Addr,
    base_url: String,
    readiness_url: String,
    https: Option<HttpsPlan>,
    turn: Option<TurnPlan>,
    passkey: bool,
    presentation: bool,
    metrics_token: Option<String>,
    artifacts: PathBuf,
    path: String,
}

struct HttpsPlan {
    port: u16,
    openssl: PathBuf,
}

struct TurnPlan {
    port: u16,
}

/// The owned processes, stopped in reverse order of their start.
struct Harness {
    plan: Plan,
    server: Option<Service>,
    turn: Option<Service>,
    turn_config: Option<PathBuf>,
    https: Option<Service>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ServerShutdown {
    schema_version: u8,
    server_pid: Option<u32>,
    status_before_cleanup: i32,
    term_attempted: bool,
    term_sent: bool,
    kill_attempted: bool,
    kill_sent: bool,
    wait_status: Option<i32>,
    group_cleanup_passed: bool,
    wait_status_interpretation: &'static str,
    passed: bool,
    finished_at: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TurnShutdown {
    schema_version: u8,
    version: &'static str,
    pid: u32,
    wait_status: i32,
    forced: bool,
    group_cleanup_passed: bool,
    passed: bool,
    finished_at: String,
}

pub(crate) async fn run(root: &Path, command: &[String], environment: &Environment) -> Result<i32> {
    let mut signals = Signals::new()?;
    run_with_companion(
        root,
        command,
        environment,
        &mut signals,
        &mut super::NoCompanion,
    )
    .await
}

pub(crate) async fn run_with_companion(
    root: &Path,
    command: &[String],
    environment: &Environment,
    signals: &mut Signals,
    companion: &mut impl super::Companion,
) -> Result<i32> {
    let plan = plan(root, command, environment, signals).await?;
    helper::check(signals).await?;
    preflight(&plan)?;
    fs::create_dir_all(&plan.artifacts)
        .with_context(|| format!("creating {}", plan.artifacts.display()))?;
    let mut harness = Harness {
        plan,
        server: None,
        turn: None,
        turn_config: None,
        https: None,
    };
    let status = match harness
        .start_and_run_with_companion(command, signals, companion)
        .await
    {
        Ok(status) => status,
        Err(error) => {
            if let Some(stop) = error.downcast_ref::<Stop>() {
                eprintln!("{}", stop.message);
                i32::from(stop.code)
            } else if let Some(failure) = error.downcast_ref::<crate::tooling::Failure>() {
                eprintln!("Owned test companion failed.");
                i32::from(failure.code)
            } else {
                eprintln!("xtask: {error:#}");
                1
            }
        }
    };
    let status = companion.finish(status, signals).await;
    Ok(harness.finish(status, signals).await)
}

fn env(environment: &Environment, name: &str) -> Option<String> {
    environment
        .get(OsStr::new(name))
        .and_then(|value| value.to_str())
        .map(str::to_owned)
}

fn flag(environment: &Environment, name: &str) -> bool {
    env(environment, name).is_some_and(|value| value == "1")
}

async fn plan(
    root: &Path,
    command: &[String],
    environment: &Environment,
    signals: &mut Signals,
) -> Result<Plan> {
    if !flag(environment, "DISPOSABLE_TEST_DATABASE") || command.is_empty() {
        return Err(Stop::usage(
            "Set DISPOSABLE_TEST_DATABASE=1 and pass the test command to run.",
        )
        .into());
    }
    let database_url = env(environment, "DATABASE_URL").unwrap_or_default();
    if !disposable_database_url(&database_url) {
        return Err(Stop::usage(
            "DATABASE_URL must use loopback and a name ending _test; only sslmode=disable is accepted as a query.",
        )
        .into());
    }
    let binary = env(environment, "TEST_SERVER_BINARY")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("target/debug/simplestChat"));
    let workdir = env(environment, "TEST_SERVER_WORKDIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.to_path_buf());
    if !workdir.join("Cargo.toml").is_file() || !workdir.join("migrations").is_dir() {
        return Err(Stop {
            code: 1,
            message: format!(
                "TEST_SERVER_WORKDIR must hold Cargo.toml and migrations: {}",
                workdir.display()
            ),
        }
        .into());
    }
    let port_text =
        env(environment, "TEST_SERVER_PORT").unwrap_or_else(|| DEFAULT_SERVER_PORT.to_owned());
    let https_enabled = match env(environment, "TEST_BROWSER_HTTPS").as_deref() {
        None | Some("0") => false,
        Some("1") => true,
        Some(_) => return Err(Stop::usage("TEST_BROWSER_HTTPS must be 0 or 1.").into()),
    };
    let media_text =
        env(environment, "TEST_MEDIA_PORT").unwrap_or_else(|| DEFAULT_MEDIA_PORT.to_owned());
    let turn_text =
        env(environment, "TEST_TURN_PORT").unwrap_or_else(|| DEFAULT_TURN_PORT.to_owned());
    // An explicitly empty override is invalid, not a request for the default.
    let announce_text =
        env(environment, "TEST_ANNOUNCE_IP").unwrap_or_else(|| "127.0.0.1".to_owned());
    let (port, media_port, turn_port) = match (
        parse_port(&port_text),
        parse_port(&media_text),
        parse_port(&turn_text),
    ) {
        (Some(port), Some(media), Some(turn)) => (port, media, turn),
        _ => {
            return Err(Stop::usage("Test ports must be integers from 1000 through 65535.").into());
        }
    };
    let https = if https_enabled {
        let https_text = env(environment, "TEST_HTTPS_PORT")
            .unwrap_or_else(|| (u32::from(port) + 1).to_string());
        let https_port = parse_port(&https_text).filter(|candidate| *candidate != port);
        let Some(https_port) = https_port else {
            return Err(Stop::usage(
                "TEST_HTTPS_PORT must be a distinct integer from 1000 through 65535.",
            )
            .into());
        };
        let openssl = env(environment, "OPENSSL_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| root.join("target/openssl-4.0.3"))
            .join("bin/openssl");
        if !openssl.is_absolute() || !executable(&openssl) {
            return Err(Stop::usage(
                "TEST_BROWSER_HTTPS requires the checksum-pinned OpenSSL installation.",
            )
            .into());
        }
        Some(HttpsPlan {
            port: https_port,
            openssl,
        })
    } else {
        None
    };
    let path = env(environment, "PATH").unwrap_or_default();
    let turn = if flag(environment, "TURN_E2E") {
        if find_on_path(&path, "turnserver").is_none() {
            return Err(Stop::usage("TURN_E2E requires coturn (turnserver) on PATH.").into());
        }
        let mut version = Command::new("turnserver");
        version
            .arg("--version")
            .env_clear()
            .env("PATH", &path)
            .stderr(Stdio::null());
        let version =
            helper::run(version, Duration::from_secs(5), Some(4096), Some(signals)).await?;
        let version = String::from_utf8_lossy(&version.stdout).trim().to_owned();
        if version != TURN_VERSION {
            return Err(
                Stop::usage("TURN_E2E requires the verified coturn 4.18.0 configuration.").into(),
            );
        }
        Some(TurnPlan { port: turn_port })
    } else {
        None
    };
    if !executable(&binary) {
        return Err(Stop {
            code: 1,
            message: format!("Test server binary is not executable: {}", binary.display()),
        }
        .into());
    }
    let passkey = flag(environment, "PASSKEY_E2E");
    // WebAuthn relying party IDs are domains. Keep the owned HTTP bind on IPv4
    // loopback, but use localhost for both the browser origin and relying party.
    let browser_host = if passkey { "localhost" } else { "127.0.0.1" };
    let readiness_url = format!("http://127.0.0.1:{port}");
    let base_url = match &https {
        Some(https) => format!("https://{browser_host}:{}", https.port),
        None => format!("http://{browser_host}:{port}"),
    };
    let announce = match announce_refusal(&announce_text) {
        Some(reason) => {
            return Err(Stop::usage(format!("Refusing TEST_ANNOUNCE_IP: {reason}")).into());
        }
        None => announce_text.parse::<Ipv4Addr>().expect("checked above"),
    };
    let metrics_token = [
        "LIFECYCLE_E2E",
        "UI_STRESS_E2E",
        "SESSION_SOAK_E2E",
        "ACCOUNT_ROOM_E2E",
    ]
    .iter()
    .any(|name| flag(environment, name))
    .then(process::random_hex)
    .transpose()?;
    let artifacts = match env(environment, "E2E_ARTIFACTS") {
        Some(artifacts) if !artifacts.is_empty() => PathBuf::from(artifacts),
        _ => create_private_directory(&temp_root(environment)?, "simplestchat-tests.")?,
    };
    Ok(Plan {
        environment: environment.clone(),
        binary,
        workdir,
        database_url,
        port,
        media_port,
        announce,
        base_url,
        readiness_url,
        https,
        turn,
        passkey,
        presentation: flag(environment, "PRESENTATION_E2E"),
        metrics_token,
        artifacts,
        path,
    })
}

/// Probe every socket the owned processes will bind: a non-HTTP listener must
/// not be mistaken for a free port, and nothing already listening is touched.
fn preflight(plan: &Plan) -> Result<()> {
    let refuse =
        |error: std::io::Error| Stop::usage(format!("Refusing unavailable test ports: {error}"));
    let _tcp = TcpListener::bind(("127.0.0.1", plan.port)).map_err(refuse)?;
    let _udp = UdpSocket::bind(("0.0.0.0", plan.media_port)).map_err(refuse)?;
    let _turn = match &plan.turn {
        Some(turn) => Some((
            TcpListener::bind(("127.0.0.1", turn.port)).map_err(refuse)?,
            UdpSocket::bind(("127.0.0.1", turn.port)).map_err(refuse)?,
        )),
        None => None,
    };
    let _https = match &plan.https {
        Some(https) => Some(TcpListener::bind(("127.0.0.1", https.port)).map_err(refuse)?),
        None => None,
    };
    Ok(())
}

impl Harness {
    #[cfg(test)]
    async fn start_and_run(&mut self, command: &[String], signals: &mut Signals) -> Result<i32> {
        self.start_and_run_with_companion(command, signals, &mut super::NoCompanion)
            .await
    }
    async fn start_and_run_with_companion(
        &mut self,
        command: &[String],
        signals: &mut Signals,
        companion: &mut impl super::Companion,
    ) -> Result<i32> {
        let artifacts = self.plan.artifacts.clone();
        let mut server_env: Vec<(String, String)> = vec![("PATH".into(), self.plan.path.clone())];
        if self.plan.presentation {
            // A fixed, owned diagnostic profile; inherited operator log filters
            // stay out of the clean environment.
            server_env.push(("RUST_LOG".into(), PRESENTATION_LOG_FILTER.into()));
        }
        if self.plan.passkey {
            server_env.push(("WEBAUTHN_RP_ID".into(), "localhost".into()));
            server_env.push(("WEBAUTHN_ORIGIN".into(), self.plan.base_url.clone()));
        }
        if let Some(token) = &self.plan.metrics_token {
            server_env.push(("METRICS_TOKEN".into(), token.clone()));
        }
        if let Some(port) = self.plan.turn.as_ref().map(|turn| turn.port) {
            // This opt-in creates its own relay and credentials. Never accept
            // deployment TURN settings or listen for clients outside loopback.
            let secret = process::random_hex()?;
            server_env.push((
                "TURN_URLS".into(),
                format!("turn:127.0.0.1:{port}?transport=udp"),
            ));
            server_env.push(("TURN_SECRET".into(), secret.clone()));
            server_env.push(("TURN_TTL".into(), "60".into()));
            let config = self.write_turn_config(port, &secret)?;
            let log = log_file(&artifacts.join("turn.log"))?;
            let mut child = Command::new("turnserver");
            child
                .arg("-c")
                .arg(&config)
                .env_clear()
                .env("PATH", &self.plan.path)
                .stdin(Stdio::null())
                .stdout(Stdio::from(log.try_clone()?))
                .stderr(Stdio::from(log));
            helper::check(signals).await?;
            self.turn = Some(Service::spawn(child).context("starting turnserver")?);
            self.await_turn_ready(port, signals).await?;
        }
        let log = log_file(&artifacts.join("server.log"))?;
        let mut server = Command::new(&self.plan.binary);
        server
            .current_dir(&self.plan.workdir)
            .env_clear()
            .envs(server_env)
            .env("DATABASE_URL", &self.plan.database_url)
            .env("BIND_ADDR", "127.0.0.1")
            .env("PORT", self.plan.port.to_string())
            .env("ANNOUNCE_IP", self.plan.announce.to_string())
            .env("MEDIA_WORKERS", "1")
            .env("WEBRTC_SERVER_PORT_BASE", self.plan.media_port.to_string())
            .env("ALLOWED_ORIGINS", &self.plan.base_url)
            .env("REGISTRATION_ENABLED", "true")
            .env("ALLOW_AD_HOC_ROOMS", "true")
            .env("REGISTRATIONS_PER_IP_PER_HOUR", "100")
            .env("AUTH_REQUESTS_PER_MINUTE", "300")
            .env("JWT_SECRET", TEST_JWT_SECRET)
            .env("RUN_MIGRATIONS", "true")
            .stdin(Stdio::null())
            .stdout(Stdio::from(log.try_clone()?))
            .stderr(Stdio::from(log));
        helper::check(signals).await?;
        let server = Service::spawn(server)
            .with_context(|| format!("starting {}", self.plan.binary.display()))?;
        let server_pid = server.pid();
        self.server = Some(server);
        self.await_server_ready(signals).await?;
        if let Some(https) = &self.plan.https {
            let log = log_file(&artifacts.join("https-proxy.log"))?;
            let mut child = Command::new("node");
            child
                .arg("build/test-https-proxy.mjs")
                .arg(self.plan.port.to_string())
                .arg(https.port.to_string())
                .arg(&https.openssl)
                .env_clear()
                .env("PATH", &self.plan.path)
                .env(
                    "TMPDIR",
                    env(&self.plan.environment, "TMPDIR").unwrap_or_else(|| "/tmp".into()),
                )
                .stdin(Stdio::null())
                .stdout(Stdio::from(log.try_clone()?))
                .stderr(Stdio::from(log));
            helper::check(signals).await?;
            self.https = Some(Service::spawn(child).context("starting the HTTPS proxy")?);
            self.await_https_ready(signals).await?;
        }
        let status = self
            .run_child_with_companion(command, server_pid, signals, companion)
            .await?;
        if status != 0 {
            return Ok(status);
        }
        if exited(self.https.as_ref())? {
            eprintln!("Owned HTTPS proxy exited while the test command was running.");
            return Ok(1);
        }
        if exited(self.turn.as_ref())? {
            eprintln!("Owned TURN relay exited while the test command was running.");
            return Ok(1);
        }
        if exited(self.server.as_ref())? {
            eprintln!("Test server exited while the test command was running.");
            return Ok(1);
        }
        Ok(0)
    }

    fn write_turn_config(&mut self, port: u16, secret: &str) -> Result<PathBuf> {
        let artifacts = self.plan.artifacts.display();
        let announce = self.plan.announce;
        let mut text = String::new();
        text.push_str(&format!(
            "listening-ip=127.0.0.1\nlistening-port={port}\nrelay-ip={announce}\n\
realm=simplestchat-test.invalid\nuse-auth-secret\nstatic-auth-secret={secret}\n\
userdb={artifacts}/turn.sqlite\npidfile={artifacts}/turn.pid\nno-tls\nno-tcp-relay\n\
no-multicast-peers\nno-rfc5780\ndenied-peer-ip=0.0.0.0-255.255.255.255\n\
denied-peer-ip=::-ffff:ffff:ffff:ffff:ffff:ffff:ffff:ffff\nallowed-peer-ip={announce}\n\
relay-threads=1\nuser-quota=8\ntotal-quota=64\nlog-file=stdout\nlog-min-level=warning\n"
        ));
        if announce.is_loopback() {
            text.push_str("allow-loopback-peers\n");
        }
        for _ in 0..64 {
            let suffix: String = process::random_hex()?.chars().take(6).collect();
            let candidate = self.plan.artifacts.join(format!("turn-config.{suffix}"));
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&candidate)
            {
                Ok(mut file) => {
                    self.turn_config = Some(candidate.clone());
                    use std::io::Write;
                    file.write_all(text.as_bytes())?;
                    return Ok(candidate);
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error).context("writing the TURN configuration"),
            }
        }
        anyhow::bail!("could not name the TURN configuration")
    }

    /// Readiness uses the local TCP listener; the browser must additionally
    /// prove authenticated relay allocation and selected relay candidates.
    async fn await_turn_ready(&mut self, port: u16, signals: &mut Signals) -> Result<()> {
        for attempt in 0..40 {
            if exited(self.turn.as_ref())? {
                anyhow::bail!("Owned TURN relay did not become ready");
            }
            if helper::wait(signals, tcp_connects(port)).await? {
                return Ok(());
            }
            if attempt == 39 {
                anyhow::bail!("Owned TURN relay did not become ready");
            }
            helper::wait(signals, sleep(Duration::from_millis(100))).await?;
        }
        Ok(())
    }

    async fn await_server_ready(&mut self, signals: &mut Signals) -> Result<()> {
        let url = format!("{}/api/rooms", self.plan.readiness_url);
        for _ in 0..120 {
            if exited(self.server.as_ref())? {
                return Err(Stop {
                    code: 1,
                    message: "Test server exited before readiness.".into(),
                }
                .into());
            }
            if http_ok(&url, false, &self.plan.path, signals).await? {
                return Ok(());
            }
            helper::wait(signals, sleep(Duration::from_millis(250))).await?;
        }
        Err(Stop {
            code: 1,
            message: "Test server failed database-backed API readiness.".into(),
        }
        .into())
    }

    async fn await_https_ready(&mut self, signals: &mut Signals) -> Result<()> {
        let url = format!("{}/api/rooms", self.plan.base_url);
        for _ in 0..80 {
            if exited(self.https.as_ref())? {
                return Err(Stop {
                    code: 1,
                    message: "Owned HTTPS proxy exited before readiness.".into(),
                }
                .into());
            }
            if http_ok(&url, true, &self.plan.path, signals).await? {
                return Ok(());
            }
            helper::wait(signals, sleep(Duration::from_millis(250))).await?;
        }
        Err(Stop {
            code: 1,
            message: "Owned HTTPS proxy failed readiness.".into(),
        }
        .into())
    }

    /// Run the test command with the owned environment exported and nothing
    /// of an earlier run's credentials inherited; INT or TERM stops it and
    /// reports 130 or 143.
    #[cfg(test)]
    async fn run_child(
        &mut self,
        command: &[String],
        server_pid: u32,
        signals: &mut Signals,
    ) -> Result<i32> {
        self.run_child_with_companion(command, server_pid, signals, &mut super::NoCompanion)
            .await
    }
    async fn run_child_with_companion(
        &mut self,
        command: &[String],
        server_pid: u32,
        signals: &mut Signals,
        companion: &mut impl super::Companion,
    ) -> Result<i32> {
        let mut child = std::process::Command::new(&command[0]);
        child.args(&command[1..]);
        child
            .env_clear()
            .envs(&self.plan.environment)
            .env_remove("TEST_METRICS_TOKEN")
            .env_remove("TEST_SERVER_PID");
        child
            .env("BASE_URL", &self.plan.base_url)
            .env("TEST_ANNOUNCE_IP", self.plan.announce.to_string())
            .env("E2E_ARTIFACTS", &self.plan.artifacts)
            .env("TEST_SERVER_PID", server_pid.to_string())
            .env("COMMUNITY_E2E", "1")
            .env("TEST_DATABASE_URL", &self.plan.database_url)
            .env("TEST_SERVER_BINARY", &self.plan.binary)
            .env("TEST_SERVER_WORKDIR", &self.plan.workdir);
        if let Some(token) = &self.plan.metrics_token {
            child.env("TEST_METRICS_TOKEN", token);
        }
        process::run_test_child_with_companion(child, CHILD_GRACE, signals, companion).await
    }

    /// Cleanup cannot be cancelled midway. Observe a pending interruption only
    /// after every acquired resource has received its retirement attempt.
    async fn finish(&mut self, status: i32, signals: &mut Signals) -> i32 {
        let mut status = self.cleanup(status).await;
        if status == 0
            && let Some(code) = signals.pending().await
        {
            status = i32::from(code);
        }
        status
    }

    async fn cleanup(&mut self, mut status: i32) -> i32 {
        let artifacts = self.plan.artifacts.clone();
        if let Some(proxy) = self.https.take() {
            let ended = proxy.stop(SHUTDOWN_GRACE).await;
            if ended.kill_attempted {
                eprintln!("Owned HTTPS proxy needed forced shutdown.");
                worsen(&mut status);
            }
            if ended.wait_status != Some(0) || !ended.group_cleanup_passed {
                eprintln!("Owned HTTPS proxy exited unsuccessfully.");
                worsen(&mut status);
            }
        }
        let (server_pid, ended) = match self.server.take() {
            Some(server) => {
                let pid = Some(server.pid());
                // Bound shutdown; never kill by executable name or target another server.
                (pid, server.stop(SHUTDOWN_GRACE).await)
            }
            None => (None, Termination::default()),
        };
        let report = ServerShutdown {
            schema_version: 1,
            server_pid,
            status_before_cleanup: status,
            term_attempted: ended.term_attempted,
            term_sent: ended.term_sent,
            kill_attempted: ended.kill_attempted,
            kill_sent: ended.kill_sent,
            wait_status: ended.wait_status,
            group_cleanup_passed: ended.group_cleanup_passed,
            wait_status_interpretation: interpretation(ended.wait_status),
            passed: ended.wait_status == Some(0)
                && !ended.kill_attempted
                && ended.group_cleanup_passed,
            finished_at: now(),
        };
        let retained = write_evidence(&artifacts.join("server-shutdown.json"), &report);
        if !retained {
            eprintln!("Could not retain the owned test server shutdown report.");
        }
        if !retained || !report.passed {
            eprintln!(
                "Owned test server shutdown was unsuccessful or its evidence was unavailable."
            );
            worsen(&mut status);
        }
        if let Some(relay) = self.turn.take() {
            let pid = relay.pid();
            let ended = relay.stop(SHUTDOWN_GRACE).await;
            if ended.kill_attempted {
                eprintln!("Owned TURN relay needed forced shutdown.");
                worsen(&mut status);
            }
            let wait_status = ended.wait_status.unwrap_or(-1);
            if (wait_status != 0 && wait_status != 143) || !ended.group_cleanup_passed {
                eprintln!("Owned TURN relay exited unsuccessfully: {wait_status}");
                worsen(&mut status);
            }
            let report = TurnShutdown {
                schema_version: 1,
                version: TURN_VERSION,
                pid,
                wait_status,
                forced: ended.kill_attempted,
                group_cleanup_passed: ended.group_cleanup_passed,
                passed: !ended.kill_attempted
                    && (wait_status == 0 || wait_status == 143)
                    && ended.group_cleanup_passed,
                finished_at: now(),
            };
            if !write_evidence(&artifacts.join("turn-shutdown.json"), &report) {
                eprintln!("Could not retain the owned TURN relay shutdown report.");
                worsen(&mut status);
            }
        }
        // The generated shared secret is needed only while the owned relay is alive.
        if let Some(config) = self.turn_config.take()
            && fs::remove_file(config).is_err()
        {
            eprintln!("Could not remove the owned TURN test configuration.");
            worsen(&mut status);
        }
        println!(
            "Test server log and browser artifacts: {}",
            artifacts.display()
        );
        if status != 0 {
            tail(&artifacts.join("server.log"), 80);
        }
        status
    }
}

fn worsen(status: &mut i32) {
    if *status == 0 {
        *status = 1;
    }
}

fn exited(service: Option<&Service>) -> Result<bool> {
    service.map_or(Ok(false), Service::exited)
}

fn executable(path: &Path) -> bool {
    fs::metadata(path)
        .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

fn log_file(path: &Path) -> Result<fs::File> {
    OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("opening {}", path.display()))
}

/// A fresh evidence file; an earlier run's evidence is never overwritten.
fn write_evidence<T: Serialize>(path: &Path, report: &T) -> bool {
    let Ok(text) = serde_json::to_string_pretty(report) else {
        return false;
    };
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .and_then(|mut file| {
            use std::io::Write;
            file.write_all(text.as_bytes())?;
            file.write_all(b"\n")
        })
        .is_ok()
}

/// Bash cannot distinguish signal termination from an explicit high exit code
/// (or exit 127 from unavailable child status); retain that ambiguity.
pub(crate) fn interpretation(wait_status: Option<i32>) -> &'static str {
    match wait_status {
        None => "not_waited",
        Some(0) => "exited_zero",
        Some(127) => "unavailable_or_exit_127",
        Some(status) if status >= 128 => "signal_or_high_exit_code",
        Some(_) => "nonzero_exit",
    }
}

fn now() -> String {
    let elapsed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let seconds = elapsed.as_secs();
    let millis = elapsed.subsec_millis();
    let days = seconds / 86_400;
    let (year, month, day) = civil_from_days(i64::try_from(days).unwrap_or(0));
    let rest = seconds % 86_400;
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{millis:03}Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

/// Days since 1970-01-01 to a proleptic Gregorian civil date (Howard Hinnant).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    (
        year,
        u32::try_from(m).unwrap_or(1),
        u32::try_from(d).unwrap_or(1),
    )
}

/// `curl --fail --silent --max-time 2` with a clean environment, insecure for
/// the owned proxy's self-signed certificate.
async fn http_ok(url: &str, insecure: bool, path: &str, signals: &mut Signals) -> Result<bool> {
    let mut curl = Command::new("curl");
    curl.args([
        "--fail",
        "--silent",
        "--max-time",
        "2",
        "--output",
        "/dev/null",
    ]);
    if insecure {
        curl.arg("--insecure");
    }
    curl.arg(url)
        .env_clear()
        .env("PATH", path)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    Ok(
        helper::run(curl, Duration::from_secs(5), None, Some(signals))
            .await?
            .status
            == 0,
    )
}

async fn tcp_connects(port: u16) -> bool {
    let address = SocketAddr::from(([127, 0, 0, 1], port));
    tokio::time::timeout(
        Duration::from_millis(500),
        tokio::net::TcpStream::connect(address),
    )
    .await
    .is_ok_and(|connection| connection.is_ok())
}

/// Reject remote hosts, query-string host overrides and non-test database names:
/// `postgres[ql]://[user@](127.0.0.1|localhost|[::1])[:port]/<name>_test[?sslmode=disable]`.
pub(crate) fn disposable_database_url(url: &str) -> bool {
    let Some(rest) = url
        .strip_prefix("postgresql://")
        .or_else(|| url.strip_prefix("postgres://"))
    else {
        return false;
    };
    let (authority, remainder) = match rest.split_once('/') {
        Some(split) => split,
        None => return false,
    };
    let host_port = match authority.rsplit_once('@') {
        Some((user, host_port)) if !user.is_empty() && !user.contains('/') => host_port,
        Some(_) => return false,
        None => authority,
    };
    let host = ["127.0.0.1", "localhost", "[::1]"]
        .iter()
        .find_map(|host| host_port.strip_prefix(host));
    let Some(after_host) = host else {
        return false;
    };
    if let Some(port) = after_host.strip_prefix(':') {
        if port.is_empty() || !port.bytes().all(|byte| byte.is_ascii_digit()) {
            return false;
        }
    } else if !after_host.is_empty() {
        return false;
    }
    let (name, query) = match remainder.split_once('?') {
        Some((name, query)) => (name, Some(query)),
        None => (remainder, None),
    };
    let named = !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        && name.ends_with("_test");
    named && query.is_none_or(|query| query == "sslmode=disable")
}

/// Why `TEST_ANNOUNCE_IP` cannot be announced: not a literal IPv4 address, or
/// not an address of this machine (`127.0.0.1` is the one loopback accepted;
/// binding proves an interface owns any other).
pub(crate) fn announce_refusal(text: &str) -> Option<String> {
    let Ok(address) = text.parse::<Ipv4Addr>() else {
        return Some("expected a literal IPv4 address".into());
    };
    let assigned = if address.is_loopback() || address.is_unspecified() {
        address == Ipv4Addr::LOCALHOST
    } else {
        UdpSocket::bind((address, 0)).is_ok()
    };
    (!assigned).then(|| "address is not assigned to a local network interface".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disposable_database_urls_are_loopback_test_databases_without_host_overrides() {
        for accepted in [
            "postgres://test_owner@127.0.0.1:15434/simplestchat_test?sslmode=disable",
            "postgresql://localhost/chat_test",
            "postgres://user@[::1]:5432/a_test",
        ] {
            assert!(disposable_database_url(accepted), "{accepted}");
        }
        for refused in [
            "",
            "postgres://test_owner@example.test/simplestchat_test",
            "postgres://test_owner@127.0.0.1/production",
            "postgres://test_owner@127.0.0.1/simplestchat_test?hostaddr=203.0.113.10&sslmode=disable",
            "postgres://test_owner@127.0.0.1/simplestchat_test?sslmode=require",
            "mysql://127.0.0.1/x_test",
            "postgres://127.0.0.1:/x_test",
            "postgres://127.0.0.1/x_test/extra",
        ] {
            assert!(!disposable_database_url(refused), "{refused}");
        }
    }

    #[test]
    fn announcements_must_be_literal_assigned_ipv4_addresses() {
        assert_eq!(announce_refusal("127.0.0.1"), None);
        for refused in [
            "",
            "localhost",
            "127.0.0.1.example.test",
            "::1",
            "127.1",
            " 127.0.0.1",
        ] {
            assert_eq!(
                announce_refusal(refused).as_deref(),
                Some("expected a literal IPv4 address"),
                "{refused:?}"
            );
        }
        for refused in ["0.0.0.0", "127.0.0.2", "203.0.113.77"] {
            assert_eq!(
                announce_refusal(refused).as_deref(),
                Some("address is not assigned to a local network interface"),
                "{refused}"
            );
        }
    }

    #[test]
    fn shutdown_interpretations_keep_the_shell_ambiguity() {
        assert_eq!(interpretation(None), "not_waited");
        assert_eq!(interpretation(Some(0)), "exited_zero");
        assert_eq!(interpretation(Some(127)), "unavailable_or_exit_127");
        assert_eq!(interpretation(Some(143)), "signal_or_high_exit_code");
        assert_eq!(interpretation(Some(23)), "nonzero_exit");
    }

    #[test]
    fn timestamps_are_utc_rfc3339_with_milliseconds() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(20_746), (2026, 10, 20));
        let stamp = now();
        assert_eq!(stamp.len(), 24);
        assert!(stamp.ends_with('Z') && stamp.as_bytes()[10] == b'T');
    }
}

#[cfg(test)]
#[path = "server_tests.rs"]
mod lifecycle_tests;
