//! Fake metadata commands and owned proc-style files; no host probe is started.

use super::super::process::{Child, Stream};
use super::*;
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::PathBuf;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("environment-fixture-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let fixture = Self(root);
        fixture.write(
            "cpuinfo",
            "processor : 0\nmodel name : Fixture CPU\n\nprocessor : 2\nmodel name : Other CPU\n",
        );
        fixture.write("meminfo", "MemTotal:       33554432 kB\nMemFree: 200 kB\n");
        fixture.write("loadavg", "0.11 0.23 0.34 2/345 12345\n");
        fixture
    }

    fn write(&self, name: &str, value: &str) {
        fs::write(self.0.join(name), value).unwrap();
    }

    async fn observe(&self, host: &FakeHost, platform: &str, arch: &str) -> Result<Value> {
        let environment = BTreeMap::from([
            ("PATH".into(), "/fixture/bin".into()),
            ("LANG".into(), "C.UTF-8".into()),
            ("DATABASE_URL".into(), "private-database-sentinel".into()),
            ("HTTP_PROXY".into(), "private-proxy-sentinel".into()),
        ]);
        observe_for(
            host,
            Path::new("/literal $(not a shell) workspace"),
            &environment,
            platform,
            arch,
            &self.0,
        )
        .await
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

#[derive(Clone)]
struct Response {
    output: Vec<u8>,
    exit: Exit,
    poll_error: bool,
    stop_error: bool,
    cancel: bool,
}

impl Response {
    fn text(value: &str) -> Self {
        Self {
            output: value.as_bytes().to_vec(),
            exit: Exit {
                code: Some(0),
                signal: None,
            },
            poll_error: false,
            stop_error: false,
            cancel: false,
        }
    }
}

#[derive(Default)]
struct FakeHost {
    responses: BTreeMap<String, Response>,
    calls: Mutex<Vec<(String, Vec<String>)>>,
    stopped: Arc<AtomicUsize>,
    cancelled: Arc<AtomicBool>,
}

impl FakeHost {
    fn linux() -> Self {
        Self {
            responses: BTreeMap::from([("uname -r".into(), Response::text("6.12.0-fixture\n"))]),
            ..Self::default()
        }
    }

    fn darwin() -> Self {
        Self {
            responses: [
                ("uname -r", "25.0.0\n"),
                ("sysctl -n machdep.cpu.brand_string", "Apple fixture\n"),
                ("sysctl -n hw.ncpu", "12\n"),
                ("sysctl -n hw.memsize", "34359738368\n"),
                ("sysctl -n vm.loadavg", "{ 1.125 2.25 3.5 }\n"),
            ]
            .into_iter()
            .map(|(key, value)| (key.into(), Response::text(value)))
            .collect(),
            ..Self::default()
        }
    }
}

struct FakeChild {
    response: Response,
    stopped: Arc<AtomicUsize>,
    cancelled: Arc<AtomicBool>,
    closed: bool,
}

#[async_trait::async_trait]
impl Host for FakeHost {
    fn spawn(&self, command: Command) -> Result<Box<dyn Child>> {
        let program = command.executable.to_str().unwrap().to_owned();
        let key = format!("{program} {}", command.arguments.join(" "));
        self.calls
            .lock()
            .unwrap()
            .push((program, command.arguments));
        assert_eq!(
            command.directory,
            Path::new("/literal $(not a shell) workspace")
        );
        assert_eq!(
            command.environment,
            BTreeMap::from([
                ("PATH".into(), "/fixture/bin".into()),
                ("LANG".into(), "C.UTF-8".into()),
                ("LC_ALL".into(), "C".into()),
            ])
        );
        assert_eq!(command.timeout, Duration::from_secs(2));
        assert_eq!(command.stdout_limit, 4096);
        assert_eq!(command.stderr_limit, 4096);
        assert!(command.input.is_empty());
        assert!(!command.cleanup);
        assert!(matches!(command.stdout, Sink::Capture));
        assert!(matches!(command.stderr, Sink::Capture));
        let response = self
            .responses
            .get(&key)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("private-spawn-sentinel"))?;
        Ok(Box::new(FakeChild {
            response,
            stopped: self.stopped.clone(),
            cancelled: self.cancelled.clone(),
            closed: false,
        }))
    }

    async fn cancelled(&self) -> u8 {
        if self.cancelled.load(Ordering::SeqCst) {
            143
        } else {
            std::future::pending().await
        }
    }
}

