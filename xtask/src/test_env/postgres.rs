//! A disposable PostgreSQL cluster for one command: created in a private
//! directory, bound to loopback only, stopped afterwards and retained for
//! diagnostics. Never a shared service, never a Unix socket.

use super::process;
use super::{Stop, helper};
use crate::owned_process::Signals;
use anyhow::{Context, Result};
use std::fs::{self, OpenOptions};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

const DEFAULT_PORT: &str = "15434";
const DATABASE: &str = "simplestchat_test";
const OWNER: &str = "test_owner";
const TOOLS: [&str; 3] = ["initdb", "pg_ctl", "createdb"];
const CHILD_GRACE: Duration = Duration::from_secs(10);
const CONTAINER_MARKERS: [&str; 2] = ["/.dockerenv", "/run/.containerenv"];

/// The `postgres` account the disposable act runner hands the cluster to when
/// the job runs as root; only the owned database changes identity.
struct RootRunner {
    uid: String,
    gid: String,
}

struct Cluster {
    path: String,
    directory: PathBuf,
    data: PathBuf,
    identity: Option<RootRunner>,
    start_attempted: bool,
    start_confirmed: bool,
}

pub(crate) async fn run(_root: &Path, command: &[String]) -> Result<i32> {
    if command.is_empty() {
        return Err(
            Stop::usage("Usage: cargo run -q -p xtask -- test-postgres COMMAND [ARG ...]").into(),
        );
    }
    let mut signals = Signals::new()?;
    let port_text = std::env::var("TEST_POSTGRES_PORT").unwrap_or_else(|_| DEFAULT_PORT.to_owned());
    let port = parse_port(&port_text).ok_or_else(|| {
        Stop::usage("TEST_POSTGRES_PORT must be an integer from 1000 through 65535.")
    })?;
    let path = std::env::var("PATH").unwrap_or_default();
    for tool in TOOLS {
        if find_on_path(&path, tool).is_none() {
            return Err(
                Stop::usage(format!("Missing {tool}; put the PostgreSQL tools on PATH.")).into(),
            );
        }
    }
    let identity = root_runner(&path, &mut signals).await?;
    if let Err(error) = std::net::TcpListener::bind(("127.0.0.1", port)) {
        return Err(Stop::usage(format!(
            "Refusing unavailable PostgreSQL test port: {error}"
        ))
        .into());
    }
    let temp_root = temp_root()?;
    let directory = create_private_directory(&temp_root, "simplestchat-postgres.")?;
    let mut cluster = Cluster {
        path,
        data: directory.join("data"),
        directory,
        identity,
        start_attempted: false,
        start_confirmed: false,
    };
    let status = match cluster.start_and_run(port, command, &mut signals).await {
        Ok(status) => status,
        Err(error) => {
            if let Some(stop) = error.downcast_ref::<Stop>() {
                eprintln!("{}", stop.message);
                i32::from(stop.code)
            } else {
                eprintln!("xtask: {error:#}");
                1
            }
        }
    };
    Ok(cluster.finish(status, &mut signals).await)
}

