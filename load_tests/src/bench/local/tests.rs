//! Full controller fixtures own virtual processes and private temporary files.
//! The real process owner and reader subprocess have separate native fixtures;
//! no server, socket, container, media generator or benchmark is started here.

#[test]
fn help_and_invalid_commands_are_handled_before_runtime_or_resource_creation() {
    use std::process::ExitCode;
    assert_eq!(crate::bench::run(&["--help".into()]), ExitCode::SUCCESS);
    assert_eq!(super::run(&["--help".into()]), ExitCode::SUCCESS);
    assert_eq!(crate::bench::run(&["unknown".into()]), ExitCode::from(2));
    assert_eq!(super::run(&["--not-an-option".into()]), ExitCode::FAILURE);
}

use super::{Context, Manifest, Services, run_one};
use crate::bench::{
    artifacts,
    clock::Clock,
    media_sampler::Media,
    metrics::{Metrics, Snapshot},
    options::{self, Options},
    process::{Child, Command, Exit, Host, Sink, Stream},
    provenance::{self, ServerIdentity, SourceTreeIdentity},
    reports::{Reports, Request},
    resources::ProcessSample,
    sampling::{Ports, Resources},
};
use anyhow::Result;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};
use std::time::Duration;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Fault {
    None,
    Port,
    Health,
    GeneratorSpawn,
    GeneratorExit,
    ServerExit,
    ForeignSummary,
    UnsafeResults,
    Timeout,
    Report,
    ReportWrite,
    TimelineWrite,
    ResourcesWrite,
    GeneratorStop,
    GeneratorChanged,
    ServerStop,
    CaptureStop,
    CaptureExit,
    Cleanup,
    Interrupt,
}

#[derive(Default)]
struct Journal {
    active: BTreeSet<&'static str>,
    spawned: Vec<&'static str>,
    stopped: Vec<&'static str>,
    reports: Vec<String>,
    environments: Vec<BTreeMap<OsString, OsString>>,
    arguments: Vec<Vec<String>>,
    probes: usize,
    resources: usize,
}

#[derive(Clone)]
struct Fixture {
    root: PathBuf,
    clock: Arc<AtomicU64>,
    journal: Arc<Mutex<Journal>>,
    fault: Fault,
    summary: Value,
}

