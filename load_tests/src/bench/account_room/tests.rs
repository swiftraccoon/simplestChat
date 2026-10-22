//! Fixed in-memory service observations and owned local files. These fixtures
//! execute no server, generator, psql, HTTP request or container command.

use super::{
    config::{ACTIVE_MS, Config},
    controller::{self, Services},
    http::Http,
    sampling::Observations,
};
use crate::bench::{
    clock::Clock,
    process::{Child, Command, Exit, Host, Retirement, Stream},
    sampling::Ports,
};
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU8, AtomicU64, Ordering},
    },
    time::Duration,
};

pub(super) struct Fixture(pub(super) PathBuf);
impl Fixture {
    pub(super) fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("account-room-fixture-{}", uuid::Uuid::new_v4()));
        fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        fs::create_dir(path.join("bin")).unwrap();
        for name in ["psql", "server", "generator"] {
            fs::write(path.join("bin").join(name), b"inert selected file").unwrap();
            fs::set_permissions(
                path.join("bin").join(name),
                fs::Permissions::from_mode(0o700),
            )
            .unwrap();
        }
        fs::write(path.join("Cargo.lock"), b"owned fixture lock").unwrap();
        Self(fs::canonicalize(path).unwrap())
    }
    pub(super) fn environment(&self) -> BTreeMap<OsString, OsString> {
        [
            ("GITHUB_ACTIONS", "true".into()),
            ("RUNNER_OS", "Linux".into()),
            ("ACCOUNT_ROOM_E2E", "1".into()),
            ("DISPOSABLE_TEST_DATABASE", "1".into()),
            (
                "DATABASE_URL",
                concat!(
                    "postgres://test_owner:PRIVATE_PASSWORD@127.0.0.1:5432/",
                    "fixture_test?sslmode=disable"
                )
                .into(),
            ),
            ("SERVER_REVISION", "b".repeat(40)),
            ("E2E_ARTIFACTS", self.0.join("output").display().to_string()),
            ("RUNNER_TEMP", self.0.display().to_string()),
            ("PATH", self.0.join("bin").display().to_string()),
            (
                "TEST_SERVER_BINARY",
                self.0.join("bin/server").display().to_string(),
            ),
            (
                "TEST_GENERATOR_BINARY",
                self.0.join("bin/generator").display().to_string(),
            ),
        ]
        .into_iter()
        .map(|(k, v)| (k.into(), v.into()))
        .collect()
    }
    pub(super) fn config(&self) -> Config {
        Config::parse(self.environment(), self.0.clone(), "linux").unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[derive(Default)]
struct FakeClock(AtomicU64);
#[async_trait::async_trait]
impl Clock for FakeClock {
    fn elapsed_ms(&self) -> f64 {
        self.0.load(Ordering::SeqCst) as f64
    }
    fn utc(&self) -> String {
        let base = chrono::DateTime::parse_from_rfc3339("2026-10-22T12:00:00Z").unwrap();
        (base + chrono::Duration::milliseconds(self.elapsed_ms() as i64))
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    }
    async fn sleep(&self, duration: Duration) {
        self.0
            .fetch_add(duration.as_millis() as u64, Ordering::SeqCst);
    }
}
fn account(index: usize) -> String {
    format!("00000000-0000-4000-8000-{index:012}")
}
fn rows() -> Value {
    json!(
        (0..30)
            .map(|i| json!({"messageId":format!("message-{i}"),
    "senderAccountId":account(i),"clientMessageId":format!("client-{i}"),
    "content":format!("content-{i}")}))
            .collect::<Vec<_>>()
    )
}
fn inventory() -> Value {
    json!(
        (0..30)
            .map(|i| json!({"messageId":format!("message-{i}"),
    "senderAccountId":account(i),"clientMessageId":format!("client-{i}"),
    "contentSha256":hex::encode(Sha256::digest(format!("content-{i}").as_bytes()))}))
            .collect::<Vec<_>>()
    )
}
fn summary(passed: bool) -> Value {
    json!({"run":{"startedAt":"2026-10-22T12:00:00Z","completed":true,"passed":passed,
        "failureReasons":if passed {vec![]} else {vec!["controlled generator failure"]},
        "configuration":{"durationSecs":1200,"rampUpSecs":60,"warmupSecs":30,
            "profile":{"name":"browser"},"maxVideoConsumers":9,"maxAudioConsumers":29}},
        "measurement":{"durationMs":1200000,"bytesReceived":1000000,"packetsReceived":10000},
        "totalClients":30,"successfulConnections":30,"failedConnections":0,"failedConsumers":0,
        "validatedConsumers":1140,"chat":{"passed":true,"acknowledgements":30},
        "authentication":(0..30).map(|i|json!({"accountId":account(i),"passed":true,
            "originalExpiresAt":1800000000,"latestExpiresAt":1800000800,
            "originalExpiryCrossed":true,"httpRefreshes":1,"socketRenewals":1,"failures":0}))
            .collect::<Vec<_>>(),"runtimeLag":{"maxMsBySecond":vec![0;1290]}})
}

type Invocation = (String, Vec<String>, BTreeMap<OsString, OsString>);
#[derive(Default)]
struct State {
    calls: Mutex<Vec<Invocation>>,
    cancelled: AtomicU8,
    unknown: bool,
    stop_failure: bool,
    generator_failure: bool,
    late_terminal: bool,
    wrong_version: bool,
    cancel_sample: u8,
    generator_code: i32,
    generator_poll_error: bool,
    server_code: i32,
    stop_code: u8,
    setup_delay: u64,
    sample_delay: u64,
    missing_inventory: bool,
    malformed_results: bool,
    noncanonical_account: bool,
    final_namespace_failure: bool,
    retention_code: i32,
    cancel_retention: u8,
    probe_code: i32,
    probe_timeout_once: bool,
    probe_timeout_at: u64,
    probe_io_error: bool,
    probe_cleanup_failure: bool,
    probe_unknown: bool,
    probe_reaped_code: i32,
    probe_reaped_signal: i32,
    probes: AtomicU64,
}
struct FakeHost {
    state: Arc<State>,
    clock: Arc<FakeClock>,
    output: PathBuf,
}
struct FakeChild {
    state: Arc<State>,
    clock: Arc<FakeClock>,
    output: PathBuf,
    role: String,
    stdout: Vec<u8>,
    stopped: bool,
    finished: bool,
    command_code: i32,
    probe: bool,
    local_timeout: bool,
}
fn retirement() -> Retirement {
    Retirement {
        term_attempted: true,
        term_sent: true,
        kill_attempted: true,
        kill_sent: false,
        leader_terminal_before_cleanup: Some(true),
        leader_grace_expired: Some(false),
        wait_code: Some(0),
        wait_signal: None,
        group_cleanup_passed: true,
        streams_closed: true,
    }
}
#[async_trait::async_trait]
impl Child for FakeChild {
    fn pid(&self) -> u32 {
        if self.role == "server" { 123 } else { 456 }
    }
    fn retirement(&self) -> Option<Retirement> {
        let long = matches!(self.role.as_str(), "server" | "generator");
        (self.stopped
            && !(long && (self.state.unknown || self.state.stop_failure))
            && !(self.probe && self.state.probe_unknown))
            .then(|| {
                let mut facts = retirement();
                facts.wait_code = Some(if self.role == "generator" {
                    self.state.generator_code
                } else if self.role == "server" {
                    self.state.server_code
                } else {
                    self.command_code
                });
                if self.probe && self.state.probe_reaped_code != 0 {
                    facts.wait_code = Some(self.state.probe_reaped_code);
                }
                if self.probe && self.state.probe_reaped_signal != 0 {
                    facts.wait_code = None;
                    facts.wait_signal = Some(self.state.probe_reaped_signal);
                }
                facts.streams_closed =
                    !(self.role == "generator" && self.state.generator_poll_error);
                facts
            })
    }
    fn take_stdout(&mut self) -> Option<Box<dyn Stream>> {
        None
    }
    async fn poll(&mut self) -> Result<Option<Exit>> {
        if self.local_timeout {
            return Err(crate::owned_process::child::CommandTimedOut.into());
        }
        if self.probe && self.state.probe_io_error {
            anyhow::bail!("private_probe_io_detail");
        }
        let terminal = (self.role == "server" && self.state.server_code != 0)
            || (self.role != "server"
                && (self.role != "generator" || self.clock.elapsed_ms() >= 1_290_000.0));
        if terminal && self.role == "generator" && !self.finished {
            // These are deliberately invalid until stop() proves stream closure.
            fs::write(self.output.join("load_test_summary.json"), b"unsettled").unwrap();
            self.finished = true;
            if self.state.late_terminal {
                self.clock.0.store(ACTIVE_MS as u64, Ordering::SeqCst);
            }
        }
        if self.role == "generator" && self.finished && self.state.generator_poll_error {
            anyhow::bail!("private_generator_pipe_detail");
        }
        Ok(terminal.then(|| Exit {
            code: Some(if self.role == "generator" {
                if self.state.generator_failure {
                    1
                } else {
                    self.state.generator_code
                }
            } else if self.role == "server" {
                self.state.server_code
            } else {
                self.command_code
            }),
            signal: None,
        }))
    }
    async fn stop(&mut self, _grace: Duration) -> Result<Exit> {
        self.stopped = true;
        if self.role == "server" && self.state.final_namespace_failure {
            fs::set_permissions(
                self.output.parent().unwrap(),
                fs::Permissions::from_mode(0o755),
            )?;
        }
        if self.role == "generator" && self.finished {
            fs::write(
                self.output.join("load_test_summary.json"),
                serde_json::to_vec(&summary(!self.state.generator_failure))?,
            )
            .unwrap();
            fs::write(
                self.output.join("load_test_results.json"),
                if self.state.malformed_results {
                    b"bad".as_slice()
                } else {
                    b"[]".as_slice()
                },
            )
            .unwrap();
            if !self.state.missing_inventory {
                fs::write(
                    self.output.join("chat_inventory.json"),
                    serde_json::to_vec(&inventory())?,
                )
                .unwrap();
            }
        }
        if self.role == "generator" && self.state.stop_code != 0 {
            return Err(crate::owned_process::Interrupted(self.state.stop_code).into());
        }
        ensure!(
            !(self.state.stop_failure && matches!(self.role.as_str(), "server" | "generator"))
                && !(self.probe && self.state.probe_cleanup_failure),
            "fake_stop_failed"
        );
        Ok(Exit {
            code: Some(if self.role == "generator" {
                if self.state.generator_failure {
                    1
                } else {
                    self.state.generator_code
                }
            } else if self.role == "server" {
                self.state.server_code
            } else {
                self.command_code
            }),
            signal: None,
        })
    }
    fn output(&self) -> &[u8] {
        &self.stdout
    }
    fn errors(&self) -> &[u8] {
        b"PRIVATE_PASSWORD child diagnostic"
    }
}
#[async_trait::async_trait]
impl Host for FakeHost {
    fn spawn(&self, command: Command) -> Result<Box<dyn Child>> {
        let role = command
            .executable
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        self.state.calls.lock().unwrap().push((
            role.clone(),
            command.arguments.clone(),
            command.environment,
        ));
        let retention = role == "psql"
            && command
                .arguments
                .last()
                .is_some_and(|sql| sql.contains("FROM chat_messages"));
        if retention && self.state.cancel_retention != 0 {
            self.state
                .cancelled
                .store(self.state.cancel_retention, Ordering::SeqCst);
        }
        let probe = role == "psql"
            && command
                .arguments
                .last()
                .is_some_and(|sql| sql == super::database::PROBE);
        let local_timeout = probe
            && self.state.probes.fetch_add(1, Ordering::SeqCst) == self.state.probe_timeout_at
            && self.state.probe_timeout_once;
        let command_code = if retention {
            self.state.retention_code
        } else if role == "psql"
            && command
                .arguments
                .last()
                .is_some_and(|sql| sql == super::database::PROBE)
        {
            self.state.probe_code
        } else {
            0
        };
        let stdout = match role.as_str() {
            "psql" => {
                let sql = command.arguments.last().unwrap();
                if sql == super::database::VERSION {
                    if self.state.wrong_version {
                        b"170000".to_vec()
                    } else {
                        b"180006".to_vec()
                    }
                } else if sql == super::database::PROBE {
                    b"{\"connections\":5,\"active\":1,\"waiting\":0}\nTime: 0.5 ms\n".to_vec()
                } else if sql.contains("history_retention_days") {
                    b"7".to_vec()
                } else {
                    serde_json::to_vec(&rows()).unwrap()
                }
            }
            "uname" => b"inert-kernel\n".to_vec(),
            _ => Vec::new(),
        };
        Ok(Box::new(FakeChild {
            state: self.state.clone(),
            clock: self.clock.clone(),
            output: self.output.clone(),
            role,
            stdout,
            stopped: false,
            finished: false,
            command_code,
            probe,
            local_timeout,
        }))
    }
    async fn cancelled(&self) -> u8 {
        let code = self.state.cancelled.load(Ordering::SeqCst);
        if code != 0 {
            code
        } else {
            std::future::pending().await
        }
    }
}
struct FakeHttp {
    state: Arc<State>,
    registrations: Mutex<usize>,
    clock: Arc<FakeClock>,
}
#[async_trait::async_trait]
impl Http for FakeHttp {
    async fn ready(&self) -> Result<bool> {
        Ok(true)
    }
    async fn post(
        &self,
        index: usize,
        path: &str,
        body: &Value,
        token: Option<&str>,
    ) -> Result<Value> {
        if path == "/api/auth/register" {
            let mut registrations = self.registrations.lock().unwrap();
            assert_eq!(*registrations, index);
            *registrations += 1;
            assert_eq!(body["display_name"], format!("Account {}", index + 1));
            assert!(body["password"].as_str().unwrap().ends_with("Aa1!"));
            assert!(token.is_none());
            let id = if self.state.noncanonical_account {
                account(index).replace('-', "")
            } else {
                account(index)
            };
            Ok(json!({"user":{"id":id},"token":"PRIVATE_TOKEN"}))
        } else {
            assert_eq!(token, Some("PRIVATE_TOKEN"));
            assert_eq!(body["max_participants"], 30);
            assert_eq!(body["guests_allowed"], false);
            if self.state.setup_delay != 0 {
                self.clock.0.store(self.state.setup_delay, Ordering::SeqCst);
            }
            Ok(json!({}))
        }
    }
    async fn metrics(&self) -> Result<String> {
        let _ = &self.state;
        Ok(concat!(
            "simplestchat_connections_active 30\n",
            "simplestchat_participants_active 30\nsimplestchat_rooms_active 1\n",
            "simplestchat_participants_snapshot_complete 1\n",
            "simplestchat_cpu_saturated 0\nsimplestchat_memory_saturated 0\n",
            "simplestchat_media_worker_cpu{worker=\"0\"} 0.4\n",
            "simplestchat_database_errors_total{kind=\"pool_timeout\"} 0\n",
            "simplestchat_db_pool_connections{state=\"used\"} 1\n",
            "simplestchat_db_pool_connections{state=\"max\"} 10\n"
        )
        .into())
    }
}
struct FakeObservations(Arc<FakeClock>, Arc<State>);
#[async_trait::async_trait]
impl Observations for FakeObservations {
    async fn initialize(&self, server: u32) -> Result<()> {
        assert_eq!(server, 123);
        Ok(())
    }
    async fn sample(&self, _server: u32, _generator: u32) -> Value {
        let index = self.0.elapsed_ms() / 500.0;
        if index >= 5.0 && self.1.cancel_sample != 0 {
            self.1
                .cancelled
                .store(self.1.cancel_sample, Ordering::SeqCst);
        }
        self.0.0.fetch_add(self.1.sample_delay, Ordering::SeqCst);
        json!({"server":{"cpuSeconds":index*0.1,"rssKiB":100000,"childCount":0,
                "processes":[{"pid":123,"startTicks":1000}]},
            "generator":{"cpuSeconds":index*0.3,"rssKiB":150000},
            "host":{"total":index*200.0,"idle":index*120.0,"steal":0,"availableBytes":1073741824},
            "cgroup":{"periods":index,"throttledPeriods":0,"quotaCores":null}})
    }
}
struct FakePorts;
impl Ports for FakePorts {
    fn available(&self, p: u16, u: u16, w: u32) -> Result<()> {
        assert_eq!((p, u, w), (3119, 41010, 1));
        Ok(())
    }
}

async fn execute(fixture: &Fixture, state: Arc<State>) -> controller::Outcome {
    let config = fixture.config();
    let private = config.temporary.clone();
    let clock = Arc::new(FakeClock::default());
    let host = FakeHost {
        state: state.clone(),
        clock: clock.clone(),
        output: config.output.join("generator"),
    };
    let final_namespace_failure = state.final_namespace_failure;
    let http = FakeHttp {
        state: state.clone(),
        registrations: Mutex::new(0),
        clock: clock.clone(),
    };
    let observations = FakeObservations(clock.clone(), state);
    let services = Services {
        host: &host,
        clock: clock.as_ref(),
        http: &http,
        observations: &observations,
        ports: &FakePorts,
    };
    let outcome = controller::execute(&config, &services).await.unwrap();
    assert!(!private.exists());
    if !final_namespace_failure {
        let report = fs::read_to_string(config.output.join("account-room-report.json")).unwrap();
        assert!(!report.contains("PRIVATE_PASSWORD") && !report.contains("PRIVATE_TOKEN"));
        assert!(!report.contains(private.to_str().unwrap()));
    }
    outcome
}

#[tokio::test]
async fn full_owner_stops_before_reading_all_selected_results_and_removes_credentials() {
    let fixture = Fixture::new();
    let state = Arc::new(State::default());
    let outcome = execute(&fixture, state.clone()).await;
    assert_eq!(outcome.code, 0, "{}", outcome.report);
    assert_eq!(outcome.report["passed"], true);
    for key in [
        "platform",
        "release",
        "arch",
        "cpuModel",
        "logicalCpus",
        "totalMemoryBytes",
    ] {
        assert!(outcome.report["host"].get(key).is_some(), "{key}");
    }
    for key in ["cpu", "memoryBytes", "implementation", "loadAverageBefore"] {
        assert!(outcome.report["host"].get(key).is_none(), "{key}");
    }
    assert_eq!(outcome.report["privateCredentialsRemoved"], true);
    for verdict in ["generator", "authentication", "resources", "retention"] {
        assert_eq!(outcome.report[verdict]["passed"], true, "{verdict}");
    }
    let calls = state.calls.lock().unwrap();
    let server = &calls.iter().find(|call| call.0 == "server").unwrap().2;
    assert_eq!(
        server.get(&OsString::from("REGISTRATION_ENABLED")).unwrap(),
        "true"
    );
    assert_eq!(
        server.get(&OsString::from("RUN_MIGRATIONS")).unwrap(),
        "true"
    );
    assert!(!server.contains_key(&OsString::from("MAX_CONNECTIONS_PER_IP")));
    let generator = &calls.iter().find(|call| call.0 == "generator").unwrap().1;
    for (key, value) in [
        ("--clients", "30"),
        ("--duration", "1200"),
        ("--source-addresses", "30"),
    ] {
        assert_eq!(
            generator[generator.iter().position(|v| v == key).unwrap() + 1],
            value
        );
    }
    let shutdown: Value =
        serde_json::from_slice(&fs::read(fixture.0.join("output/server-shutdown.json")).unwrap())
            .unwrap();
    assert_eq!(shutdown["schemaVersion"], 2);
    assert_eq!(shutdown["groupSignals"]["killAttempted"], true);
    assert_eq!(shutdown["leaderGraceExpired"], false);
    assert_eq!(shutdown["passed"], true);
}

#[tokio::test]
async fn full_completed_exit_one_keeps_all_independent_verdicts() {
    let fixture = Fixture::new();
    let state = Arc::new(State {
        generator_failure: true,
        ..State::default()
    });
    let outcome = execute(&fixture, state).await;
    assert_eq!(outcome.code, 1);
    assert_eq!(outcome.report["completed"], true);
    assert_eq!(outcome.report["generator"]["passed"], false);
    for verdict in ["authentication", "resources", "retention"] {
        assert_eq!(outcome.report[verdict]["passed"], true);
    }
}

#[tokio::test]
async fn unknown_or_failed_retirement_never_admits_results_or_success() {
    for state in [
        State {
            unknown: true,
            ..State::default()
        },
        State {
            stop_failure: true,
            ..State::default()
        },
    ] {
        let fixture = Fixture::new();
        let outcome = execute(&fixture, Arc::new(state)).await;
        assert_eq!(outcome.code, 1);
        assert_eq!(outcome.report["passed"], false);
        assert!(outcome.report.get("authentication").is_none());
    }
}

#[tokio::test]
async fn terminal_acceptance_cannot_bypass_active_deadline() {
    let fixture = Fixture::new();
    let state = Arc::new(State {
        late_terminal: true,
        ..State::default()
    });
    let outcome = execute(&fixture, state).await;
    assert_eq!(outcome.code, 124);
    assert_eq!(outcome.report["passed"], false);
}

#[tokio::test]
async fn wrong_database_version_dispatches_no_server_or_generator() {
    let fixture = Fixture::new();
    let state = Arc::new(State {
        wrong_version: true,
        ..State::default()
    });
    let outcome = execute(&fixture, state.clone()).await;
    assert_eq!(outcome.code, 1);
    let calls = state.calls.lock().unwrap();
    assert!(
        !calls
            .iter()
            .any(|v| matches!(v.0.as_str(), "server" | "generator"))
    );
}

#[test]
fn profile_refuses_unowned_targets_and_keeps_libpq_secrets_out_of_uri_values() {
    let fixture = Fixture::new();
    let environment = fixture.environment();
    assert!(Config::parse(environment.clone(), fixture.0.clone(), "macos").is_err());
    for (key, value) in [
        ("GITHUB_ACTIONS", "false"),
        ("ACCOUNT_ROOM_E2E", "0"),
        (
            "DATABASE_URL",
            "postgres://secret@public.example/fixture_test",
        ),
        ("DATABASE_URL", "postgres://secret@127.0.0.1/production"),
        (
            "DATABASE_URL",
            "postgres://secret@127.0.0.1/fixture_test?host=public.example",
        ),
        (
            "TEST_DATABASE_URL",
            "postgres://secret@127.0.0.1/other_test",
        ),
        ("TEST_SERVER_PORT", "0"),
        ("SERVER_REVISION", "HEAD"),
    ] {
        let mut environment = environment.clone();
        environment.insert(key.into(), value.into());
        assert!(
            Config::parse(environment, fixture.0.clone(), "linux").is_err(),
            "{key}"
        );
    }
    let mut environment = environment;
    environment.insert(
        "DATABASE_URL".into(),
        "postgres://test%5Fowner:public%2Dfixture%21@127.0.0.1:5439/fixture_test?sslmode=disable"
            .into(),
    );
    let config = Config::parse(environment, fixture.0.clone(), "linux").unwrap();
    let environment = super::database::environment(&config).unwrap();
    assert_eq!(environment[&OsString::from("PGUSER")], "test_owner");
    assert_eq!(
        environment[&OsString::from("PGPASSWORD")],
        "public-fixture!"
    );
    assert_eq!(environment[&OsString::from("PGPORT")], "5439");
    assert!(
        !environment
            .values()
            .any(|v| v.to_string_lossy().contains("postgres://"))
    );
}

#[tokio::test]
async fn cancellation_observed_during_a_sample_keeps_its_typed_primary_status() {
    for code in [130, 143] {
        let fixture = Fixture::new();
        let state = Arc::new(State {
            cancel_sample: code,
            ..State::default()
        });
        let outcome = execute(&fixture, state).await;
        assert_eq!(outcome.code, code);
        assert_eq!(outcome.report["passed"], false);
        assert_eq!(outcome.report["privateCredentialsRemoved"], true);
    }
}

#[tokio::test]
async fn observed_nonzero_leader_status_survives_deadline_and_retirement_errors() {
    for state in [
        State {
            generator_code: 42,
            late_terminal: true,
            ..State::default()
        },
        State {
            generator_code: 42,
            stop_code: 143,
            ..State::default()
        },
        State {
            server_code: 42,
            ..State::default()
        },
    ] {
        let fixture = Fixture::new();
        let outcome = execute(&fixture, Arc::new(state)).await;
        assert_eq!(outcome.code, 42);
        assert_eq!(outcome.report["passed"], false);
    }
    let fixture = Fixture::new();
    let outcome = execute(
        &fixture,
        Arc::new(State {
            stop_code: 143,
            ..State::default()
        }),
    )
    .await;
    assert_eq!(outcome.code, 143);
    assert_eq!(outcome.report["passed"], false);
}

#[tokio::test]
async fn a_known_insufficient_full_workload_budget_refuses_before_generator_spawn() {
    let fixture = Fixture::new();
    let state = Arc::new(State {
        setup_delay: 510_001,
        ..State::default()
    });
    let outcome = execute(&fixture, state.clone()).await;
    assert_eq!(outcome.code, 124);
    assert!(state.calls.lock().unwrap().iter().any(|v| v.0 == "server"));
    assert!(
        !state
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|v| v.0 == "generator")
    );
    assert_eq!(outcome.report["privateCredentialsRemoved"], true);
}