impl Cluster {
    async fn start_and_run(
        &mut self,
        port: u16,
        command: &[String],
        signals: &mut Signals,
    ) -> Result<i32> {
        let initdb_log = self.directory.join("initdb.log");
        let postgres_log = self.directory.join("postgres.log");
        if let Some(identity) = &self.identity {
            // Transfer only this newly created private directory, never its shared parent.
            let owner = format!("{}:{}", identity.uid, identity.gid);
            let mut chown = Command::new("chown");
            chown.arg(&owner).arg(&self.directory);
            let chown = helper::run(chown, Duration::from_secs(10), None, Some(signals)).await?;
            if chown.status != 0 {
                return Ok(chown.status);
            }
            let writable = helper::run(
                self.postgres_command("test", &["-w", &self.directory.to_string_lossy()]),
                Duration::from_secs(10),
                None,
                Some(signals),
            )
            .await?;
            if writable.status != 0 {
                return Err(Stop::usage(
                    "The postgres account cannot access its cluster; use a traversable RUNNER_TEMP/TMPDIR parent.",
                )
                .into());
            }
        }
        // Owner-only logs from the start: the tools inherit the caller's umask.
        for log in [&initdb_log, &postgres_log] {
            OpenOptions::new()
                .create(true)
                .append(true)
                .mode(0o600)
                .open(log)
                .with_context(|| format!("creating {}", log.display()))?;
        }
        let data = self.data.to_string_lossy().into_owned();
        let status = self
            .step(
                "initdb",
                &[
                    "-D",
                    &data,
                    "-U",
                    OWNER,
                    "--auth-local=reject",
                    "--auth-host=trust",
                    "--encoding=UTF8",
                    "--no-locale",
                ],
                &initdb_log,
                true,
                Some(signals),
            )
            .await?;
        if status != 0 {
            return Ok(status);
        }
        let options = format!("-h 127.0.0.1 -p {port} -c unix_socket_directories=''");
        let status = self
            .step(
                "pg_ctl",
                &[
                    "-D",
                    &data,
                    "-l",
                    &postgres_log.to_string_lossy(),
                    "-w",
                    "-t",
                    "15",
                    "-o",
                    &options,
                    "start",
                ],
                &initdb_log,
                false,
                Some(signals),
            )
            .await?;
        if status != 0 {
            return Ok(status);
        }
        self.start_confirmed = true;
        let port_text = port.to_string();
        let status = self
            .step(
                "createdb",
                &[
                    "-h",
                    "127.0.0.1",
                    "-p",
                    &port_text,
                    "-U",
                    OWNER,
                    "--maintenance-db=postgres",
                    DATABASE,
                ],
                &initdb_log,
                false,
                Some(signals),
            )
            .await?;
        if status != 0 {
            return Ok(status);
        }
        if !matches!(self.state(Some(signals)).await?, State::Running) {
            return Ok(1);
        }
        let database_url =
            format!("postgres://{OWNER}@127.0.0.1:{port}/{DATABASE}?sslmode=disable");
        let status = self.run_child(command, &database_url, signals).await?;
        if status != 0 {
            return Ok(status);
        }
        if !matches!(self.state(Some(signals)).await?, State::Running) {
            eprintln!("Owned PostgreSQL exited while the test command was running.");
            return Ok(1);
        }
        Ok(0)
    }