impl Fixture {
    fn new(fault: Fault) -> (Self, Options, Manifest) {
        let root = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("benchmark-local-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let server = root.join("server fixture");
        let generator = root.join("generator fixture");
        std::fs::write(&server, b"owned server fixture").unwrap();
        std::fs::write(&generator, b"owned generator fixture").unwrap();
        let server_hash = provenance::hash_file(&server).unwrap().0;
        let generator_hash = provenance::hash_file(&generator).unwrap().0;
        let arguments = [
            "--baseline-root",
            root.to_str().unwrap(),
            "--baseline-bin",
            server.to_str().unwrap(),
            "--candidate-root",
            root.to_str().unwrap(),
            "--candidate-bin",
            server.to_str().unwrap(),
            "--generator",
            generator.to_str().unwrap(),
            "--output",
            root.join("output").to_str().unwrap(),
            "--duration",
            "3",
            "--warmup",
            "2",
            "--ramp-up",
            "1",
            "--repetitions",
            "1",
            "--purpose",
            "diagnostic",
        ]
        .map(OsString::from);
        let options = options::parse(&arguments, &root).unwrap();
        artifacts::directory(&options.output).unwrap();
        let identity = ServerIdentity {
            root: root.clone(),
            binary: server,
            revision: "fixture".into(),
            tracked_diff_sha256: "a".repeat(64),
            source_tree: SourceTreeIdentity {
                source_tree_sha256: "b".repeat(64),
                source_tree_files: 1,
                source_tree_missing_paths: Vec::new(),
                source_tree_scope: Vec::new(),
            },
            cargo_lock_sha256: "c".repeat(64),
            binary_sha256: server_hash,
            binary_bytes: 20,
        };
        let latency = serde_json::to_value(crate::LatencyStats::default()).unwrap();
        let summary = json!({"schemaVersion":2,"run":{
            "completed":true,"passed":true,"failureReasons":[],
            "startedAt":"2026-10-21T00:00:00.000Z","finishedAt":"2026-10-21T00:00:06.000Z",
            "configuration":{"diagnostics":true,"departure":"abrupt","rampUpSecs":1,
                "warmupSecs":2,"durationSecs":3,"subscriptionPlan":null,"subscriptionSeed":null},
            "provenance":{"generatorBinarySha256":generator_hash}},
            "totalErrors":0,"failedConnections":0,"failedConsumers":0,
            "validatedConsumers":4,"skippedShortLivedConsumers":0,"p99ConnectionTimeMs":10,
            "sendMediaReady":latency,"receiveMediaReady":latency,"measurement":{
                "durationMs":3000,"packetsQueued":30,"packetsReceived":90,
                "bytesQueued":300,"bytesReceived":900},"diagnosticFailures":0});
        let manifest = Manifest {
            baseline: identity.clone(),
            candidate: identity,
            generator_sha256: generator_hash,
            generator_source_identity: "d".repeat(64),
            value: Value::Null,
        };
        (
            Self {
                root,
                clock: Arc::default(),
                journal: Arc::default(),
                fault,
                summary,
            },
            options,
            manifest,
        )
    }

    fn services<'a>(&'a self, environment: &'a BTreeMap<OsString, OsString>) -> Services<'a> {
        Services {
            host: self,
            clock: self,
            metrics: self,
            resources: self,
            ports: self,
            media: self,
            reports: self,
            environment,
        }
    }

    fn result_path(&self) -> PathBuf {
        self.root.join("output/conference-10-1-baseline")
    }
    fn clean(&self) {
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}

#[async_trait::async_trait]
impl Clock for Fixture {
    fn elapsed_ms(&self) -> f64 {
        self.clock.load(Ordering::SeqCst) as f64
    }
    fn utc(&self) -> String {
        chrono::DateTime::from_timestamp_millis(1_792_540_800_000 + self.elapsed_ms() as i64)
            .unwrap()
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    }
    async fn sleep(&self, duration: Duration) {
        self.clock
            .fetch_add(duration.as_millis() as u64, Ordering::SeqCst);
        tokio::task::yield_now().await;
    }
}

struct Process {
    fixture: Fixture,
    role: &'static str,
    expires: Option<u64>,
    retired: bool,
    artifacts: Option<PathBuf>,
}

#[async_trait::async_trait]
impl Child for Process {
    fn retirement(&self) -> Option<crate::bench::process::Retirement> {
        self.retired.then_some(crate::bench::process::Retirement {
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
        })
    }
    fn pid(&self) -> u32 {
        if self.role == "server" { 1001 } else { 1002 }
    }
    fn take_stdout(&mut self) -> Option<Box<dyn Stream>> {
        None
    }
    async fn poll(&mut self) -> Result<Option<Exit>> {
        let done = self.retired
            || self
                .expires
                .is_some_and(|end| self.fixture.clock.load(Ordering::SeqCst) >= end);
        Ok(done.then(|| Exit {
            code: Some(
                if self.role == "generator" && self.fixture.fault == Fault::GeneratorExit {
                    7
                } else if self.role == "capture" && self.fixture.fault == Fault::CaptureExit {
                    1
                } else {
                    0
                },
            ),
            signal: None,
        }))
    }
    async fn stop(&mut self, grace: Duration) -> Result<Exit> {
        if !self.retired {
            self.retired = true;
            let mut journal = self.fixture.journal.lock().unwrap();
            journal.active.remove(self.role);
            journal.stopped.push(self.role);
            if self.role == "server" {
                assert_eq!(grace, Duration::from_secs(20));
            }
        }
        let fault = match self.role {
            "server" => Fault::ServerStop,
            "generator" => Fault::GeneratorStop,
            _ => Fault::CaptureStop,
        };
        if self.fixture.fault == fault {
            anyhow::bail!("secret-sentinel native error");
        }
        if let Some(directory) = &self.artifacts {
            let pending = directory.join("pending-summary.fixture");
            if pending.exists() {
                std::fs::rename(pending, directory.join("load_test_summary.json"))?;
                std::fs::rename(
                    directory.join("pending-results.fixture"),
                    directory.join("load_test_results.json"),
                )?;
            }
            if self.fixture.fault == Fault::GeneratorChanged {
                std::fs::write(
                    self.fixture.root.join("generator fixture"),
                    b"changed fixture",
                )?;
            }
        }
        self.poll().await.map(Option::unwrap)
    }
    fn output(&self) -> &[u8] {
        &[]
    }
    fn errors(&self) -> &[u8] {
        &[]
    }
}

#[async_trait::async_trait]
impl Host for Fixture {
    fn spawn(&self, mut command: Command) -> Result<Box<dyn Child>> {
        let role = if command.executable == std::path::Path::new("tcpdump") {
            "capture"
        } else if command
            .arguments
            .first()
            .is_some_and(|argument| argument == "--server")
        {
            "generator"
        } else {
            "server"
        };
        let mut journal = self.journal.lock().unwrap();
        assert!(journal.probes > 0, "Port probes precede every owned launch");
        assert!(
            !command.cleanup,
            "Only post-stop readers may bypass cancellation"
        );
        assert!(
            !command
                .environment
                .contains_key(&OsString::from("DATABASE_URL"))
        );
        assert!(
            !command
                .environment
                .contains_key(&OsString::from("HTTPS_PROXY"))
        );
        journal.environments.push(command.environment);
        journal.arguments.push(command.arguments.clone());
        if role == "generator" && self.fault == Fault::GeneratorSpawn {
            anyhow::bail!("secret-sentinel spawn failure");
        }
        journal.spawned.push(role);
        journal.active.insert(role);
        drop(journal);
        if role == "capture"
            && let Sink::File(file) = &mut command.stdout
        {
            file.write_all(b"listening on lo0\n")?;
        }
        let now = self.clock.load(Ordering::SeqCst);
        let output_directory = if role == "generator" {
            Some(
                command
                    .arguments
                    .windows(2)
                    .find(|pair| pair[0] == "--output-dir")
                    .map(|pair| PathBuf::from(&pair[1]))
                    .unwrap(),
            )
        } else {
            None
        };
        if let Some(directory) = &output_directory {
            let mut summary = self.summary.clone();
            if self.fault == Fault::ForeignSummary {
                summary["run"]["provenance"]["generatorBinarySha256"] = json!("foreign");
            }
            // Observed leader exit is insufficient: the actual report appears
            // only once stop has successfully closed the owned process/sinks.
            artifacts::json(&directory.join("pending-summary.fixture"), &summary)?;
            if self.fault == Fault::UnsafeResults {
                artifacts::new_file(
                    &directory.join("pending-results.fixture"),
                    b"private sentinel",
                )?;
            } else {
                artifacts::json(&directory.join("pending-results.fixture"), &json!([]))?;
            }
            let occupied = match self.fault {
                Fault::Timeout => Some("load_test_timeout.json"),
                Fault::ReportWrite => Some("server-media-report.json"),
                Fault::TimelineWrite => Some("lifecycle-timeline.json"),
                Fault::ResourcesWrite => Some("resources.json"),
                _ => None,
            };
            if let Some(file) = occupied {
                artifacts::new_file(&directory.join(file), b"owned sentinel")?;
            }
        }
        let expires = match role {
            "generator" => Some(now + 6500),
            "server" if self.fault == Fault::ServerExit => Some(now + 1000),
            "capture" if self.fault == Fault::CaptureExit => Some(now + 1000),
            _ => None,
        };
        Ok(Box::new(Process {
            fixture: self.clone(),
            role,
            expires,
            retired: false,
            artifacts: output_directory,
        }))
    }
    async fn cancelled(&self) -> u8 {
        if self.fault != Fault::Interrupt {
            return std::future::pending().await;
        }
        loop {
            if self.clock.load(Ordering::SeqCst) >= 4000 {
                return 143;
            }
            tokio::task::yield_now().await;
        }
    }
}

impl Ports for Fixture {
    fn available(&self, port: u16, media: u16, workers: u32) -> Result<()> {
        assert_eq!((port, media, workers), (3129, 41100, 1));
        self.journal.lock().unwrap().probes += 1;
        if self.fault == Fault::Port {
            anyhow::bail!("Port unavailable");
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl Metrics for Fixture {
    async fn healthy(&self, origin: &str) -> Result<bool> {
        assert_eq!(origin, "http://127.0.0.1:3129");
        Ok(self.fault != Fault::Health)
    }
    async fn fetch(&self, origin: &str, token: &str) -> Result<Snapshot> {
        assert_eq!(origin, "http://127.0.0.1:3129");
        assert_eq!(token.len(), 64);
        let value = if self.fault == Fault::Cleanup {
            1.0
        } else {
            0.0
        };
        Ok(Snapshot {
            raw: "fixed private metrics\n".into(),
            values: ["connections", "rooms", "participants"]
                .into_iter()
                .map(|name| (format!("simplestchat_{name}_active"), value))
                .collect(),
        })
    }
}

#[async_trait::async_trait]
impl Resources for Fixture {
    async fn sample(&self, _pid: u32) -> Option<ProcessSample> {
        self.journal.lock().unwrap().resources += 1;
        Some(ProcessSample {
            rss_ki_b: 1024.0,
            cpu_seconds: self.elapsed_ms() / 1000.0,
        })
    }
}

#[async_trait::async_trait]
impl Media for Fixture {
    async fn fetch(&self, _origin: &str, _token: &str) -> Value {
        json!({"status":"error","issue":"media_unavailable"})
    }
}

#[async_trait::async_trait]
impl Reports for Fixture {
    async fn read(&self, request: Request<'_>) -> Result<Value> {
        let mut journal = self.journal.lock().unwrap();
        assert!(
            journal.active.is_empty(),
            "Every report requires stopped owned processes"
        );
        journal.reports.push(request.mode.into());
        if [Fault::Timeout, Fault::ForeignSummary, Fault::UnsafeResults].contains(&self.fault)
            && ["media", "receiver-stall"].contains(&request.mode)
        {
            assert!(
                !request.admitted,
                "Rejected results cannot be read back by the generator"
            );
        }
        if self.fault == Fault::Report && request.mode == "diagnostics" {
            anyhow::bail!("secret-sentinel child payload");
        }
        if !request.admitted && ["media", "receiver-stall"].contains(&request.mode) {
            return Ok(json!({"schemaVersion":1,"coverage":{"available":false,
                "complete":false,"issues":["generator_correlation_unavailable"]}}));
        }
        Ok(
            json!({"schemaVersion":1,"coverage":{"available":true,"complete":true,
            "issues":[],"scope":"fixture"}}),
        )
    }
}

async fn run(fixture: &Fixture, options: &Options, manifest: &Manifest) -> Result<Value> {
    let environment = [
        ("PATH".into(), "/fixture tools".into()),
        ("DATABASE_URL".into(), "secret-sentinel".into()),
        ("HTTPS_PROXY".into(), "secret-sentinel".into()),
    ]
    .into_iter()
    .collect();
    run_one(Context {
        options,
        scenario: &options.scenarios[0],
        variant: "baseline",
        repetition: 1,
        manifest,
        services: &fixture.services(&environment),
    })
    .await
}

#[tokio::test]
async fn successful_diagnostics_require_stopped_children_independent_reports_and_private_files() {
    let (fixture, options, manifest) = Fixture::new(Fault::None);
    let row = run(&fixture, &options, &manifest).await.unwrap();
    assert_eq!(row["passed"], true);
    let journal = fixture.journal.lock().unwrap();
    assert!(journal.active.is_empty());
    assert_eq!(journal.stopped, ["generator", "server"]);
    assert_eq!(
        journal.reports,
        ["receiver-stall", "lifecycle", "media", "diagnostics"]
    );
    assert_eq!(
        journal.resources, 0,
        "Diagnostic measurements never collect performance samples"
    );
    drop(journal);
    let timeline = artifacts::read_json(
        &fixture.result_path().join("lifecycle-timeline.json"),
        65536,
    )
    .unwrap();
    assert_eq!(
        timeline["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|event| event["event"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "run_started",
            "server_ready",
            "generator_started",
            "generator_exited",
            "cleanup_observed",
            "server_stop_requested",
            "server_exited"
        ]
    );
    let invocation =
        std::fs::read_to_string(fixture.result_path().join("invocation.json")).unwrap();
    assert!(!invocation.contains("secret-sentinel"));
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(fixture.result_path().join("result.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    fixture.clean();
}

#[tokio::test]
async fn workload_failures_still_attempt_every_owned_stop_and_poststop_evidence() {
    for fault in [
        Fault::Port,
        Fault::Health,
        Fault::GeneratorSpawn,
        Fault::GeneratorExit,
        Fault::ServerExit,
        Fault::ForeignSummary,
        Fault::Timeout,
        Fault::Cleanup,
        Fault::Interrupt,
    ] {
        let (fixture, options, manifest) = Fixture::new(fault);
        assert!(
            run(&fixture, &options, &manifest).await.is_err(),
            "{fault:?}"
        );
        let journal = fixture.journal.lock().unwrap();
        assert!(journal.active.is_empty(), "{fault:?}");
        assert_eq!(journal.spawned.len(), journal.stopped.len(), "{fault:?}");
        assert_eq!(
            journal.reports.len(),
            4,
            "One report cannot suppress others: {fault:?}"
        );
        drop(journal);
        let failure = std::fs::read_to_string(fixture.result_path().join("failure.json")).unwrap();
        assert!(!failure.contains("secret-sentinel"));
        assert!(fixture.result_path().join("resources.json").is_file());
        assert!(!fixture.result_path().join("result.json").exists());
        fixture.clean();
    }
}

#[tokio::test]
async fn finalizer_failures_cannot_publish_success_or_skip_other_owned_cleanup() {
    for fault in [
        Fault::Report,
        Fault::ReportWrite,
        Fault::TimelineWrite,
        Fault::ResourcesWrite,
        Fault::GeneratorStop,
        Fault::GeneratorChanged,
        Fault::ServerStop,
    ] {
        let (fixture, options, manifest) = Fixture::new(fault);
        let outcome = run(&fixture, &options, &manifest).await;
        if fault == Fault::ReportWrite {
            assert_eq!(outcome.unwrap()["passed"], false);
        } else {
            assert!(outcome.is_err(), "{fault:?}");
        }
        let journal = fixture.journal.lock().unwrap();
        assert!(journal.active.is_empty());
        assert_eq!(journal.stopped, ["generator", "server"]);
        assert_eq!(
            journal.reports.len(),
            if [Fault::GeneratorStop, Fault::ServerStop].contains(&fault) {
                0
            } else {
                4
            }
        );
        drop(journal);
        let result = fixture.result_path().join("result.json");
        if result.exists() {
            let value = artifacts::read_json(&result, 65536).unwrap();
            assert_eq!(value["passed"], false, "{fault:?}");
            assert!(!value.to_string().contains("secret-sentinel"));
        }
        if fault == Fault::ResourcesWrite {
            assert_eq!(
                std::fs::read(fixture.result_path().join("resources.json")).unwrap(),
                b"owned sentinel"
            );
        }
        fixture.clean();
    }
}

#[tokio::test]
async fn capture_failures_retire_all_three_owned_processes_without_a_comparison_row() {
    for fault in [Fault::None, Fault::CaptureStop, Fault::CaptureExit] {
        let (fixture, mut options, manifest) = Fixture::new(fault);
        options.capture_interface = Some("lo0".into());
        let result = run(&fixture, &options, &manifest).await;
        assert_eq!(result.is_ok(), fault == Fault::None);
        let journal = fixture.journal.lock().unwrap();
        assert!(journal.active.is_empty());
        assert!(journal.stopped.contains(&"capture"));
        assert!(journal.stopped.contains(&"server"));
        drop(journal);
        fixture.clean();
    }
}

#[tokio::test]
async fn performance_uses_actual_resource_window_and_keeps_diagnostics_unrequested() {
    let (fixture, mut options, manifest) = Fixture::new(Fault::None);
    options.purpose = options::Purpose::Performance;
    let row = run(&fixture, &options, &manifest).await.unwrap();
    assert_eq!(row["passed"], true);
    assert_eq!(row["receivedPacketsPerSecond"], 30.0);
    assert_eq!(row["serverCpuPercent"], 100.0);
    assert_eq!(row["serverPeakRssMiB"], 1.0);
    let journal = fixture.journal.lock().unwrap();
    assert!(journal.reports.is_empty());
    assert!(journal.resources > 10);
    assert!(journal.active.is_empty());
    drop(journal);
    fixture.clean();
}

#[tokio::test]
async fn changed_generator_cannot_publish_a_passing_performance_result() {
    let (fixture, mut options, manifest) = Fixture::new(Fault::GeneratorChanged);
    options.purpose = options::Purpose::Performance;
    assert!(run(&fixture, &options, &manifest).await.is_err());
    let result = artifacts::read_json(&fixture.result_path().join("result.json"), 65536).unwrap();
    assert_eq!(result["passed"], false);
    assert_eq!(
        result["finalizerIssues"],
        json!(["generator_identity_changed"])
    );
    assert!(fixture.journal.lock().unwrap().active.is_empty());
    fixture.clean();
}

#[tokio::test]
async fn rejected_results_remain_unavailable_for_every_poststop_reader() {
    let (fixture, options, manifest) = Fixture::new(Fault::UnsafeResults);
    let row = run(&fixture, &options, &manifest).await.unwrap();
    assert_eq!(row["workloadPassed"], true);
    assert_eq!(row["passed"], false);
    assert_eq!(row["mediaDiagnosticCoverage"]["complete"], false);
    assert_eq!(row["receiverStallReport"]["available"], false);
    assert_eq!(fixture.journal.lock().unwrap().reports.len(), 4);
    fixture.clean();
}

#[tokio::test]
async fn ci_receipt_requires_six_existing_workload_finalizers_and_keeps_cleanup_failure() {
    for fault in [Fault::None, Fault::ServerStop] {
        let (fixture, mut options, manifest) = Fixture::new(fault);
        options.purpose = options::Purpose::Performance;
        let environment = BTreeMap::new();
        let observed = super::receipt::ObservedHost::new(&fixture);
        observed.bind_output(&options.output).unwrap();
        let mut services = fixture.services(&environment);
        services.host = &observed;
        for repetition in 1..=3 {
            for variant in ["baseline", "candidate"] {
                let row = run_one(Context {
                    options: &options,
                    scenario: &options.scenarios[0],
                    variant,
                    repetition,
                    manifest: &manifest,
                    services: &services,
                })
                .await;
                if row.is_ok() {
                    observed.completed_run();
                }
                if fault == Fault::None {
                    assert_eq!(row.unwrap()["passed"], true);
                } else {
                    assert!(row.is_err());
                }
            }
        }
        let receipt = observed.receipt(
            "12345678-1234-4234-8234-123456789abc",
            fault == Fault::None,
            fault == Fault::None,
        );
        observed.publish(&receipt).unwrap();
        let value =
            artifacts::read_json(&options.output.join("comparison-owner.json"), 16384).unwrap();
        assert_eq!(value["passed"], fault == Fault::None);
        assert_eq!(value["runs"], if fault == Fault::None { 6 } else { 0 });
        let journal = fixture.journal.lock().unwrap();
        assert!(journal.active.is_empty());
        assert_eq!(journal.stopped.len(), 12);
        drop(journal);
        fixture.clean();
    }
}