#[tokio::test]
async fn sample_timestamp_is_taken_before_the_observed_read_work() {
    let fixture = Fixture::new();
    let outcome = execute(
        &fixture,
        Arc::new(State {
            sample_delay: 17,
            ..State::default()
        }),
    )
    .await;
    assert_eq!(outcome.code, 0);
    let source = fs::read_to_string(fixture.0.join("output/resource-samples.ndjson")).unwrap();
    let first: Value = serde_json::from_str(source.lines().next().unwrap()).unwrap();
    assert_eq!(first["elapsedMs"], 0.0);
}

#[test]
fn report_assessments_keep_fixed_controller_failure_codes_and_hide_arbitrary_errors() {
    for code in [
        "full_measurement_window_not_completed",
        "retention_chat_ack_mismatch",
        "room_history_retention_not_enabled",
    ] {
        assert_eq!(
            super::report::assess(Err(anyhow::anyhow!(code)))["failure"],
            code
        );
    }
    assert!(
        !super::report::assess(Err(anyhow::anyhow!("PRIVATE_PASSWORD arbitrary detail")))
            .to_string()
            .contains("PRIVATE_PASSWORD")
    );
}

#[tokio::test]
async fn unavailable_secondary_artifacts_do_not_erase_completed_independent_assessments() {
    for state in [
        State {
            generator_failure: true,
            missing_inventory: true,
            ..State::default()
        },
        State {
            malformed_results: true,
            ..State::default()
        },
    ] {
        let fixture = Fixture::new();
        let outcome = execute(&fixture, Arc::new(state)).await;
        assert_eq!(outcome.code, 1);
        assert_eq!(outcome.report["completed"], true);
        assert_eq!(outcome.report["authentication"]["passed"], true);
        assert_eq!(outcome.report["resources"]["passed"], true);
        assert_eq!(outcome.report["passed"], false);
    }
}