    /// Run the test command in its own process group with libpq defaults
    /// removed, so an inherited service can never receive its writes; a
    /// signal to this process stops the whole group and reports 130 or 143.
    async fn run_child(
        &self,
        command: &[String],
        database_url: &str,
        signals: &mut Signals,
    ) -> Result<i32> {
        let mut child = std::process::Command::new(&command[0]);
        child.args(&command[1..]);
        child.env_clear();
        for (name, value) in std::env::vars_os() {
            if !name.to_string_lossy().starts_with("PG") {
                child.env(name, value);
            }
        }
        child
            .env("DATABASE_URL", database_url)
            .env("TEST_DATABASE_URL", database_url)
            .env("DISPOSABLE_TEST_DATABASE", "1");
        process::run_test_child(child, CHILD_GRACE, signals).await
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

    async fn cleanup(&mut self, status: i32) -> i32 {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        self.cleanup_observing(status, || tokio::time::Instant::now() < deadline)
            .await
    }

    async fn cleanup_observing(
        &mut self,
        mut status: i32,
        mut continue_settlement: impl FnMut() -> bool,
    ) -> i32 {
        let data = self.data.to_string_lossy().into_owned();
        let postgres_log = self.directory.join("postgres.log");
        if self.start_attempted {
            // pg_ctl detaches postmaster before its pidfile exists. A failed
            // start leaves that gap unproven, even if every status says 3.
            if !self.start_confirmed {
                eprintln!(
                    "Owned PostgreSQL startup is unproven; detached daemon closure cannot be confirmed."
                );
                worsen(&mut status);
            }
            loop {
                let before = self.state(None).await;
                if before.is_err() {
                    eprintln!(
                        "Owned PostgreSQL shutdown status is unproven; attempting its exact data-directory stop."
                    );
                    worsen(&mut status);
                }
                if !matches!(before, Ok(State::Stopped)) {
                    let fast = self
                        .step(
                            "pg_ctl",
                            &["-D", &data, "-w", "-t", "15", "stop", "-m", "fast"],
                            &postgres_log,
                            false,
                            None,
                        )
                        .await;
                    if !matches!(fast, Ok(0)) {
                        eprintln!(
                            "Fast shutdown failed; stopping only the owned test cluster immediately."
                        );
                        worsen(&mut status);
                        let immediate = self
                            .step(
                                "pg_ctl",
                                &["-D", &data, "-w", "-t", "5", "stop", "-m", "immediate"],
                                &postgres_log,
                                false,
                                None,
                            )
                            .await;
                        if !matches!(immediate, Ok(0)) {
                            eprintln!("Could not stop the owned test cluster.");
                        }
                    }
                }
                if self.start_confirmed || !continue_settlement() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            if !matches!(self.state(None).await, Ok(State::Stopped)) {
                eprintln!("Owned PostgreSQL shutdown could not be confirmed.");
                worsen(&mut status);
            }
        }
        println!(
            "Disposable PostgreSQL logs: {}/{{initdb,postgres}}.log",
            self.directory.display()
        );
        if status != 0 {
            for name in ["initdb.log", "postgres.log"] {
                tail(&self.directory.join(name), 40);
            }
        }
        // Retain the private directory for diagnostics; never delete user data here.
        status
    }

    /// Status 3 is a current not-running observation. It cannot settle an
    /// unsuccessful detached startup; that uncertainty is sticky in cleanup.
    /// Other failures leave ownership uncertain and must not skip a stop.
    async fn state(&self, signals: Option<&mut Signals>) -> Result<State> {
        let mut command =
            self.postgres_command("pg_ctl", &["-D", &self.data.to_string_lossy(), "status"]);
        command.stdout(Stdio::null()).stderr(Stdio::null());
        match helper::run(command, Duration::from_secs(5), None, signals)
            .await?
            .status
        {
            0 => Ok(State::Running),
            3 => Ok(State::Stopped),
            _ => anyhow::bail!("Owned PostgreSQL status could not be determined."),
        }
    }

    /// Logs stream to private files; setup commands have an outer deadline and
    /// a reserved owned group independently of pg_ctl's daemon/data-dir owner.
    async fn step(
        &mut self,
        tool: &str,
        args: &[&str],
        log: &Path,
        truncate: bool,
        signals: Option<&mut Signals>,
    ) -> Result<i32> {
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .append(!truncate)
            .truncate(truncate)
            .mode(0o600)
            .open(log)
            .with_context(|| format!("opening {}", log.display()))?;
        let errors = file.try_clone()?;
        let mut command = self.postgres_command(tool, args);
        command
            .stdout(Stdio::from(file))
            .stderr(Stdio::from(errors));
        // initdb may initialize catalogs; pg_ctl's own -t remains 15s/5s.
        let timeout = if tool == "initdb" {
            Duration::from_secs(60)
        } else {
            Duration::from_secs(30)
        };
        let output = if tool == "pg_ctl" && args.contains(&"start") {
            helper::run_dispatched(command, timeout, None, signals, &mut self.start_attempted)
                .await?
        } else {
            helper::run(command, timeout, None, signals).await?
        };
        Ok(output.status)
    }

    /// A PostgreSQL tool with every `PG*` variable, service file and inherited
    /// locale out of its environment; under the disposable root runner it runs
    /// as the `postgres` account, which clears PAM's additions again itself.
    fn postgres_command(&self, tool: &str, args: &[&str]) -> Command {
        let mut command = if self.identity.is_some() {
            let mut command = Command::new("runuser");
            command.current_dir(&self.directory);
            command.args(["-u", "postgres", "--", "env", "-i"]);
            command.arg(format!("PATH={}", self.path));
            command.arg("LC_ALL=C");
            command.arg(tool);
            command
        } else {
            Command::new(tool)
        };
        command.args(args);
        command
            .env_clear()
            .env("PATH", &self.path)
            .env("LC_ALL", "C");
        command
    }
}

/// `TEST_POSTGRES_PORT`: four or five digits without a leading zero, at most
/// 65535; an explicitly empty value is invalid, not a request for the default.
pub(crate) fn parse_port(text: &str) -> Option<u16> {
    if !(4..=5).contains(&text.len())
        || text.starts_with('0')
        || !text.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    u16::try_from(text.parse::<u32>().ok()?).ok()
}

/// The first executable called `tool` on `path`.
pub(crate) fn find_on_path(path: &str, tool: &str) -> Option<PathBuf> {
    path.split(':')
        .map(Path::new)
        .map(|dir| dir.join(tool))
        .find(|candidate| {
            fs::metadata(candidate)
                .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
                .unwrap_or(false)
        })
}

/// Root may set up PostgreSQL only inside a disposable local CI
/// runner, where the job's identity changes only for the owned database.
fn disposable_root_runner(act: bool, disposable: bool, os: &str, container: bool) -> bool {
    act && disposable && os == "Linux" && container
}

async fn root_runner(path: &str, signals: &mut Signals) -> Result<Option<RootRunner>> {
    if tool_output("id", &["-u"], path, signals).await?.as_deref() != Some("0") {
        return Ok(None);
    }
    let act = std::env::var("ACT").is_ok_and(|value| value == "true");
    let disposable = std::env::var("LOCAL_CI_DISPOSABLE").is_ok_and(|value| value == "1");
    let os = tool_output("uname", &["-s"], path, signals)
        .await?
        .unwrap_or_default();
    let container = CONTAINER_MARKERS
        .iter()
        .any(|marker| Path::new(marker).is_file());
    if !disposable_root_runner(act, disposable, &os, container) {
        return Err(Stop::usage(
            "Root PostgreSQL setup is only supported inside a disposable local CI runner.",
        )
        .into());
    }
    let uid = tool_output("id", &["-u", "postgres"], path, signals).await?;
    let gid = tool_output("id", &["-g", "postgres"], path, signals).await?;
    match (find_on_path(path, "runuser"), uid, gid) {
        (Some(_), Some(uid), Some(gid)) if positive_integer(&uid) && positive_integer(&gid) => {
            Ok(Some(RootRunner { uid, gid }))
        }
        _ => Err(Stop::usage(
            "The disposable runner must provide runuser and a non-root postgres account/group.",
        )
        .into()),
    }
}

fn positive_integer(text: &str) -> bool {
    !text.is_empty() && !text.starts_with('0') && text.bytes().all(|byte| byte.is_ascii_digit())
}

/// A tool's trimmed standard output, when it exits successfully.
async fn tool_output(
    tool: &str,
    args: &[&str],
    path: &str,
    signals: &mut Signals,
) -> Result<Option<String>> {
    let mut command = Command::new(tool);
    command
        .args(args)
        .env_clear()
        .env("PATH", path)
        .stderr(Stdio::null());
    let output = helper::run(command, Duration::from_secs(5), Some(4096), Some(signals)).await?;
    Ok((output.status == 0).then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned()))
}

#[derive(Debug)]
enum State {
    Running,
    Stopped,
}

fn worsen(status: &mut i32) {
    if *status == 0 {
        *status = 1;
    }
}

/// `RUNNER_TEMP`, else `TMPDIR`, else `/tmp`: an existing absolute directory.
pub(crate) fn temp_root() -> Result<PathBuf> {
    let chosen = ["RUNNER_TEMP", "TMPDIR"]
        .iter()
        .find_map(|name| std::env::var(name).ok().filter(|value| !value.is_empty()))
        .unwrap_or_else(|| "/tmp".to_owned());
    let root = PathBuf::from(chosen.trim_end_matches('/'));
    if !root.is_absolute() || !root.is_dir() {
        return Err(Stop::usage(
            "RUNNER_TEMP/TMPDIR must identify an existing absolute temporary directory.",
        )
        .into());
    }
    Ok(root)
}

/// A fresh private directory `<prefix><8 random alphanumerics>` under `root`.
pub(crate) fn create_private_directory(root: &Path, prefix: &str) -> Result<PathBuf> {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    for _ in 0..64 {
        let random = process::random_hex()?;
        let suffix: String = random
            .as_bytes()
            .chunks(2)
            .take(8)
            .map(|pair| {
                let value = usize::from(pair[0]) * 16 + usize::from(pair[1]);
                char::from(ALPHABET[value % ALPHABET.len()])
            })
            .collect();
        let candidate = root.join(format!("{prefix}{suffix}"));
        match fs::DirBuilder::new().mode(0o700).create(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(error).with_context(|| format!("creating {}", candidate.display()));
            }
        }
    }
    anyhow::bail!("could not find a free name under {}", root.display())
}