#[async_trait::async_trait]
impl Child for FakeChild {
    fn pid(&self) -> u32 {
        42
    }
    fn take_stdout(&mut self) -> Option<Box<dyn Stream>> {
        None
    }
    async fn poll(&mut self) -> Result<Option<Exit>> {
        if self.response.cancel {
            self.cancelled.store(true, Ordering::SeqCst);
            return Ok(None);
        }
        ensure!(!self.response.poll_error, "private-poll-sentinel");
        Ok(Some(self.response.exit.clone()))
    }
    async fn stop(&mut self, grace: Duration) -> Result<Exit> {
        assert_eq!(grace, Duration::ZERO);
        self.stopped.fetch_add(1, Ordering::SeqCst);
        ensure!(!self.response.stop_error, "private-close-sentinel");
        self.closed = true;
        Ok(self.response.exit.clone())
    }
    fn output(&self) -> &[u8] {
        assert!(
            self.closed,
            "Observation output requires successful owned cleanup"
        );
        &self.response.output
    }
    fn errors(&self) -> &[u8] {
        b"private-stderr-sentinel"
    }
}

#[tokio::test]
async fn linux_records_original_host_fields_and_documents_load_average_precision() {
    let fixture = Fixture::new();
    let host = FakeHost::linux();
    let value = fixture.observe(&host, "linux", "x86_64").await.unwrap();
    assert_eq!(
        value,
        json!({"platform":"linux","release":"6.12.0-fixture","arch":"x64",
        "cpu":"Fixture CPU","logicalCpus":2,"memoryBytes":34359738368_u64,
        "loadAverageBefore":[0.11,0.23,0.34],"implementation":"rust",
        "loadAverageSource":"/proc/loadavg (rounded to two decimals)","unavailableObservations":[]})
    );
    assert!(value.get("node").is_none());
    assert_eq!(
        *host.calls.lock().unwrap(),
        [("uname".into(), vec!["-r".into()])]
    );
    assert_eq!(host.stopped.load(Ordering::SeqCst), 1);
    assert!(!value.to_string().contains("12345"));
    assert!(!value.to_string().contains("private-"));
}

#[tokio::test]
async fn darwin_uses_fixed_metadata_argv_and_node_architecture_labels() {
    let fixture = Fixture::new();
    let host = FakeHost::darwin();
    let value = fixture.observe(&host, "macos", "aarch64").await.unwrap();
    assert_eq!(
        value,
        json!({"platform":"darwin","release":"25.0.0","arch":"arm64",
        "cpu":"Apple fixture","logicalCpus":12,"memoryBytes":34359738368_u64,
        "loadAverageBefore":[1.125,2.25,3.5],"implementation":"rust",
        "loadAverageSource":"sysctl vm.loadavg","unavailableObservations":[]})
    );
    let calls: BTreeSet<_> = host.calls.lock().unwrap().iter().cloned().collect();
    assert_eq!(
        calls,
        BTreeSet::from([
            ("uname".into(), vec!["-r".into()]),
            (
                "sysctl".into(),
                vec!["-n".into(), "machdep.cpu.brand_string".into()]
            ),
            ("sysctl".into(), vec!["-n".into(), "hw.ncpu".into()]),
            ("sysctl".into(), vec!["-n".into(), "hw.memsize".into()]),
            ("sysctl".into(), vec!["-n".into(), "vm.loadavg".into()]),
        ])
    );
    assert_eq!(host.stopped.load(Ordering::SeqCst), 5);
}

#[tokio::test]
async fn unavailable_or_invalid_metadata_is_null_instead_of_a_fabricated_zero() {
    let fixture = Fixture::new();
    for (key, field, body) in [
        ("uname -r", "release", b"\xff".to_vec()),
        ("sysctl -n machdep.cpu.brand_string", "cpu", vec![b'a'; 257]),
        ("sysctl -n hw.ncpu", "logicalCpus", b"0\n".to_vec()),
        (
            "sysctl -n hw.memsize",
            "memoryBytes",
            b"9007199254740992\n".to_vec(),
        ),
        (
            "sysctl -n vm.loadavg",
            "loadAverageBefore",
            b"{ NaN 1 2 }\n".to_vec(),
        ),
        (
            "uname -r",
            "release",
            vec![b'a'; COMMAND_LIMIT as usize + 1],
        ),
    ] {
        let mut host = FakeHost::darwin();
        host.responses.get_mut(key).unwrap().output = body;
        let value = fixture.observe(&host, "macos", "aarch64").await.unwrap();
        assert_eq!(value[field], Value::Null, "{field}");
        assert_eq!(value["unavailableObservations"], json!([field]));
        assert_eq!(host.stopped.load(Ordering::SeqCst), 5);
    }
    for (code, signal) in [(Some(7), None), (None, Some("SIGTERM".into()))] {
        let mut host = FakeHost::linux();
        host.responses.get_mut("uname -r").unwrap().exit = Exit { code, signal };
        let value = fixture.observe(&host, "linux", "x86_64").await.unwrap();
        assert_eq!(value["release"], Value::Null);
        assert_eq!(value["unavailableObservations"], json!(["release"]));
        assert_eq!(host.stopped.load(Ordering::SeqCst), 1);
    }
    let value = fixture
        .observe(&FakeHost::default(), "unsupported", "x86")
        .await
        .unwrap();
    assert_eq!(value["arch"], "ia32");
    assert_eq!(
        value["unavailableObservations"],
        json!([
            "release",
            "cpu",
            "logicalCpus",
            "memoryBytes",
            "loadAverageBefore"
        ])
    );
    assert_eq!(value["loadAverageSource"], Value::Null);
    assert!(!value.to_string().contains("private-spawn-sentinel"));
}