#[tokio::test]
async fn noncanonical_registration_identifiers_refuse_before_generator_admission() {
    let fixture = Fixture::new();
    let state = Arc::new(State {
        noncanonical_account: true,
        ..State::default()
    });
    let outcome = execute(&fixture, state.clone()).await;
    assert_eq!(outcome.code, 1);
    assert_eq!(outcome.report["failure"], "invalid_registered_account");
    assert!(
        !state
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|v| v.0 == "generator")
    );
}

#[test]
fn captured_relative_path_selection_keeps_the_original_first_executable() {
    let fixture = Fixture::new();
    fs::create_dir(fixture.0.join("relative bin")).unwrap();
    let first = fixture.0.join("relative bin/psql");
    fs::write(&first, b"first inert tool").unwrap();
    fs::set_permissions(&first, fs::Permissions::from_mode(0o700)).unwrap();
    let mut environment = fixture.environment();
    environment.insert(
        "PATH".into(),
        format!("relative bin:{}", fixture.0.join("bin").display()).into(),
    );
    assert_eq!(
        super::database::find(&environment, &fixture.0, "psql").unwrap(),
        first
    );
}

#[tokio::test]
async fn final_artifact_namespace_failure_does_not_erase_observed_nonzero_status() {
    let fixture = Fixture::new();
    let outcome = execute(
        &fixture,
        Arc::new(State {
            generator_code: 42,
            final_namespace_failure: true,
            ..State::default()
        }),
    )
    .await;
    assert_eq!(outcome.code, 42);
    assert_eq!(outcome.report["passed"], false);
    assert!(!fixture.0.join("output/account-room-report.json").exists());
}