/// The last `lines` lines of a log on standard error, when it exists.
pub(crate) fn tail(path: &Path, lines: usize) {
    if let Ok(text) = fs::read_to_string(path) {
        let all: Vec<&str> = text.lines().collect();
        let start = all.len().saturating_sub(lines);
        for line in &all[start..] {
            eprintln!("{line}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_port_grammar_matches_the_shell_helpers() {
        for valid in ["1000", "15434", "65535"] {
            assert!(parse_port(valid).is_some(), "{valid}");
        }
        for invalid in ["", "999", "0999", "65536", "15434 ", "-1", "abcd"] {
            assert!(parse_port(invalid).is_none(), "{invalid:?}");
        }
    }

    #[test]
    fn root_setup_needs_the_disposable_linux_container_runner() {
        assert!(disposable_root_runner(true, true, "Linux", true));
        assert!(!disposable_root_runner(false, true, "Linux", true));
        assert!(!disposable_root_runner(true, false, "Linux", true));
        assert!(!disposable_root_runner(true, true, "Darwin", true));
        assert!(!disposable_root_runner(true, true, "Linux", false));
    }

    #[test]
    fn the_postgres_account_runs_tools_through_a_cleared_environment_twice() {
        let cluster = Cluster {
            path: "/fixture/bin".into(),
            directory: PathBuf::from("/tmp/cluster"),
            data: PathBuf::from("/tmp/cluster/data"),
            identity: Some(RootRunner {
                uid: "999".into(),
                gid: "999".into(),
            }),
            start_attempted: false,
            start_confirmed: false,
        };
        let command = cluster.postgres_command("pg_ctl", &["-D", "/tmp/cluster/data", "status"]);
        let std_command = command;
        assert_eq!(std_command.get_program(), "runuser");
        let args: Vec<String> = std_command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            [
                "-u",
                "postgres",
                "--",
                "env",
                "-i",
                "PATH=/fixture/bin",
                "LC_ALL=C",
                "pg_ctl",
                "-D",
                "/tmp/cluster/data",
                "status"
            ]
        );
        assert_eq!(
            std_command.get_current_dir(),
            Some(Path::new("/tmp/cluster"))
        );
        let env: Vec<(String, Option<String>)> = std_command
            .get_envs()
            .map(|(name, value)| {
                (
                    name.to_string_lossy().into_owned(),
                    value.map(|value| value.to_string_lossy().into_owned()),
                )
            })
            .collect();
        assert_eq!(
            env,
            [
                ("LC_ALL".to_owned(), Some("C".to_owned())),
                ("PATH".to_owned(), Some("/fixture/bin".to_owned())),
            ]
        );
    }

    #[test]
    fn private_directories_follow_mktemp_names_and_are_owner_only() {
        let root =
            std::env::temp_dir().join(format!("simplestchat-xtask-test-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let created = create_private_directory(&root, "simplestchat-postgres.").unwrap();
        let name = created.file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(name.len(), "simplestchat-postgres.".len() + 8);
        assert!(
            name["simplestchat-postgres.".len()..]
                .bytes()
                .all(|b| b.is_ascii_alphanumeric())
        );
        assert_eq!(
            fs::metadata(&created).unwrap().permissions().mode() & 0o777,
            0o700
        );
        fs::remove_dir_all(&root).unwrap();
    }
}

#[cfg(test)]
#[path = "postgres_tests.rs"]
mod lifecycle_tests;