#[tokio::test]
async fn proc_style_inputs_refuse_links_special_files_growth_and_malformed_values() {
    let fixture = Fixture::new();
    let host = FakeHost::linux();
    let outside = fixture.0.join("private-value");
    fixture.write("private-value", "MemTotal: 123 kB\n");
    fs::remove_file(fixture.0.join("meminfo")).unwrap();
    symlink(&outside, fixture.0.join("meminfo")).unwrap();
    let value = fixture.observe(&host, "linux", "x86_64").await.unwrap();
    assert_eq!(value["memoryBytes"], Value::Null);
    fs::remove_file(fixture.0.join("meminfo")).unwrap();
    nix::unistd::mkfifo(&fixture.0.join("meminfo"), Mode::from_bits_truncate(0o600)).unwrap();
    assert_eq!(
        fixture.observe(&host, "linux", "x86_64").await.unwrap()["memoryBytes"],
        Value::Null
    );
    fs::remove_file(fixture.0.join("meminfo")).unwrap();
    fixture.write("meminfo", "MemTotal: 18446744073709551615 kB\n");
    fixture.write("cpuinfo", "processor: 0\nprocessor: 0\nmodel name: CPU\n");
    fixture.write("loadavg", "0 -1 3 4/5 6\n");
    let value = fixture.observe(&host, "linux", "x86_64").await.unwrap();
    assert_eq!(value["cpu"], "CPU");
    assert_eq!(
        value["unavailableObservations"],
        json!(["logicalCpus", "memoryBytes", "loadAverageBefore"])
    );
    File::options()
        .write(true)
        .open(fixture.0.join("cpuinfo"))
        .unwrap()
        .set_len(CPU_LIMIT + 1)
        .unwrap();
    assert_eq!(
        fixture.observe(&host, "linux", "x86_64").await.unwrap()["cpu"],
        Value::Null
    );
    let link = fixture.0.join("linked-proc");
    symlink(&fixture.0, &link).unwrap();
    let value = observe_for(
        &host,
        Path::new("/literal $(not a shell) workspace"),
        &BTreeMap::from([
            ("PATH".into(), "/fixture/bin".into()),
            ("LANG".into(), "C.UTF-8".into()),
        ]),
        "linux",
        "x86_64",
        &link,
    )
    .await
    .unwrap();
    assert_eq!(
        value["unavailableObservations"],
        json!(["cpu", "logicalCpus", "memoryBytes", "loadAverageBefore"])
    );
}

#[tokio::test]
async fn cancellation_and_cleanup_failures_refuse_without_leaking_outputs_or_skipping_other_children()
 {
    let fixture = Fixture::new();
    let host = FakeHost::linux();
    host.cancelled.store(true, Ordering::SeqCst);
    let error = fixture.observe(&host, "linux", "x86_64").await.unwrap_err();
    assert_eq!(error.to_string(), FAILURE);
    assert!(host.calls.lock().unwrap().is_empty());
    let mut host = FakeHost::linux();
    host.responses.get_mut("uname -r").unwrap().cancel = true;
    assert_eq!(
        fixture
            .observe(&host, "linux", "x86_64")
            .await
            .unwrap_err()
            .to_string(),
        FAILURE
    );
    assert_eq!(host.stopped.load(Ordering::SeqCst), 1);
    let mut host = FakeHost::darwin();
    host.responses
        .get_mut("sysctl -n hw.memsize")
        .unwrap()
        .stop_error = true;
    let error = fixture
        .observe(&host, "macos", "aarch64")
        .await
        .unwrap_err();
    assert_eq!(error.to_string(), FAILURE);
    assert!(!format!("{error:#}").contains("private-"));
    assert_eq!(host.calls.lock().unwrap().len(), 5);
    assert_eq!(host.stopped.load(Ordering::SeqCst), 5);
    let mut host = FakeHost::linux();
    host.responses.get_mut("uname -r").unwrap().poll_error = true;
    let value = fixture.observe(&host, "linux", "x86_64").await.unwrap();
    assert_eq!(value["release"], Value::Null);
    assert!(!value.to_string().contains("private-"));
    assert_eq!(host.stopped.load(Ordering::SeqCst), 1);
}