#[tokio::test]
async fn required_retention_commands_preserve_observed_status_and_private_fixed_failure() {
    for (state, expected) in [
        (
            State {
                retention_code: 42,
                ..State::default()
            },
            42,
        ),
        (
            State {
                cancel_retention: 130,
                ..State::default()
            },
            130,
        ),
        (
            State {
                cancel_retention: 143,
                ..State::default()
            },
            143,
        ),
    ] {
        let fixture = Fixture::new();
        let outcome = execute(&fixture, Arc::new(state)).await;
        assert_eq!(outcome.code, expected);
        assert_eq!(outcome.report["completed"], true);
        assert_eq!(outcome.report["authentication"]["passed"], true);
        assert_eq!(outcome.report["resources"]["passed"], true);
        assert_eq!(outcome.report["retention"]["passed"], false);
        assert_eq!(outcome.report["passed"], false);
        if expected == 42 {
            assert_eq!(
                outcome.report["retention"]["failure"],
                "database_command_exit_42"
            );
        }
    }
}

#[tokio::test]
async fn a_settled_probe_control_outcome_cannot_become_a_missing_sample_or_late_exit() {
    for code in [124, 130, 143] {
        let fixture = Fixture::new();
        let state = Arc::new(State {
            probe_code: code,
            ..State::default()
        });
        let outcome = execute(&fixture, state).await;
        assert_eq!(outcome.code, code as u8);
        assert_eq!(outcome.report["passed"], false);
        assert_eq!(outcome.report["privateCredentialsRemoved"], true);
    }
}

