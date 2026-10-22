//! Complete fake-engine lifecycles exercise ownership and evidence ordering.

use super::{
    controller,
    engine::{Engine, MIB},
    options,
    ownership::LABEL,
};
use crate::bench::{
    artifacts,
    clock::Clock,
    metrics::{Metrics, Snapshot},
    process::{Child, Command, Exit, Host, Sink, Stream},
};
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};
use std::time::Duration;

#[derive(Default)]
struct Time(AtomicU64);
#[async_trait::async_trait]
impl Clock for Time {
    fn elapsed_ms(&self) -> f64 {
        self.0.load(Ordering::SeqCst) as f64
    }
    fn utc(&self) -> String {
        "2026-10-21T12:00:00.000Z".into()
    }
    async fn sleep(&self, duration: Duration) {
        self.0
            .fetch_add(duration.as_millis() as u64, Ordering::SeqCst);
        for _ in 0..12 {
            tokio::task::yield_now().await;
        }
    }
}
struct Health {
    ready: bool,
    fail: bool,
}
#[async_trait::async_trait]
impl Metrics for Health {
    async fn healthy(&self, origin: &str) -> Result<bool> {
        assert!(origin.starts_with("http://127.0.0.1:"));
        Ok(self.ready)
    }
    async fn fetch(&self, _: &str, token: &str) -> Result<Snapshot> {
        assert_eq!(token.len(), 64);
        ensure!(!self.fail, "Fixture metrics failed");
        Ok(Snapshot {
            raw: "owned metrics\n".into(),
            values: BTreeMap::from([
                ("simplestchat_connections_active".into(), 0.0),
                ("simplestchat_rooms_active".into(), 0.0),
                ("simplestchat_participants_active".into(), 0.0),
                ("simplestchat_consumers_created_total".into(), 2.0),
            ]),
        })
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Failure {
    None,
    Create,
    Running,
    Exit,
    Shutdown,
    Archive,
    Close,
    Stream,
    Interrupted,
    Foreign,
    Paused,
    Restarting,
    Remove,
    InvalidImage,
    ImagePrefix,
    WrongImage,
}
#[derive(Clone)]
struct Container {
    name: String,
    image: String,
    image_id: String,
    run: String,
    id: String,
    running: bool,
    code: i32,
}
struct State {
    containers: Vec<Container>,
    calls: Vec<Vec<String>>,
    copied: Vec<String>,
    tags: BTreeMap<String, String>,
}
struct FakeHost {
    state: Arc<Mutex<State>>,
    time: Arc<Time>,
    failure: Failure,
}
impl FakeHost {
    fn new(time: Arc<Time>, failure: Failure) -> Self {
        Self {
            state: Arc::new(Mutex::new(State {
                containers: Vec::new(),
                calls: Vec::new(),
                copied: Vec::new(),
                tags: BTreeMap::new(),
            })),
            time,
            failure,
        }
    }
}
struct Fixture {
    root: PathBuf,
    host: Arc<FakeHost>,
    engine: Engine,
    options: options::Options,
}
impl Fixture {
    fn new(failure: Failure, capture: bool) -> Self {
        let root = std::env::temp_dir().join(format!("podman-runtime-{}", uuid::Uuid::new_v4()));
        artifacts::directory(&root).unwrap();
        let executable = root.join("podman");
        fs::write(&executable, b"fixture; never executed").unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        let time = Arc::new(Time::default());
        let host = Arc::new(FakeHost::new(time.clone(), failure));
        let engine = Engine::new(
            host.clone(),
            time,
            root.clone(),
            BTreeMap::from([("PATH".into(), root.clone().into_os_string())]),
        )
        .unwrap();
        let arguments = vec![
            "--server-image",
            "server:test",
            "--generator-image",
            "generator:test",
            "--output",
            "output",
            "--clients",
            "2",
            "--ramp-up",
            "1",
            "--warmup",
            "2",
            "--duration",
            "3",
            "--scenarios",
            "conference",
            "--capture-image",
            "capture:test",
            "--netem-image",
            "netem:test",
            "--capture-handshakes",
            if capture { "true" } else { "false" },
        ]
        .into_iter()
        .map(Into::into)
        .collect::<Vec<_>>();
        let options = options::parse(&arguments, &root).unwrap();
        Self {
            root,
            host,
            engine,
            options,
        }
    }
    async fn execute(&self, ready: bool, fail_metrics: bool) -> Result<Value> {
        controller::execute(
            &self.options,
            &self.engine,
            &Health {
                ready,
                fail: fail_metrics,
            },
            &self.root.join("podman"),
        )
        .await
    }
    fn run(&self) -> PathBuf {
        self.options.output.join("conference-2-1")
    }
    fn evidence(&self, name: &str) -> Value {
        serde_json::from_slice(&fs::read(self.run().join(name)).unwrap()).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}

fn option(args: &[String], key: &str) -> String {
    args.windows(2)
        .find(|pair| pair[0] == key)
        .map_or(String::new(), |pair| pair[1].clone())
}
fn image(args: &[String]) -> String {
    args.iter()
        .find(|arg| super::engine::valid_image_id(arg))
        .cloned()
        .unwrap_or_else(|| {
            args.iter()
                .find(|arg| {
                    [
                        "server:test",
                        "generator:test",
                        "netem:test",
                        "capture:test",
                    ]
                    .contains(&arg.as_str())
                })
                .cloned()
                .unwrap_or_default()
        })
}

fn archive(valid: bool, capture: bool) -> Vec<u8> {
    if !valid {
        return b"invalid archive".to_vec();
    }
    let latency = serde_json::to_value(crate::LatencyStats::default()).unwrap();
    let summary = json!({"schemaVersion":2,"run":{"completed":true,"passed":true,
        "failureReasons":[],"startedAt":"s","finishedAt":"f","configuration":{},"provenance":{}},
        "totalErrors":0,"failedConnections":0,"failedConsumers":0,"validatedConsumers":2,
        "skippedShortLivedConsumers":0,"p99ConnectionTimeMs":5,"sendMediaReady":latency,
        "receiveMediaReady":latency,"measurement":{"durationMs":3000,"packetsQueued":30,
        "packetsReceived":30,"bytesQueued":300,"bytesReceived":300}});
    let summary = serde_json::to_vec(&summary).unwrap();
    let entries: Vec<(&str, &[u8])> = if capture {
        vec![("handshakes.pcap", b"capture")]
    } else {
        vec![
            ("load_test_summary.json", &summary),
            ("load_test_results.json", b"[]"),
        ]
    };
    let mut archive = tar::Builder::new(Vec::new());
    for (name, bytes) in entries {
        let mut header = tar::Header::new_ustar();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o600);
        header.set_cksum();
        archive.append_data(&mut header, name, bytes).unwrap();
    }
    archive.into_inner().unwrap()
}
fn stat(cpu: u32) -> String {
    let mut fields = vec!["0".to_owned(); 24];
    fields[0] = "S".into();
    fields[11] = cpu.to_string();
    fields[21] = "1000".into();
    format!("1 (tokio-runtime-w) {}", fields.join(" "))
}
struct Samples {
    time: Arc<Time>,
    index: usize,
    server: bool,
    memory: bool,
    fail: bool,
}
#[async_trait::async_trait]
impl Stream for Samples {
    async fn read(&mut self, buffer: &mut [u8]) -> Result<usize> {
        tokio::task::yield_now().await;
        ensure!(!self.fail, "Fixture sampler stream failure");
        if self.index >= 2 {
            return Ok(0);
        }
        let elapsed = if self.index == 0 { 3000 } else { 6000 };
        self.index += 1;
        self.time.0.store(elapsed, Ordering::SeqCst);
        let block = if self.memory {
            "Rss: 1000 kB\n@@M\n@@C\nanon 10\n@@E\n".into()
        } else if self.server {
            format!(
                "{}\n@@S\nusage_usec {}\nnr_periods {}\nnr_throttled 1\nthrottled_usec 10\n@@Y\n{}\n@@E\n",
                stat(elapsed as u32 / 10),
                elapsed * 1000,
                elapsed / 100,
                stat(elapsed as u32 / 10)
            )
        } else {
            format!("{}\n@@E\n", stat(elapsed as u32 / 10))
        };
        assert!(block.len() <= buffer.len());
        buffer[..block.len()].copy_from_slice(block.as_bytes());
        Ok(block.len())
    }
}
struct FakeChild {
    exit: Option<Exit>,
    output: Vec<u8>,
    errors: Vec<u8>,
    stdout: Option<File>,
    stderr: Option<File>,
    stream: Option<Samples>,
    close_failure: bool,
    stopped: bool,
}
#[async_trait::async_trait]
impl Child for FakeChild {
    fn pid(&self) -> u32 {
        1
    }
    fn take_stdout(&mut self) -> Option<Box<dyn Stream>> {
        self.stream.take().map(|s| Box::new(s) as Box<dyn Stream>)
    }
    async fn poll(&mut self) -> Result<Option<Exit>> {
        Ok(self.exit.clone())
    }
    async fn stop(&mut self, _: Duration) -> Result<Exit> {
        if !self.stopped {
            self.stopped = true;
            if let Some(mut file) = self.stdout.take() {
                file.write_all(&self.output)?;
                nix::unistd::close(file)?;
            }
            if let Some(mut file) = self.stderr.take() {
                file.write_all(&self.errors)?;
                nix::unistd::close(file)?;
            }
        }
        ensure!(!self.close_failure, "Fixture output close failed");
        Ok(self.exit.clone().unwrap_or(Exit {
            code: Some(0),
            signal: None,
        }))
    }
    fn output(&self) -> &[u8] {
        &self.output
    }
    fn errors(&self) -> &[u8] {
        &self.errors
    }
}
#[async_trait::async_trait]
impl Host for FakeHost {
    fn spawn(&self, command: Command) -> Result<Box<dyn Child>> {
        let args = &command.arguments;
        let mut state = self.state.lock().unwrap();
        state.calls.push(args.clone());
        assert!(
            !args
                .iter()
                .any(|arg| ["--all", "--latest", "--filter"].contains(&arg.as_str()))
        );
        assert!(command.input.is_empty());
        assert_eq!(command.stderr_limit, 2 * MIB);
        let mut output = Vec::new();
        let mut errors = Vec::new();
        let mut code = 0;
        let mut stream = None;
        match args[0].as_str() {
            "info" => output = b"{}".to_vec(),
            "image" => {
                let tag = args.last().unwrap();
                let number = match tag.as_str() {
                    "server:test" => 1,
                    "generator:test" => 2,
                    "netem:test" => 3,
                    "capture:test" => 4,
                    _ => 5,
                };
                let original = state
                    .tags
                    .entry(tag.clone())
                    .or_insert_with(|| format!("sha256:{}", number.to_string().repeat(64)))
                    .clone();
                output = if self.failure == Failure::InvalidImage {
                    b"not-an-image-id".to_vec()
                } else {
                    format!("{original} sha256:digest").into_bytes()
                };
                // Repoint immediately after capture; runtime IDs must stay frozen.
                state
                    .tags
                    .insert(tag.clone(), format!("sha256:{}", "f".repeat(64)));
            }
            "run" => {
                let name = option(args, "--name");
                let image = image(args);
                let run = option(args, "--label");
                let id = format!("{:064x}", state.containers.len() + 1);
                let ephemeral = args.contains(&"--rm".to_owned());
                if !ephemeral {
                    let image_id = state
                        .tags
                        .get(&image)
                        .cloned()
                        .unwrap_or_else(|| image.clone());
                    state.containers.push(Container {
                        name: name.clone(),
                        image_id,
                        image,
                        run: if self.failure == Failure::Foreign {
                            "unrelated-run".into()
                        } else {
                            run.strip_prefix(&format!("{LABEL}=")).unwrap().into()
                        },
                        id: id.clone(),
                        running: true,
                        code: 0,
                    });
                }
                output = if ephemeral && name.starts_with("bench-udp-") {
                    b"Udp: InDatagrams InErrors RcvbufErrors\nUdp: 1 0 0\n@@UDP\n".to_vec()
                } else if ephemeral {
                    b"applied netem".to_vec()
                } else {
                    id.into_bytes()
                };
                if self.failure == Failure::Create && name.starts_with("bench-sfu-") {
                    code = 1;
                    errors = b"Response lost after create".to_vec();
                }
            }
            "inspect" => {
                let target = args.last().unwrap();
                let found = state
                    .containers
                    .iter()
                    .position(|c| &c.id == target || &c.name == target);
                if let Some(index) = found {
                    let container = &mut state.containers[index];
                    if container.name.starts_with("bench-gen-")
                        && !matches!(self.failure, Failure::Paused | Failure::Restarting)
                        && self.time.elapsed_ms()
                            >= if self.failure == Failure::Running {
                                9000.0
                            } else if self.failure == Failure::Interrupted {
                                f64::MAX
                            } else {
                                6000.0
                            }
                    {
                        container.running = false;
                        container.code = if self.failure == Failure::Exit { 9 } else { 0 };
                    }
                    if container.name.starts_with("bench-sfu-")
                        && self.failure == Failure::Running
                        && self.time.elapsed_ms() >= 6000.0
                    {
                        container.running = false;
                        container.code = 9;
                    }
                    output = if args.contains(&"--format".to_owned()) {
                        format!(
                            "{} {}",
                            if container.running
                                && container.name.starts_with("bench-gen-")
                                && self.time.elapsed_ms() >= 6000.0
                                && self.failure == Failure::Paused
                            {
                                "paused"
                            } else if container.running
                                && container.name.starts_with("bench-gen-")
                                && self.time.elapsed_ms() >= 6000.0
                                && self.failure == Failure::Restarting
                            {
                                "restarting"
                            } else if container.running {
                                "running"
                            } else {
                                "exited"
                            },
                            container.code
                        )
                        .into_bytes()
                    } else {
                        let actual = if self.failure == Failure::WrongImage {
                            "0".repeat(64)
                        } else {
                            container
                                .image_id
                                .strip_prefix("sha256:")
                                .unwrap_or(&container.image_id)
                                .into()
                        };
                        let actual = if self.failure == Failure::ImagePrefix {
                            format!("sha256:{actual}")
                        } else {
                            actual
                        };
                        let configured = if self.failure == Failure::WrongImage {
                            container.image.clone()
                        } else if container.name.starts_with("bench-sfu-") {
                            "server:test".into()
                        } else if container.name.starts_with("bench-gen-") {
                            "generator:test".into()
                        } else {
                            "capture:test".into()
                        };
                        serde_json::to_vec(
                            &json!([{"Id":container.id,"Name":container.name,"Image":actual,
                        "Config":{"Image":configured,"Labels":{LABEL:container.run}}}]),
                        )
                        .unwrap()
                    };
                } else {
                    code = 1;
                    errors = b"no such container".to_vec();
                }
            }
            "exec" => {
                let script = args.last().unwrap();
                if script == "getconf CLK_TCK" {
                    output = b"100".to_vec();
                } else if script == "getconf PAGESIZE" {
                    output = b"4096".to_vec();
                } else {
                    stream = Some(Samples {
                        time: self.time.clone(),
                        index: 0,
                        server: script.contains("@@S"),
                        memory: script.contains("@@M"),
                        fail: self.failure == Failure::Stream,
                    });
                }
            }
            "stop" | "rm" | "logs" | "cp" => {
                assert!(command.cleanup);
                let target = if args[0] == "stop" {
                    args.last().unwrap().clone()
                } else if args[0] == "cp" {
                    args[1].split(':').next().unwrap().into()
                } else {
                    args.last().unwrap().clone()
                };
                assert!(super::engine::valid_id(&target));
                let index = state
                    .containers
                    .iter()
                    .position(|c| c.id == target)
                    .unwrap();
                if args[0] == "stop" {
                    let container = &mut state.containers[index];
                    container.running = false;
                    if self.failure == Failure::Shutdown && container.name.starts_with("bench-sfu-")
                    {
                        container.code = 9;
                    }
                } else if args[0] == "rm" {
                    if self.failure == Failure::Remove
                        && state.containers[index].name.starts_with("bench-sfu-")
                    {
                        code = 1;
                        errors = b"Fixture removal refused".to_vec();
                    } else {
                        state.containers.remove(index);
                    }
                } else if args[0] == "logs" {
                    output = b"owned log\n".to_vec();
                } else {
                    assert!(
                        !state.containers[index].running,
                        "Artifact source is still mutable"
                    );
                    state.copied.push(target);
                    output = archive(self.failure != Failure::Archive, args[1].ends_with("pcap"));
                }
            }
            other => panic!("Unexpected fake engine command {other}: {args:?}"),
        }
        let stdout = if let Sink::File(file) = command.stdout {
            Some(file)
        } else {
            None
        };
        let stderr = if let Sink::File(file) = command.stderr {
            Some(file)
        } else {
            None
        };
        Ok(Box::new(FakeChild {
            exit: stream.is_none().then_some(Exit {
                code: Some(code),
                signal: None,
            }),
            output,
            errors,
            stdout,
            stderr,
            stream,
            close_failure: self.failure == Failure::Close && args[0] == "cp",
            stopped: false,
        }))
    }
    async fn cancelled(&self) -> u8 {
        if self.failure == Failure::Interrupted {
            while self.time.elapsed_ms() < 6000.0 {
                tokio::task::yield_now().await;
            }
            143
        } else {
            std::future::pending().await
        }
    }
}

#[tokio::test]
async fn complete_fake_run_closes_samples_copies_only_stopped_ids_and_gates_shutdown() {
    for capture in [false, true] {
        let fixture = Fixture::new(Failure::None, capture);
        let comparison = fixture.execute(true, false).await.unwrap();
        assert_eq!(comparison["passed"], true);
        assert_eq!(comparison["rows"][0]["serverShutdownPassed"], true);
        assert_eq!(comparison["rows"][0]["receivedPacketsPerSecond"], 10.0);
        assert_eq!(fixture.evidence("result.json")["passed"], true);
        assert_eq!(
            fixture.evidence("resources.json")["server"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert!(fixture.host.state.lock().unwrap().containers.is_empty());
        assert!(fixture.run().join("collection.json").exists());
        assert_eq!(
            fixture.run().join("capture-collection.json").exists(),
            capture
        );
        assert!(
            !fs::read_to_string(fixture.run().join("invocation.json"))
                .unwrap()
                .contains("METRICS_TOKEN")
        );
    }
    let fixture = Fixture::new(Failure::Shutdown, false);
    let comparison = fixture.execute(true, false).await.unwrap();
    assert_eq!(comparison["completed"], true);
    assert_eq!(comparison["passed"], false);
    assert_eq!(comparison["rows"][0]["workloadPassed"], true);
    assert_eq!(comparison["rows"][0]["serverShutdownPassed"], false);
}

#[tokio::test]
async fn failures_keep_primary_and_partial_evidence_and_remove_every_verified_owner() {
    for failure in [
        Failure::Create,
        Failure::Running,
        Failure::Exit,
        Failure::Archive,
        Failure::Close,
        Failure::Stream,
    ] {
        let fixture = Fixture::new(failure, true);
        assert!(fixture.execute(true, false).await.is_err());
        let comparison: Value = serde_json::from_slice(
            &fs::read(fixture.options.output.join("comparison.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(comparison["completed"], false);
        assert_eq!(comparison["passed"], false);
        assert!(
            fixture.evidence("failure.json")["error"]
                .as_str()
                .is_some_and(|value| !value.is_empty())
        );
        assert!(fixture.host.state.lock().unwrap().containers.is_empty());
        assert!(fixture.run().join("server-exit.json").exists());
        if matches!(failure, Failure::Archive | Failure::Close) {
            assert!(fixture.run().join("generator-results.tar").exists());
            assert!(!fixture.run().join("collection.json").exists());
        }
    }
}

#[tokio::test]
async fn readiness_and_metrics_refusals_stop_the_owned_server_without_starting_load() {
    for (ready, metrics) in [(false, false), (true, true)] {
        let fixture = Fixture::new(Failure::None, false);
        assert!(fixture.execute(ready, metrics).await.is_err());
        assert!(fixture.host.state.lock().unwrap().containers.is_empty());
        let calls = fixture.host.state.lock().unwrap().calls.clone();
        assert!(!calls.iter().any(
            |args| args.iter().any(|value| value.starts_with("bench-gen-")) && args[0] == "run"
        ));
        assert!(fixture.run().join("failure.json").exists());
    }
}

#[tokio::test]
async fn interruption_finalizes_all_owned_inputs_and_cannot_pass_the_workload() {
    let fixture = Fixture::new(Failure::Interrupted, true);
    let error = fixture.execute(true, false).await.unwrap_err();
    assert!(error.to_string().contains("interrupted"));
    assert!(fixture.host.state.lock().unwrap().containers.is_empty());
    assert_eq!(fixture.evidence("failure.json")["generatorStarted"], true);
    assert!(fixture.run().join("collection.json").exists());
    assert!(fixture.run().join("capture-collection.json").exists());
    assert!(!fixture.run().join("result.json").exists());
}

#[tokio::test]
async fn foreign_label_refuses_every_destructive_operation_and_retains_ownership_intent() {
    let fixture = Fixture::new(Failure::Foreign, false);
    // Readiness refusal starts cleanup while the recorded server still exists.
    assert!(fixture.execute(false, false).await.is_err());
    let state = fixture.host.state.lock().unwrap();
    assert_eq!(state.containers.len(), 1);
    assert!(
        !state
            .calls
            .iter()
            .any(|args| ["stop", "rm", "cp", "logs"].contains(&args[0].as_str()))
    );
    assert!(
        fixture.evidence("failure.json")["cleanupErrors"]
            .as_array()
            .unwrap()
            .iter()
            .any(|error| error.as_str().unwrap().contains("identity mismatch"))
    );
    assert_eq!(
        fixture.evidence("ownership.json")["containers"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn paused_and_restarting_sources_are_stopped_before_artifact_transfer() {
    for failure in [Failure::Paused, Failure::Restarting] {
        let fixture = Fixture::new(failure, false);
        assert_eq!(fixture.execute(true, false).await.unwrap()["passed"], true);
        let state = fixture.host.state.lock().unwrap();
        let copy = state.calls.iter().position(|args| args[0] == "cp").unwrap();
        let id = state.calls[copy][1].split(':').next().unwrap();
        assert!(
            state.calls[..copy]
                .iter()
                .any(|args| args[0] == "stop" && args.last().unwrap() == id)
        );
        assert!(state.containers.is_empty());
    }
}

#[tokio::test]
async fn invalid_image_arguments_refuse_before_any_engine_or_output_creation() {
    for image in [
        "--all".into(),
        "server bad".into(),
        "x".repeat(4097),
        "server\0bad".into(),
    ] {
        let mut fixture = Fixture::new(Failure::None, false);
        fixture.options.server_image = image;
        assert!(fixture.execute(true, false).await.is_err());
        assert!(fixture.host.state.lock().unwrap().calls.is_empty());
        assert!(!fixture.options.output.exists());
    }
}

#[tokio::test]
async fn cleanup_failure_keeps_the_actual_workload_and_shutdown_verdicts_without_passing() {
    let fixture = Fixture::new(Failure::Remove, true);
    assert!(fixture.execute(true, false).await.is_err());
    let result = fixture.evidence("result.json");
    assert_eq!(result["workloadPassed"], true);
    assert_eq!(result["serverShutdownPassed"], true);
    assert_eq!(result["cleanupComplete"], false);
    assert_eq!(result["passed"], false);
    let state = fixture.host.state.lock().unwrap();
    assert_eq!(state.containers.len(), 1);
    assert!(!state.containers[0].running);
    assert!(state.containers[0].name.starts_with("bench-sfu-"));
    assert!(
        fixture.evidence("failure.json")["cleanupErrors"]
            .as_array()
            .unwrap()
            .iter()
            .any(|error| error.as_str().unwrap().contains("removal refused"))
    );
}

#[tokio::test]
async fn manifest_tags_are_descriptive_but_every_requested_runtime_image_is_frozen() {
    let mut fixture = Fixture::new(Failure::None, true);
    fixture.options.netem = Some("loss 2%".into());
    assert_eq!(fixture.execute(true, false).await.unwrap()["passed"], true);
    let manifest: Value =
        serde_json::from_slice(&fs::read(fixture.options.output.join("manifest.json")).unwrap())
            .unwrap();
    assert_eq!(manifest["options"]["serverImage"], "server:test");
    assert_eq!(manifest["server"]["image"], "server:test");
    assert_eq!(manifest["generator"]["image"], "generator:test");
    let state = fixture.host.state.lock().unwrap();
    for (role, tag, digit) in [
        ("sfu", "server:test", 1),
        ("gen", "generator:test", 2),
        ("netem", "netem:test", 3),
        ("cap", "capture:test", 4),
        ("udp", "capture:test", 4),
    ] {
        let args = state
            .calls
            .iter()
            .find(|args| {
                args[0] == "run" && option(args, "--name").starts_with(&format!("bench-{role}-"))
            })
            .unwrap();
        assert!(args.contains(&format!("sha256:{}", digit.to_string().repeat(64))));
        assert!(!args.contains(&tag.into()));
        assert_eq!(state.tags[tag], format!("sha256:{}", "f".repeat(64)));
    }
}

#[tokio::test]
async fn unused_sidecars_are_neither_required_nor_inspected_and_malformed_ids_cannot_start_load() {
    let mut fixture = Fixture::new(Failure::None, false);
    fixture.options.netem_image = "--not-needed".into();
    fixture.options.capture_image = "--not-needed".into();
    assert_eq!(fixture.execute(true, false).await.unwrap()["passed"], true);
    assert_eq!(
        fixture
            .host
            .state
            .lock()
            .unwrap()
            .calls
            .iter()
            .filter(|args| args[0] == "image")
            .count(),
        2
    );
    let fixture = Fixture::new(Failure::InvalidImage, true);
    assert!(fixture.execute(true, false).await.is_err());
    assert!(
        !fixture
            .host
            .state
            .lock()
            .unwrap()
            .calls
            .iter()
            .any(|args| args[0] == "run")
    );
    assert!(fixture.host.state.lock().unwrap().containers.is_empty());
}

#[test]
fn image_ids_are_exact_sha256_bodies_with_only_the_optional_explicit_prefix() {
    for value in ["a".repeat(64), format!("sha256:{}", "b".repeat(64))] {
        assert!(super::engine::valid_image_id(&value));
    }
    for value in [
        "a".repeat(63),
        "g".repeat(64),
        format!("sha256:{}", "b".repeat(65)),
        format!("sha512:{}", "a".repeat(128)),
        "server:tag".into(),
    ] {
        assert!(!super::engine::valid_image_id(&value));
    }
}

#[tokio::test]
async fn podman_display_names_do_not_replace_the_real_image_identity_guard() {
    // Ordinary fixtures already return an original tag in Config.Image and
    // a bare full image ID in Image, as real Podman inspection does.
    let fixture = Fixture::new(Failure::ImagePrefix, true);
    assert_eq!(fixture.execute(true, false).await.unwrap()["passed"], true);
    let fixture = Fixture::new(Failure::WrongImage, false);
    assert!(fixture.execute(false, false).await.is_err());
    let state = fixture.host.state.lock().unwrap();
    assert_eq!(state.containers.len(), 1);
    assert!(
        !state
            .calls
            .iter()
            .any(|args| ["stop", "rm", "cp", "logs"].contains(&args[0].as_str()))
    );
    assert!(
        fixture.evidence("failure.json")["cleanupErrors"]
            .as_array()
            .unwrap()
            .iter()
            .any(|error| error.as_str().unwrap().contains("identity mismatch"))
    );
}
