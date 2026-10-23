//! Inert complete-controller transactions and owned filesystem fault boundaries.
use super::*;
use crate::{
    ci::{
        command::{Command, Program},
        ops_artifact::verify::tools::{Captures, Seal},
        process::{self, Executor},
    },
    tooling::{Environment, Result},
};
use serde_json::{Value, json};
use std::{
    cell::RefCell,
    fs,
    io::Write,
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Temporary(PathBuf);
impl Temporary {
    fn new() -> Self {
        let path = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "benchmark82-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        Self(path)
    }
    fn paths(&self) -> files::Paths {
        fs::create_dir(self.0.join("srv")).unwrap();
        files::Paths {
            state: self.0.join("state"),
            public: self.0.join("srv/public"),
            uid: rustix::process::getuid().as_raw(),
            gid: rustix::process::getgid().as_raw(),
            guest_uid: rustix::process::getuid().as_raw(),
            guest_gid: rustix::process::getgid().as_raw(),
        }
    }
}
impl Drop for Temporary {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
fn settings(root: &Temporary) -> config::Settings {
    config::Settings {
        server_image: format!("sha256:{}", "a".repeat(64)),
        generator_image: format!("sha256:{}", "b".repeat(64)),
        output: root.0.join("output"),
        workers: 1,
        clients: 10,
        rooms: 1,
        duration: 3,
        ramp_up: 1,
        warmup: 10,
    }
}
fn cid(role: io::Role) -> String {
    if role == io::Role::Server { "c" } else { "d" }.repeat(64)
}
fn summary(s: &config::Settings, run: &str) -> Value {
    json!({"schemaVersion":2,"run":{"completed":true,"passed":true,
        "configuration":{"numClients":s.clients,"numRooms":s.rooms,"durationSecs":s.duration,
            "rampUpSecs":s.ramp_up,"warmupSecs":s.warmup,"runLabel":run},
        "provenance":{"serverRevision":s.server_image,"generatorRevision":s.generator_image}},
        "totalErrors":0,"failedConnections":0,"failedConsumers":0,"validatedConsumers":80,
        "skippedShortLivedConsumers":0,"attemptCoverage":{"version":1,"scope":"stable-publishers",
            "available":true,"attempts":10,"passedAttempts":10,"failedAttempts":0,
            "missingCoverageAttempts":0,"skippedShortTailAttempts":0,"requestedChurners":0,
            "validatedChurners":0}})
}
struct Lease(Arc<AtomicBool>, bool);
impl io::Namespace for Lease {
    fn check(&self, _: &io::Process) -> Result<()> {
        if self.0.load(Ordering::SeqCst) {
            Err(failure(1))
        } else {
            Ok(())
        }
    }
    fn finish(&self) -> Result<()> {
        self.0.store(true, Ordering::SeqCst);
        if self.1 { Err(failure(1)) } else { Ok(()) }
    }
}
struct Groups(Arc<AtomicBool>);
impl io::Groups for Groups {
    fn sample(&mut self, _: &str, t: [bool; 2]) -> Result<Vec<Value>> {
        Ok(vec![json!({"terminal":t})])
    }
    fn finish(self) -> Result<()> {
        self.0.store(true, Ordering::SeqCst);
        Ok(())
    }
}
struct Observed {
    leases: RefCell<Vec<Arc<AtomicBool>>>,
    groups: Arc<AtomicBool>,
    fail_lease: usize,
}
impl Observed {
    fn new() -> Self {
        Self {
            leases: RefCell::new(Vec::new()),
            groups: Arc::new(AtomicBool::new(false)),
            fail_lease: 0,
        }
    }
}
impl io::Observations for Observed {
    type Namespace = Lease;
    type Groups = Groups;
    fn platform(&self) -> Result<()> {
        Ok(())
    }
    fn snapshot(&self) -> Result<std::collections::BTreeMap<String, Vec<u8>>> {
        Ok([("kernel.txt".into(), b"inert fixture\n".to_vec())].into())
    }
    fn namespace(&self, _: &io::Process) -> Result<Arc<Lease>> {
        let closed = Arc::new(AtomicBool::new(false));
        self.leases.borrow_mut().push(closed.clone());
        let fail = self.fail_lease == self.leases.borrow().len();
        Ok(Arc::new(Lease(closed, fail)))
    }
    fn groups(&self, _: &[io::Process; 2]) -> Result<Groups> {
        Ok(Groups(self.groups.clone()))
    }
    fn curl(
        &self,
        _: &Arc<Lease>,
        e: io::Endpoint,
        input: std::fs::File,
        timeout: Duration,
        cleanup: bool,
    ) -> Result<Command> {
        let mut c = Command::capture(Program::BenchmarkCurl, &[e.path()]);
        c.timeout = timeout;
        c.grace = Duration::from_secs(1);
        c.capture = Some(plan::CAPTURE);
        c.cleanup = cleanup;
        c.stdin = Some(Arc::new(input));
        Ok(c)
    }
}
struct Fake {
    settings: config::Settings,
    time: plan::Time,
    run: String,
    present: [bool; 2],
    running: [bool; 2],
    inspect_generator: usize,
    terminal_after: usize,
    trace: Vec<(bool, Vec<String>)>,
    fault: Option<(&'static str, u8)>,
    cancellation: u8,
    late_replay: Option<u8>,
    clean: bool,
    late: Option<&'static str>,
    request_error: Option<(&'static str, u8)>,
    metrics: usize,
    public_running: bool,
    groups: Arc<AtomicBool>,
    removed_without_groups: bool,
    charge: bool,
    costs: Vec<(Vec<String>, Duration)>,
    fail_cleanup_stop: bool,
    foreign: Option<(&'static str, Value)>,
}
impl Fake {
    fn new(s: config::Settings, time: plan::Time, groups: Arc<AtomicBool>) -> Self {
        Self {
            settings: s,
            time,
            run: String::new(),
            present: [false; 2],
            running: [false; 2],
            inspect_generator: 0,
            terminal_after: 2,
            trace: Vec::new(),
            fault: None,
            cancellation: 0,
            late_replay: None,
            clean: true,
            late: None,
            request_error: None,
            metrics: 0,
            public_running: false,
            groups,
            removed_without_groups: false,
            charge: false,
            costs: Vec::new(),
            fail_cleanup_stop: false,
            foreign: None,
        }
    }
    fn response(&mut self, c: &Command, a: &[String]) -> Result<Vec<u8>> {
        let b = if a.first().is_some_and(|v| v == "--host") {
            &a[2..]
        } else {
            a
        };
        self.trace.push((c.cleanup, b.to_vec()));
        let op = b.first().map(String::as_str).unwrap_or("");
        if c.cleanup && op == "stop" && self.fail_cleanup_stop {
            return Err(failure(1));
        }
        if let Some((fail, status)) = self.fault
            && fail == op
        {
            self.fault = None;
            return Err(failure(status));
        }
        if c.program == Program::BenchmarkCurl {
            if op == "/metrics" {
                self.metrics += 1;
            }
            if self.late == Some("ready") && op == "/ready" {
                self.time.set(self.time.elapsed() + Duration::from_secs(60));
                self.late = None;
            }
            if self.late == Some("zero") && op == "/metrics" && self.metrics == 2 {
                self.time.set(self.time.elapsed() + Duration::from_secs(40));
                self.late = None;
            }
            if self.late == Some("measurement") && op == "/metrics" && self.metrics == 1 {
                self.time
                    .set(plan::ENTRY - plan::TAIL - Duration::from_secs(100));
                self.late = None;
            }
            if let Some((path, code)) = self.request_error
                && path == op
                && (path != "/metrics" || self.metrics == 2)
            {
                self.request_error = None;
                return Err(failure(code));
            }
            return Ok(if op == "/ready" {
                b"{\"status\":\"ready\"}".to_vec()
            } else {
                concat!(
                    "simplestchat_rooms_active 0\nsimplestchat_participants_active 0\n",
                    "simplestchat_connections_active 0\n",
                    "simplestchat_participants_snapshot_complete 1\n"
                )
                .as_bytes()
                .to_vec()
            });
        }
        match op {
            "context" => Ok(b"unix:///owned-inert.sock\n".to_vec()),
            "info" => {
                if b[2] == plan::DAEMON {
                    Ok(
                        serde_json::to_vec(&json!({"id":"owned-daemon","os":"linux","cgroup":"2",
                "system":"inertLinux","security":[]}))
                        .unwrap(),
                    )
                } else {
                    Ok(b"owned-daemon\n".to_vec())
                }
            }
            "ps" => Ok(if self.public_running {
                b"foreign\n".to_vec()
            } else {
                Vec::new()
            }),
            "image" => Ok(match b[3].as_str() {
                "{{.Id}}" => format!("{}\n", b[4]).into_bytes(),
                "{{.Config.User}}" => b"10001:10001\n".to_vec(),
                "{{json .Config.Env}}" => b"[]".to_vec(),
                "{{json .Config}}" => {
                    b"{\"Cmd\":[\"/app/simplestChat\"],\"Entrypoint\":null}".to_vec()
                }
                "{{json .Config.Entrypoint}}" => b"[\"/app/load_test\"]".to_vec(),
                _ => panic!("unexpected image"),
            }),
            "create" => {
                let role = if b[2].ends_with("-server") {
                    io::Role::Server
                } else {
                    io::Role::Generator
                };
                self.run = b[4]
                    .strip_prefix("simplestchat.benchmark.run=")
                    .unwrap()
                    .into();
                self.present[role.index()] = true;
                Ok(format!("{}\n", cid(role)).into_bytes())
            }
            "start" => {
                let i = if b[1] == cid(io::Role::Server) { 0 } else { 1 };
                self.running[i] = true;
                Ok(format!("{}\n", b[1]).into_bytes())
            }
            "inspect" => {
                let role = if b[3] == cid(io::Role::Server) {
                    io::Role::Server
                } else {
                    io::Role::Generator
                };
                if role == io::Role::Generator {
                    self.inspect_generator += 1;
                    if self.inspect_generator >= self.terminal_after {
                        self.running[1] = false;
                        fs::write(
                            self.settings.output.join("workload/load_test_summary.json"),
                            serde_json::to_vec(&summary(&self.settings, &self.run)).unwrap(),
                        )
                        .unwrap();
                    }
                }
                let network = if role == io::Role::Server {
                    "none".into()
                } else {
                    format!("container:{}", cid(io::Role::Server))
                };
                let image = if role == io::Role::Server {
                    &self.settings.server_image
                } else {
                    &self.settings.generator_image
                };
                let mut value = json!({"id":cid(role),
                    "name":format!("/{}",plan::name(&self.run,role)),
                    "image":image,"configuredImage":image,
                    "run":self.run,"role":role.name(),"network":network,"state":{
                        "Running":self.running[role.index()],"Restarting":false,"OOMKilled":false,
                        "ExitCode":0,"ErrorEmpty":true,"Pid":1001+role.index(),
                        "StartedAt":"2026-10-22T00:00:00Z"}});
                if role == io::Role::Generator
                    && let Some((key, selected)) = &self.foreign
                {
                    value[*key] = selected.clone();
                }
                Ok(serde_json::to_vec(&value).unwrap())
            }
            "wait" => {
                if self.late == Some("wait") {
                    self.time.set(self.time.elapsed() + self.settings.wait());
                    self.late = None;
                }
                Ok(b"0\n".to_vec())
            }
            "stats" => {
                let mut rows = Vec::new();
                for r in io::Role::ALL {
                    rows.extend_from_slice(
                        &serde_json::to_vec(&json!({"ID":&cid(r)[..12],
                        "Name":plan::name(&self.run,r),"CPUPerc":"1.00%","MemUsage":"1MiB /4GiB",
                        "MemPerc":"1.00%","NetIO":"0B /0B","BlockIO":"0B /0B","PIDs":"1",
                        "PRIVATE":"not retained"}))
                        .unwrap(),
                    );
                    rows.push(b'\n');
                }
                Ok(rows)
            }
            "stop" | "kill" => {
                let i = if b.last().unwrap() == &cid(io::Role::Server) {
                    0
                } else {
                    1
                };
                self.running[i] = false;
                Ok(Vec::new())
            }
            "logs" => Ok(format!("prefix {} suffix\n", "e".repeat(64)).into_bytes()),
            "rm" => {
                let i = if b[1] == cid(io::Role::Server) { 0 } else { 1 };
                if !self.groups.load(Ordering::SeqCst) && self.inspect_generator > 0 {
                    self.removed_without_groups = true;
                }
                self.present[i] = false;
                Ok(Vec::new())
            }
            "container" => {
                let filter = &b[5];
                let role = if filter.ends_with("server$")
                    || filter == &format!("id={}", cid(io::Role::Server))
                {
                    io::Role::Server
                } else {
                    io::Role::Generator
                };
                Ok(if self.present[role.index()] {
                    format!("{}\n", cid(role)).into_bytes()
                } else {
                    Vec::new()
                })
            }
            _ => panic!("unrecognized inert command {b:?}"),
        }
    }
}
impl Executor for Fake {
    async fn run(&mut self, mut c: Command) -> Result<Vec<u8>> {
        let args = c
            .arguments
            .iter()
            .map(|v| v.to_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        let result = self.response(&c, &args);
        if self.charge {
            let cost = c.timeout + c.grace + plan::SETTLEMENT;
            self.costs.push((args.clone(), cost));
            self.time.set(self.time.elapsed() + cost);
        }
        let written: Result<()> = match &result {
            Ok(bytes) => {
                if let Some(logs) = &c.separate_logs {
                    let mut w = logs.stdout.writer()?;
                    w.write_all(bytes).unwrap();
                    drop(w);
                } else if let Some(log) = &c.combined_log {
                    let mut w = log.writer()?;
                    w.write_all(bytes).unwrap();
                    drop(w);
                }
                Ok(())
            }
            Err(_) => Ok(()),
        };
        let receipt = c.benchmark_output.clone();
        let input = c
            .stdin
            .take()
            .map(|v| {
                Arc::try_unwrap(v)
                    .map_err(|_| failure(1))
                    .and_then(files::close)
            })
            .transpose();
        let combined = c.combined_log.take();
        let paired = c.separate_logs.take();
        let seal = |f: crate::ci::bounded_log::Finished| -> Result<Seal> {
            if !f.complete {
                return Err(failure(1));
            }
            Ok(Seal {
                sha256: f.sha256,
                bytes: f.bytes,
                metadata: f.metadata,
            })
        };
        let (combined, paired) = tokio::join!(
            async {
                match combined {
                    Some(l) => {
                        let l = Arc::try_unwrap(l).map_err(|_| failure(1))?;
                        l.finish().await.and_then(seal).map(Some)
                    }
                    None => Ok(None),
                }
            },
            async {
                match paired {
                    Some(l) => {
                        let l = Arc::try_unwrap(l).map_err(|_| failure(1))?;
                        let (a, b) = tokio::join!(l.stdout.finish(), l.stderr.finish());
                        let a = a.and_then(seal);
                        let b = b.and_then(seal);
                        Ok(Some(Captures {
                            stdout: a?,
                            stderr: b?,
                        }))
                    }
                    None => Ok(None),
                }
            }
        );
        let evidence = if input.is_ok() && combined.is_ok() && paired.is_ok() {
            Some(tools::IoSeals {
                combined: combined.as_ref().ok().cloned().flatten(),
                paired: paired.as_ref().ok().cloned().flatten(),
            })
        } else {
            None
        };
        let sealed = receipt.map(|r| r.observed_test(evidence)).transpose();
        let closed = process::settle_unforwarded(c).await;
        result?;
        written?;
        input?;
        combined?;
        paired?;
        sealed?;
        closed?;
        Ok(Vec::new())
    }
    async fn ready(&mut self) -> Result<()> {
        if self.cancellation == 0 {
            Ok(())
        } else {
            Err(failure(self.cancellation))
        }
    }
    fn cleanup_passed(&self) -> bool {
        self.clean
    }
    async fn replay(&mut self, _: &[u8], _: bool, _: Instant) -> Result<()> {
        if let Some(code) = self.late_replay {
            if code == 124 {
                self.time.set(plan::ENTRY);
                Err(failure(124))
            } else {
                self.cancellation = code;
                Err(failure(code))
            }
        } else {
            Ok(())
        }
    }
}
#[tokio::test(flavor = "current_thread")]
async fn complete_two_container_transaction_retires_all_owners_and_redacts_logs() {
    let temp = Temporary::new();
    let paths = temp.paths();
    let budget = io::Budget::default();
    let time = plan::Time::new(Instant::now());
    time.set(Duration::ZERO);
    let guard = files::Guard::acquire(paths.clone(), &time, &budget, false)
        .await
        .unwrap();
    let obs = Observed::new();
    let s = settings(&temp);
    let mut fake = Fake::new(s.clone(), time.clone(), obs.groups.clone());
    fake.terminal_after = 4;
    let result = controller::run(
        &s,
        &Environment::new(),
        &mut fake,
        &controller::Context {
            obs: &obs,
            guard: &guard,
            paths: &paths,
            budget: &budget,
            time: &time,
            token: &"e".repeat(64),
        },
    )
    .await;
    let journal = journal::Journal::decode(&guard.current().unwrap().unwrap()).unwrap();
    let report: Value =
        serde_json::from_slice(&fs::read(s.output.join("result.json")).unwrap()).unwrap();
    let log = fs::read(s.output.join("server.log")).unwrap();
    let closed = guard.finish();
    assert!(result.is_ok(), "{:?}", result.err());
    closed.unwrap();
    assert_eq!(budget.count(), 0);
    assert!(journal.finalized);
    assert_eq!(report["passed"], true);
    assert_eq!(fake.present, [false; 2]);
    assert!(!fake.removed_without_groups);
    assert!(obs.groups.load(Ordering::SeqCst));
    assert!(obs.leases.borrow().iter().all(|c| c.load(Ordering::SeqCst)));
    assert!(!log.windows(64).any(|v| v == "e".repeat(64).as_bytes()));
    assert!(
        fake.trace
            .iter()
            .all(|(_, a)| !a.iter().any(|v| v == "--force" || v == "--volumes"))
    );
}
#[tokio::test(flavor = "current_thread")]
async fn failures_preserve_primary_and_attempt_both_role_cleanup_without_public_mutation() {
    for (op, status) in [
        ("create", 143),
        ("start", 130),
        ("wait", 124),
        ("stop", 1),
        ("logs", 1),
        ("rm", 1),
    ] {
        let temp = Temporary::new();
        let paths = temp.paths();
        let budget = io::Budget::default();
        let time = plan::Time::new(Instant::now());
        time.set(Duration::ZERO);
        let guard = files::Guard::acquire(paths.clone(), &time, &budget, false)
            .await
            .unwrap();
        let obs = Observed::new();
        let s = settings(&temp);
        let mut fake = Fake::new(s.clone(), time.clone(), obs.groups.clone());
        fake.fault = Some((op, status));
        let result = controller::run(
            &s,
            &Environment::new(),
            &mut fake,
            &controller::Context {
                obs: &obs,
                guard: &guard,
                paths: &paths,
                budget: &budget,
                time: &time,
                token: &"e".repeat(64),
            },
        )
        .await;
        let closed = guard.finish();
        assert_eq!(result.unwrap_err().code, status, "{op}");
        closed.unwrap();
        assert_eq!(budget.count(), 0);
        assert!(
            fake.trace
                .iter()
                .filter(|(_, a)| matches!(a[0].as_str(), "kill" | "stop" | "rm"))
                .all(|(_, a)| a
                    .iter()
                    .any(|s| s == &cid(io::Role::Server) || s == &cid(io::Role::Generator)))
        );
    }
}
#[tokio::test(flavor = "current_thread")]
async fn saved_result_is_prepublication_and_late_replay_keeps_typed_status() {
    for status in [124, 130, 143] {
        let temp = Temporary::new();
        let paths = temp.paths();
        let budget = io::Budget::default();
        let time = plan::Time::new(Instant::now());
        time.set(Duration::ZERO);
        let guard = files::Guard::acquire(paths.clone(), &time, &budget, false)
            .await
            .unwrap();
        let obs = Observed::new();
        let s = settings(&temp);
        let mut fake = Fake::new(s.clone(), time.clone(), obs.groups.clone());
        fake.late_replay = Some(status);
        let result = controller::run(
            &s,
            &Environment::new(),
            &mut fake,
            &controller::Context {
                obs: &obs,
                guard: &guard,
                paths: &paths,
                budget: &budget,
                time: &time,
                token: &"e".repeat(64),
            },
        )
        .await;
        let report: Value =
            serde_json::from_slice(&fs::read(s.output.join("result.json")).unwrap()).unwrap();
        let closed = guard.finish();
        assert_eq!(result.unwrap_err().code, status);
        assert_eq!(report["passed"], true);
        closed.unwrap();
        assert_eq!(budget.count(), 0);
        assert_eq!(fake.present, [false; 2]);
    }
}
#[test]
fn closed_configuration_and_entry_profiles_refuse_bypasses_and_exact_recovery_deadline() {
    assert!(config::Config::parse(&["cleanup".into(), "--normal".into()]).is_err());
    let t = plan::Time::recovery(Instant::now());
    t.set(plan::TAIL);
    assert_eq!(t.fence(true).unwrap_err().code, 124);
    assert_eq!(
        t.latest(Duration::from_secs(6), Duration::from_secs(1), true)
            .unwrap_err()
            .code,
        124
    );
    assert_eq!(t.entry(), plan::TAIL);
    let invalid = plan::command(
        "unix:///owned.sock",
        &["rm", "--force", &cid(io::Role::Server)],
        true,
    );
    assert!(!plan::allowed(invalid.program, &invalid.arguments));
    let fake = Temporary::new();
    let s = settings(&fake);
    let c = s.create("unix:///owned.sock", &"a".repeat(32), io::Role::Server, "");
    assert!(plan::allowed(c.program, &c.arguments));
}
#[tokio::test(flavor = "current_thread")]
async fn root_guard_excludes_another_owner_and_refuses_unfinished_public_record() {
    let temp = Temporary::new();
    let paths = temp.paths();
    let budget = io::Budget::default();
    let time = plan::Time::new(Instant::now());
    time.set(Duration::ZERO);
    let first = files::Guard::acquire(paths.clone(), &time, &budget, false)
        .await
        .unwrap();
    let second = files::Guard::acquire(paths.clone(), &time, &budget, false).await;
    assert!(second.is_err());
    first.finish().unwrap();
    assert_eq!(budget.count(), 0);
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&paths.public)
        .unwrap();
    fs::write(
        paths.public.join("release-state.json"),
        b"{\"schemaVersion\":1,\"finalized\":false}",
    )
    .unwrap();
    fs::set_permissions(
        paths.public.join("release-state.json"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    assert!(
        files::Guard::acquire(paths.clone(), &time, &budget, false)
            .await
            .is_err()
    );
    assert_eq!(budget.count(), 0);
}
#[test]
fn redaction_handles_split_tokens_embedded_hex_runs_and_preserves_nontoken_text() {
    let token = "e".repeat(64);
    let mut chunks = Vec::new();
    chunks.extend_from_slice(b"hello ");
    chunks.extend_from_slice(&token.as_bytes()[..31]);
    chunks.extend_from_slice(&token.as_bytes()[31..]);
    chunks.extend_from_slice(b" world");
    let redacted = files::redact(&chunks, token.as_bytes());
    assert_eq!(redacted, b"hello [metrics-token-redacted] world");
    let recovery = files::redact(format!("prefix ff{token}aaa suffix").as_bytes(), b"");
    assert_eq!(recovery, b"prefix [hex-secret-redacted] suffix");
}

async fn unfinished(
    temp: &Temporary,
    paths: &files::Paths,
    budget: &io::Budget,
) -> (config::Settings, Vec<u8>, String) {
    let time = plan::Time::new(Instant::now());
    time.set(Duration::ZERO);
    let guard = files::Guard::acquire(paths.clone(), &time, budget, false)
        .await
        .unwrap();
    let obs = Observed::new();
    let s = settings(temp);
    let mut fake = Fake::new(s.clone(), time.clone(), obs.groups.clone());
    fake.fault = Some(("logs", 1));
    let result = controller::run(
        &s,
        &Environment::new(),
        &mut fake,
        &controller::Context {
            obs: &obs,
            guard: &guard,
            paths,
            budget,
            time: &time,
            token: &"e".repeat(64),
        },
    )
    .await;
    assert_eq!(result.unwrap_err().code, 1);
    let j = journal::Journal::decode(&guard.current().unwrap().unwrap()).unwrap();
    assert!(!j.finalized);
    let before = fs::read(s.output.join("result.json")).unwrap();
    guard.finish().unwrap();
    (s, before, j.run_id)
}
#[tokio::test(flavor = "current_thread")]
async fn locked_external_recovery_retains_original_result_and_finalizes_only_proven_retirement() {
    for (copy_failure, clean) in [(false, true), (true, true), (false, false)] {
        let temp = Temporary::new();
        let paths = temp.paths();
        let budget = io::Budget::default();
        let (s, before, run) = unfinished(&temp, &paths, &budget).await;
        if copy_failure {
            files::fail_next_copy(&s.output);
        }
        let time = plan::Time::recovery(Instant::now());
        time.set(Duration::ZERO);
        let guard = files::Guard::acquire(paths.clone(), &time, &budget, true)
            .await
            .unwrap();
        let obs = Observed::new();
        let mut fake = Fake::new(s.clone(), time.clone(), obs.groups.clone());
        fake.run = run;
        fake.clean = clean;
        let result = controller::recover(
            &mut fake,
            &controller::Context {
                obs: &obs,
                guard: &guard,
                paths: &paths,
                budget: &budget,
                time: &time,
                token: "",
            },
        )
        .await;
        let j = journal::Journal::decode(&guard.current().unwrap().unwrap()).unwrap();
        let after = fs::read(s.output.join("result.json")).unwrap();
        let closed = guard.finish();
        closed.unwrap();
        assert_eq!(budget.count(), 0);
        assert_eq!(result.is_ok(), !copy_failure && clean);
        assert_eq!(j.finalized, !copy_failure && clean);
        assert!(fake.trace.iter().all(|(cleanup, _)| *cleanup));
        assert!(fake.trace.iter().any(|(_, a)| a[0] == "container"));
        if copy_failure {
            assert_eq!(after, before);
        } else {
            let report: Value = serde_json::from_slice(&after).unwrap();
            assert_eq!(report["passed"], false);
            let retained = report["retainedResult"].as_str().unwrap();
            assert_eq!(fs::read(s.output.join(retained)).unwrap(), before);
        }
    }
}
#[tokio::test(flavor = "current_thread")]
async fn recovery_deadline_does_not_reset_after_lock_and_known_failure_precedes_late_signal() {
    let temp = Temporary::new();
    let paths = temp.paths();
    let budget = io::Budget::default();
    let time = plan::Time::new(Instant::now());
    time.set(Duration::ZERO);
    let guard = files::Guard::acquire(paths.clone(), &time, &budget, false)
        .await
        .unwrap();
    let late = plan::Time::recovery(Instant::now());
    late.set(plan::TAIL);
    let refused = files::Guard::acquire(paths.clone(), &late, &budget, true).await;
    assert_eq!(refused.err().unwrap().code, 124);
    assert_eq!(late.elapsed(), plan::TAIL);
    let obs = Observed::new();
    let s = settings(&temp);
    let mut fake = Fake::new(s.clone(), time.clone(), obs.groups.clone());
    fake.fault = Some(("wait", 1));
    fake.late_replay = Some(143);
    let result = controller::run(
        &s,
        &Environment::new(),
        &mut fake,
        &controller::Context {
            obs: &obs,
            guard: &guard,
            paths: &paths,
            budget: &budget,
            time: &time,
            token: &"e".repeat(64),
        },
    )
    .await;
    guard.finish().unwrap();
    assert_eq!(result.unwrap_err().code, 1);
    assert_eq!(fake.present, [false; 2]);
    assert_eq!(budget.count(), 0);
}
#[tokio::test(flavor = "current_thread")]
async fn foreign_output_entries_and_second_capture_constructor_failure_are_retained() {
    let temp = Temporary::new();
    let paths = temp.paths();
    let budget = io::Budget::default();
    let time = plan::Time::new(Instant::now());
    time.set(Duration::ZERO);
    let guard = files::Guard::acquire(paths.clone(), &time, &budget, false)
        .await
        .unwrap();
    let out = files::Output::new(&temp.0.join("evidence"), &paths, &budget, &guard.state).unwrap();
    out.bytes("result.json", b"original").unwrap();
    fs::rename(
        out.path.join("result.json"),
        out.path.join("original-result"),
    )
    .unwrap();
    fs::write(out.path.join("result.json"), b"foreign").unwrap();
    fs::set_permissions(
        out.path.join("result.json"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    assert!(out.bytes("result.json", b"replacement").is_err());
    assert_eq!(fs::read(out.path.join("result.json")).unwrap(), b"foreign");
    assert!(out.finish().is_err());
    files::replace_next_anonymous(&paths.state);
    let guarded = files::Token::new(&guard.state, &budget, &"e".repeat(64));
    assert!(guarded.is_err());
    let replaced = fs::read_dir(&paths.state)
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| {
            p.file_name()
                .unwrap()
                .to_str()
                .is_some_and(|name| name.starts_with("token-") && !name.ends_with(".original"))
        })
        .unwrap();
    assert_eq!(fs::read(replaced).unwrap(), b"foreign replacement");
    let baseline = budget.count();
    files::change_next_write_mode(&paths.state, rustix::fs::Mode::from_bits_truncate(0o4600));
    assert!(files::Token::new(&guard.state, &budget, &"e".repeat(64)).is_err());
    assert_eq!(budget.count(), baseline);
    let foreign = paths.state.join("capture-1-err");
    fs::write(&foreign, b"foreign sink").unwrap();
    fs::set_permissions(&foreign, fs::Permissions::from_mode(0o600)).unwrap();
    struct Never(usize);
    impl Executor for Never {
        async fn run(&mut self, c: Command) -> Result<Vec<u8>> {
            self.0 += 1;
            let _ = process::settle_unforwarded(c).await;
            Err(failure(1))
        }
        async fn ready(&mut self) -> Result<()> {
            Ok(())
        }
    }
    let baseline = budget.count();
    let mut never = Never(0);
    let mut capture = files::Capture {
        inner: &mut never,
        spool: &guard.state,
        budget: &budget,
        time: &time,
        counter: 0,
        settled: true,
    };
    let result = capture
        .run(plan::command(
            "unix:///inert.sock",
            &["info", "--format", "{{.ID}}"],
            false,
        ))
        .await;
    assert!(result.is_err());
    assert_eq!(never.0, 0);
    assert_eq!(budget.count(), baseline);
    assert_eq!(fs::read(&foreign).unwrap(), b"foreign sink");
    guard.finish().unwrap();
    assert_eq!(budget.count(), 0);
}
#[tokio::test(flavor = "current_thread")]
async fn original_namespace_guard_refuses_aliases_replacements_and_late_public_release_creation() {
    let temp = Temporary::new();
    let paths = temp.paths();
    let budget = io::Budget::default();
    let time = plan::Time::new(Instant::now());
    time.set(Duration::ZERO);
    let guard = files::Guard::acquire(paths.clone(), &time, &budget, false)
        .await
        .unwrap();
    assert!(
        files::Output::new(
            &paths.state.join("owned-output"),
            &paths,
            &budget,
            &guard.state
        )
        .is_err()
    );
    let alias = paths.state.with_file_name("STATE");
    if fs::metadata(&alias).is_ok() {
        assert!(
            files::Output::new(&alias.join("owned-output"), &paths, &budget, &guard.state).is_err()
        );
        assert!(!alias.join("owned-output").exists());
    }
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&paths.public)
        .unwrap();
    assert!(guard.check().is_err());
    assert!(guard.finish().is_err());
    assert_eq!(budget.count(), 0);
}
#[test]
fn exact_tail_admission_releases_each_settled_slot_once_and_never_resets_recovery_entry() {
    let t = plan::Time::recovery(Instant::now());
    t.set(Duration::from_secs(30));
    t.begin_retirement().unwrap();
    assert!(
        t.latest(Duration::from_secs(5), Duration::from_secs(1), true)
            .is_ok()
    );
    t.set(Duration::from_secs(101));
    assert_eq!(
        t.latest(Duration::from_secs(5), Duration::from_secs(1), true)
            .unwrap_err()
            .code,
        124
    );
    t.settled_slot(Duration::from_secs(5), Duration::from_secs(1));
    assert!(
        t.latest(Duration::from_secs(5), Duration::from_secs(1), true)
            .is_ok()
    );
    t.set(plan::TAIL);
    assert_eq!(t.fence(true).unwrap_err().code, 124);
}

#[tokio::test(flavor = "current_thread")]
async fn late_positive_readiness_wait_and_zero_counts_refuse_then_retire_every_attempted_role() {
    for late in ["ready", "wait", "zero"] {
        let temp = Temporary::new();
        let paths = temp.paths();
        let budget = io::Budget::default();
        let time = plan::Time::new(Instant::now());
        time.set(Duration::ZERO);
        let guard = files::Guard::acquire(paths.clone(), &time, &budget, false)
            .await
            .unwrap();
        let obs = Observed::new();
        let s = settings(&temp);
        let mut fake = Fake::new(s.clone(), time.clone(), obs.groups.clone());
        fake.late = Some(late);
        fake.late_replay = Some(143);
        let result = controller::run(
            &s,
            &Environment::new(),
            &mut fake,
            &controller::Context {
                obs: &obs,
                guard: &guard,
                paths: &paths,
                budget: &budget,
                time: &time,
                token: &"e".repeat(64),
            },
        )
        .await;
        let report: Value =
            serde_json::from_slice(&fs::read(s.output.join("result.json")).unwrap()).unwrap();
        guard.finish().unwrap();
        assert_eq!(result.unwrap_err().code, 124, "{late}");
        assert_eq!(report["workloadPassed"], false);
        assert_eq!(fake.present, [false; 2]);
        assert_eq!(budget.count(), 0);
        assert!(obs.leases.borrow().iter().all(|c| c.load(Ordering::SeqCst)));
    }
}
#[test]
fn full_resource_binding_refuses_foreign_name_config_network_identity_and_private_state_fields() {
    let temp = Temporary::new();
    let s = settings(&temp);
    let run = "a".repeat(32);
    let j = journal::Journal::new(
        &s,
        run.clone(),
        "unix:///owned.sock".into(),
        s.output.clone(),
        "f".repeat(64),
    );
    let id = cid(io::Role::Server);
    let v = json!({"id":id,"name":format!("/{}",plan::name(&run,io::Role::Server)),
        "image":s.server_image,"configuredImage":s.server_image,
        "run":run,"role":"server","network":"none",
        "state":{"Running":false,"Restarting":false,"OOMKilled":false,"ExitCode":0,
            "ErrorEmpty":true,"Pid":0,"StartedAt":"2026-10-22T00:00:00Z"}});
    assert!(
        cleanup::inspect(
            &serde_json::to_vec(&v).unwrap(),
            &j,
            io::Role::Server,
            &id,
            None
        )
        .is_ok()
    );
    for (key, value) in [
        ("name", json!("/foreign")),
        ("image", json!("sha256:foreign")),
        ("configuredImage", json!("different")),
        ("run", json!("foreign")),
        ("role", json!("generator")),
        ("network", json!("host")),
        ("id", json!("foreign")),
        ("privateError", json!("DO NOT RETAIN")),
    ] {
        let mut bad = v.clone();
        bad[key] = value;
        assert!(
            cleanup::inspect(
                &serde_json::to_vec(&bad).unwrap(),
                &j,
                io::Role::Server,
                &id,
                None
            )
            .is_err(),
            "{key}"
        );
    }
    let mut bad = v;
    bad["state"]["ExitCode"] = json!(0.0);
    assert!(
        cleanup::inspect(
            &serde_json::to_vec(&bad).unwrap(),
            &j,
            io::Role::Server,
            &id,
            None
        )
        .is_err()
    );
}
#[tokio::test(flavor = "current_thread")]
async fn existing_fifo_inputs_are_refused_before_open_and_current_absence_cannot_be_readopted() {
    let temp = Temporary::new();
    let paths = temp.paths();
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&paths.state)
        .unwrap();
    nix::unistd::mkfifo(
        &paths.state.join("workload.lock"),
        nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
    )
    .unwrap();
    let budget = io::Budget::default();
    let time = plan::Time::new(Instant::now());
    time.set(Duration::ZERO);
    let opened = files::opened();
    assert!(
        files::Guard::acquire(paths.clone(), &time, &budget, false)
            .await
            .is_err()
    );
    assert_eq!(files::opened(), opened);
    assert_eq!(budget.count(), 0);
    fs::remove_file(paths.state.join("workload.lock")).unwrap();
    let guard = files::Guard::acquire(paths.clone(), &time, &budget, false)
        .await
        .unwrap();
    nix::unistd::mkfifo(
        &paths.state.join("current.json"),
        nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
    )
    .unwrap();
    let opened = files::opened();
    assert!(guard.current().is_err());
    assert!(files::read_test(&guard.state, "current.json").is_err());
    assert_eq!(files::opened(), opened);
    fs::remove_file(paths.state.join("current.json")).unwrap();
    assert!(guard.current().unwrap().is_none());
    let foreign = paths.state.join("current.json");
    fs::write(&foreign, b"foreign finalized record").unwrap();
    fs::set_permissions(&foreign, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(guard.current().is_err());
    assert!(guard.save(b"replacement").is_err());
    assert!(guard.finish().is_err());
    assert_eq!(fs::read(foreign).unwrap(), b"foreign finalized record");
    assert_eq!(budget.count(), 0);
}
#[tokio::test(flavor = "current_thread")]
async fn original_writer_mode_is_checked_before_publication_without_adopting_the_new_stamp() {
    let temp = Temporary::new();
    let paths = temp.paths();
    let budget = io::Budget::default();
    let time = plan::Time::new(Instant::now());
    time.set(Duration::ZERO);
    let guard = files::Guard::acquire(paths.clone(), &time, &budget, false)
        .await
        .unwrap();
    assert!(guard.current().unwrap().is_none());
    files::change_next_write_mode(&paths.state, rustix::fs::Mode::from_bits_truncate(0o4600));
    assert!(guard.save(b"original bytes").is_err());
    assert!(!paths.state.join("current.json").exists());
    let partial = paths.state.join("current.json.part");
    assert_eq!(fs::read(&partial).unwrap(), b"original bytes");
    assert_eq!(
        fs::metadata(partial).unwrap().permissions().mode() & 0o7777,
        0o4600
    );
    guard.finish().unwrap();
    assert_eq!(budget.count(), 0);
}
#[tokio::test(flavor = "current_thread")]
async fn observed_request_cancellation_precedes_a_later_phase_deadline_and_still_retires() {
    for (phase, path) in [("ready", "/ready"), ("zero", "/metrics")] {
        for code in [130, 143] {
            let temp = Temporary::new();
            let paths = temp.paths();
            let budget = io::Budget::default();
            let time = plan::Time::new(Instant::now());
            time.set(Duration::ZERO);
            let guard = files::Guard::acquire(paths.clone(), &time, &budget, false)
                .await
                .unwrap();
            let obs = Observed::new();
            let s = settings(&temp);
            let mut fake = Fake::new(s.clone(), time.clone(), obs.groups.clone());
            fake.late = Some(phase);
            fake.request_error = Some((path, code));
            let result = controller::run(
                &s,
                &Environment::new(),
                &mut fake,
                &controller::Context {
                    obs: &obs,
                    guard: &guard,
                    paths: &paths,
                    budget: &budget,
                    time: &time,
                    token: &"e".repeat(64),
                },
            )
            .await;
            guard.finish().unwrap();
            assert_eq!(result.unwrap_err().code, code, "{phase}");
            assert_eq!(fake.present, [false; 2]);
            assert_eq!(budget.count(), 0);
            assert!(obs.leases.borrow().iter().all(|c| c.load(Ordering::SeqCst)));
        }
    }
}
#[tokio::test(flavor = "current_thread")]
async fn known_measurement_window_is_admitted_before_any_generator_intent_or_dispatch() {
    let temp = Temporary::new();
    let paths = temp.paths();
    let budget = io::Budget::default();
    let time = plan::Time::new(Instant::now());
    time.set(Duration::ZERO);
    let guard = files::Guard::acquire(paths.clone(), &time, &budget, false)
        .await
        .unwrap();
    let obs = Observed::new();
    let s = settings(&temp);
    let mut fake = Fake::new(s.clone(), time.clone(), obs.groups.clone());
    fake.late = Some("measurement");
    let result = controller::run(
        &s,
        &Environment::new(),
        &mut fake,
        &controller::Context {
            obs: &obs,
            guard: &guard,
            paths: &paths,
            budget: &budget,
            time: &time,
            token: &"e".repeat(64),
        },
    )
    .await;
    guard.finish().unwrap();
    assert_eq!(result.unwrap_err().code, 124);
    assert_eq!(
        fake.trace.iter().filter(|(_, a)| a[0] == "create").count(),
        1
    );
    assert!(!s.output.join("generator.create-attempted").exists());
    assert_eq!(fake.present, [false; 2]);
    assert_eq!(budget.count(), 0);
    time.set(plan::ENTRY - plan::TAIL - Duration::from_secs(124));
    assert!(time.measurement(Duration::from_secs(14)).is_err());
    time.set(plan::ENTRY - plan::TAIL - Duration::from_secs(125));
    assert!(time.measurement(Duration::from_secs(14)).is_ok());
}
async fn running_intents(
    temp: &Temporary,
    paths: &files::Paths,
    budget: &io::Budget,
    lost_generator_response: bool,
) -> (config::Settings, String) {
    use sha2::{Digest, Sha256};
    let time = plan::Time::new(Instant::now());
    time.set(Duration::ZERO);
    let guard = files::Guard::acquire(paths.clone(), &time, budget, false)
        .await
        .unwrap();
    let s = settings(temp);
    let output = files::Output::new(&s.output, paths, budget, &guard.state).unwrap();
    let run = "a".repeat(32);
    let journal = journal::Journal::new(
        &s,
        run.clone(),
        "unix:///owned-inert.sock".into(),
        s.output.clone(),
        files::hex(&Sha256::digest(b"owned-daemon")),
    );
    journal.begin(&guard, &output).unwrap();
    for role in io::Role::ALL {
        output.mark(role).unwrap();
        if role != io::Role::Generator || !lost_generator_response {
            output
                .bytes(
                    &format!("{}.cid", role.name()),
                    format!("{}\n", cid(role)).as_bytes(),
                )
                .unwrap();
        }
    }
    output.finish().unwrap();
    guard.finish().unwrap();
    (s, run)
}
#[tokio::test(flavor = "current_thread")]
async fn full_running_recovery_binds_every_worst_case_cli_slot_and_exact_kill_fallback() {
    let temp = Temporary::new();
    let paths = temp.paths();
    let budget = io::Budget::default();
    let (s, run) = running_intents(&temp, &paths, &budget, false).await;
    let time = plan::Time::recovery(Instant::now());
    time.set(Duration::ZERO);
    let guard = files::Guard::acquire(paths.clone(), &time, &budget, true)
        .await
        .unwrap();
    let obs = Observed::new();
    let mut fake = Fake::new(s, time.clone(), obs.groups.clone());
    fake.run = run;
    fake.present = [true; 2];
    fake.running = [true; 2];
    fake.terminal_after = usize::MAX;
    fake.charge = true;
    fake.fail_cleanup_stop = true;
    let result = controller::recover(
        &mut fake,
        &controller::Context {
            obs: &obs,
            guard: &guard,
            paths: &paths,
            budget: &budget,
            time: &time,
            token: "",
        },
    )
    .await;
    let journal = journal::Journal::decode(&guard.current().unwrap().unwrap()).unwrap();
    guard.finish().unwrap();
    assert_eq!(result.unwrap_err().code, 1);
    assert!(!journal.finalized);
    assert_eq!(fake.present, [false; 2]);
    assert_eq!(time.elapsed(), Duration::from_secs(1115));
    assert_eq!(fake.costs.len(), 41);
    assert_eq!(fake.costs[0].1, Duration::from_secs(26));
    assert_eq!(
        fake.costs[1..]
            .iter()
            .map(|(_, cost)| *cost)
            .sum::<Duration>(),
        Duration::from_secs(1089)
    );
    assert_eq!(
        fake.trace.iter().filter(|(_, a)| a[0] == "info").count(),
        21
    );
    assert_eq!(
        fake.trace.iter().filter(|(_, a)| a[0] != "info").count(),
        20
    );
    for (role, grace, body) in [(io::Role::Generator, "5", 29), (io::Role::Server, "30", 54)] {
        let id = cid(role);
        assert!(fake.costs.iter().any(|(a, cost)|
            a[2..] == ["stop", "--time", grace, &id] && cost.as_secs() == body));
        assert!(
            fake.trace
                .iter()
                .any(|(_, a)| a == &["kill", "--signal", "KILL", &id])
        );
        assert!(fake.trace.iter().any(|(_, a)| a == &["rm", &id]));
    }
    assert!(
        fake.trace
            .iter()
            .all(|(cleanup, a)| *cleanup && !a.iter().any(|s| s == "--force" || s == "--volumes"))
    );
    assert_eq!(budget.count(), 0);
}
#[tokio::test(flavor = "current_thread")]
async fn recovery_reconciles_owned_lost_create_but_never_mutates_foreign_generator_identity() {
    for foreign in [
        None,
        Some(("run", json!("foreign"))),
        Some(("image", json!(format!("sha256:{}", "f".repeat(64))))),
        Some(("name", json!("/foreign"))),
    ] {
        let temp = Temporary::new();
        let paths = temp.paths();
        let budget = io::Budget::default();
        let (s, run) = running_intents(&temp, &paths, &budget, true).await;
        let time = plan::Time::recovery(Instant::now());
        time.set(Duration::ZERO);
        let guard = files::Guard::acquire(paths.clone(), &time, &budget, true)
            .await
            .unwrap();
        let obs = Observed::new();
        let mut fake = Fake::new(s.clone(), time.clone(), obs.groups.clone());
        fake.run = run;
        fake.present = [true; 2];
        fake.running = [true; 2];
        fake.terminal_after = usize::MAX;
        fake.foreign = foreign.clone();
        let result = controller::recover(
            &mut fake,
            &controller::Context {
                obs: &obs,
                guard: &guard,
                paths: &paths,
                budget: &budget,
                time: &time,
                token: "",
            },
        )
        .await;
        let journal = journal::Journal::decode(&guard.current().unwrap().unwrap()).unwrap();
        guard.finish().unwrap();
        assert_eq!(result.is_ok(), foreign.is_none());
        assert_eq!(journal.finalized, foreign.is_none());
        assert_eq!(fake.present, [false, foreign.is_some()]);
        let mutated_generator = fake.trace.iter().any(|(_, a)| {
            matches!(a[0].as_str(), "stop" | "kill" | "rm")
                && a.last() == Some(&cid(io::Role::Generator))
        });
        assert_eq!(mutated_generator, foreign.is_none());
        assert_eq!(budget.count(), 0);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn malformed_role_records_stay_fatal_while_independent_roles_still_retire() {
    for (bad, later_fault) in [
        (io::Role::Generator, None),
        (io::Role::Generator, Some(("stop", 42))),
        (io::Role::Server, None),
    ] {
        let temp = Temporary::new();
        let paths = temp.paths();
        let budget = io::Budget::default();
        let (s, run) = running_intents(&temp, &paths, &budget, false).await;
        let malformed = s.output.join(format!("{}.cid", bad.name()));
        fs::write(&malformed, b"invalid\n").unwrap();
        let time = plan::Time::recovery(Instant::now());
        time.set(Duration::ZERO);
        let guard = files::Guard::acquire(paths.clone(), &time, &budget, true)
            .await
            .unwrap();
        let obs = Observed::new();
        let mut fake = Fake::new(s.clone(), time.clone(), obs.groups.clone());
        fake.run = run;
        fake.present = [true; 2];
        fake.running = [true; 2];
        fake.terminal_after = usize::MAX;
        fake.fault = later_fault;
        let result = controller::recover(
            &mut fake,
            &controller::Context {
                obs: &obs,
                guard: &guard,
                paths: &paths,
                budget: &budget,
                time: &time,
                token: "",
            },
        )
        .await;
        let journal = journal::Journal::decode(&guard.current().unwrap().unwrap()).unwrap();
        let cleanup: Value =
            serde_json::from_slice(&fs::read(s.output.join("cleanup.json")).unwrap()).unwrap();
        let report: Value =
            serde_json::from_slice(&fs::read(s.output.join("result.json")).unwrap()).unwrap();
        guard.finish().unwrap();
        assert_eq!(result.unwrap_err().code, 1);
        assert!(!journal.finalized);
        assert_eq!(cleanup["passed"], false);
        assert_eq!(report["passed"], false);
        assert_eq!(fs::read(malformed).unwrap(), b"invalid\n");
        let mutations = fake
            .trace
            .iter()
            .filter(|(_, a)| matches!(a[0].as_str(), "stop" | "kill" | "rm"))
            .map(|(_, a)| a)
            .collect::<Vec<_>>();
        if bad == io::Role::Generator {
            assert_eq!(fake.present, [false, true]);
            assert!(
                mutations
                    .iter()
                    .all(|a| a.last() == Some(&cid(io::Role::Server)))
            );
            assert!(mutations.iter().any(|a| a[0] == "stop"));
            assert!(mutations.iter().any(|a| a[0] == "rm"));
            assert_eq!(
                mutations.iter().any(|a| a[0] == "kill"),
                later_fault.is_some()
            );
        } else {
            assert_eq!(fake.present, [true; 2]);
            assert!(mutations.is_empty());
        }
        assert!(fake.trace.iter().all(|(cleanup, a)| {
            *cleanup && !a.iter().any(|s| s == "--force" || s == "--volumes")
        }));
        assert_eq!(budget.count(), 0);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn namespace_finalization_failure_is_fatal_instead_of_being_retried_as_unavailable() {
    for failed_lease in [1, 3] {
        let temp = Temporary::new();
        let paths = temp.paths();
        let budget = io::Budget::default();
        let time = plan::Time::new(Instant::now());
        time.set(Duration::ZERO);
        let guard = files::Guard::acquire(paths.clone(), &time, &budget, false)
            .await
            .unwrap();
        let mut obs = Observed::new();
        obs.fail_lease = failed_lease;
        let s = settings(&temp);
        let mut fake = Fake::new(s.clone(), time.clone(), obs.groups.clone());
        let result = controller::run(
            &s,
            &Environment::new(),
            &mut fake,
            &controller::Context {
                obs: &obs,
                guard: &guard,
                paths: &paths,
                budget: &budget,
                time: &time,
                token: &"e".repeat(64),
            },
        )
        .await;
        let report: Value =
            serde_json::from_slice(&fs::read(s.output.join("result.json")).unwrap()).unwrap();
        guard.finish().unwrap();
        assert_eq!(result.unwrap_err().code, 1);
        assert_eq!(obs.leases.borrow().len(), failed_lease);
        assert!(obs.leases.borrow().iter().all(|v| v.load(Ordering::SeqCst)));
        assert_eq!(report["workloadPassed"], false);
        assert_eq!(fake.present, [false; 2]);
        assert_eq!(budget.count(), 0);
    }
}