#[tokio::test]
async fn one_proved_local_probe_timeout_remains_a_missing_observation_within_coverage() {
    let fixture = Fixture::new();
    let state = Arc::new(State {
        probe_timeout_once: true,
        probe_timeout_at: 20,
        ..State::default()
    });
    let outcome = execute(&fixture, state).await;
    assert_eq!(outcome.code, 0);
    assert_eq!(outcome.report["resources"]["passed"], true);
    let source = fs::read_to_string(fixture.0.join("output/resource-samples.ndjson")).unwrap();
    let missing = source
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .find(|row| row["elapsedMs"] == 100_000.0)
        .unwrap();
    assert!(missing["metricsExpected"] == true && missing["database"].is_null());
}

#[tokio::test]
async fn an_optional_probe_never_hides_failed_or_unknown_retirement_after_io_or_timeout() {
    for state in [
        State {
            probe_timeout_once: true,
            probe_cleanup_failure: true,
            ..State::default()
        },
        State {
            probe_timeout_once: true,
            probe_unknown: true,
            ..State::default()
        },
        State {
            probe_io_error: true,
            probe_cleanup_failure: true,
            ..State::default()
        },
        State {
            probe_io_error: true,
            probe_unknown: true,
            ..State::default()
        },
    ] {
        let fixture = Fixture::new();
        let state = Arc::new(state);
        let outcome = execute(&fixture, state.clone()).await;
        assert_ne!(outcome.code, 0);
        assert_eq!(outcome.report["passed"], false);
        assert_eq!(outcome.report["failure"], "database_cleanup_unconfirmed");
        assert!(state.probes.load(Ordering::SeqCst) <= 1);
        assert_eq!(outcome.report["privateCredentialsRemoved"], true);
    }
}

#[tokio::test]
async fn a_reaped_nonzero_status_is_kept_when_an_earlier_generic_pipe_error_hid_it() {
    let fixture = Fixture::new();
    let state = Arc::new(State {
        generator_poll_error: true,
        generator_code: 42,
        ..State::default()
    });
    let outcome = execute(&fixture, state).await;
    assert_eq!(outcome.code, 42);
    assert_eq!(outcome.report["passed"], false);
    assert_eq!(outcome.report["generatorCleanup"]["stopped"], false);
}

#[tokio::test]
async fn both_getconf_owners_settle_when_one_observation_fails() {
    use std::sync::atomic::AtomicUsize;
    struct Units {
        stopped: Arc<AtomicUsize>,
        abandoned: Arc<AtomicUsize>,
    }
    struct Unit {
        failed: bool,
        polls: u8,
        closed: bool,
        stopped: Arc<AtomicUsize>,
        abandoned: Arc<AtomicUsize>,
    }
    impl Drop for Unit {
        fn drop(&mut self) {
            if !self.closed {
                self.abandoned.fetch_add(1, Ordering::SeqCst);
            }
        }
    }
    #[async_trait::async_trait]
    impl Child for Unit {
        fn pid(&self) -> u32 {
            if self.failed { 123 } else { 456 }
        }
        fn take_stdout(&mut self) -> Option<Box<dyn Stream>> {
            None
        }
        async fn poll(&mut self) -> Result<Option<Exit>> {
            self.polls += 1;
            if self.failed && self.polls >= 2 {
                anyhow::bail!("inert_unit_failure");
            }
            Ok((self.polls >= 3).then_some(Exit {
                code: Some(0),
                signal: None,
            }))
        }
        async fn stop(&mut self, _grace: Duration) -> Result<Exit> {
            self.closed = true;
            self.stopped.fetch_add(1, Ordering::SeqCst);
            Ok(Exit {
                code: Some(0),
                signal: None,
            })
        }
        fn output(&self) -> &[u8] {
            b"100\n"
        }
        fn errors(&self) -> &[u8] {
            b""
        }
    }
    #[async_trait::async_trait]
    impl Host for Units {
        fn spawn(&self, command: Command) -> Result<Box<dyn Child>> {
            assert_eq!(command.executable.file_name().unwrap(), "getconf");
            Ok(Box::new(Unit {
                failed: command.arguments == ["CLK_TCK"],
                polls: 0,
                closed: false,
                stopped: self.stopped.clone(),
                abandoned: self.abandoned.clone(),
            }))
        }
        async fn cancelled(&self) -> u8 {
            std::future::pending().await
        }
    }
    let fixture = Fixture::new();
    let tool = fixture.0.join("bin/getconf");
    fs::write(&tool, b"inert selected file").unwrap();
    fs::set_permissions(&tool, fs::Permissions::from_mode(0o700)).unwrap();
    let config = fixture.config();
    let clock = FakeClock::default();
    let units = Units {
        stopped: Arc::new(AtomicUsize::new(0)),
        abandoned: Arc::new(AtomicUsize::new(0)),
    };
    let observations = super::sampling::NativeObservations::new(&config, &units, &clock);
    assert!(observations.initialize(123).await.is_err());
    assert_eq!(units.stopped.load(Ordering::SeqCst), 2);
    assert_eq!(units.abandoned.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn an_unknown_probe_error_retains_a_later_actual_nonzero_reap_before_refusing_cleanup() {
    for (code, signal, expected) in [(42, 0, 42), (0, 2, 130), (0, 15, 143)] {
        let fixture = Fixture::new();
        let state = Arc::new(State {
            probe_io_error: true,
            probe_cleanup_failure: true,
            probe_reaped_code: code,
            probe_reaped_signal: signal,
            ..State::default()
        });
        let outcome = execute(&fixture, state).await;
        assert_eq!(outcome.code, expected);
        assert_eq!(outcome.report["passed"], false);
        assert_eq!(outcome.report["failure"], "database_cleanup_unconfirmed");
    }
}

#[tokio::test]
async fn a_known_probe_status_is_not_replaced_by_a_later_different_reap() {
    for code in [124, 130, 143] {
        let fixture = Fixture::new();
        let state = Arc::new(State {
            probe_code: code,
            probe_cleanup_failure: true,
            probe_reaped_code: 42,
            ..State::default()
        });
        let outcome = execute(&fixture, state).await;
        assert_eq!(outcome.code, code as u8);
        assert_eq!(outcome.report["passed"], false);
        assert_eq!(outcome.report["failure"], "database_cleanup_unconfirmed");
    }
}
