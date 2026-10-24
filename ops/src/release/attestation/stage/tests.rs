//! Whole private host staging, atomic records and same-owner lifecycle checks.
use super::*;
use crate::test_support::Fixture;
use std::{
    fs,
    io::Write,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
};

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
}
fn write(path: &std::path::Path, bytes: &[u8], mode: u32) -> io::Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    let result = file
        .write_all(bytes)
        .and_then(|()| file.flush())
        .and_then(|()| file.set_permissions(fs::Permissions::from_mode(mode)));
    let closed = nix::unistd::close(file).map_err(io::Error::from);
    result?;
    closed
}
struct Data {
    _fixture: Fixture,
    access: Access,
    image: serde_json::Value,
    migrations: Vec<u8>,
}
impl Data {
    fn new() -> Self {
        let fixture = Fixture::new();
        let cached = super::super::verification_fixture::Fixture::new();
        let public = fixture.0.join("public");
        let config = fixture.0.join("config");
        let work = fixture.0.join("work");
        let revision = "a".repeat(40);
        for path in [
            &public,
            &config,
            &work,
            &public.join("results"),
            &public.join("releases"),
            &public.join("releases").join(&revision),
        ] {
            fs::DirBuilder::new().mode(0o700).create(path).unwrap();
        }
        for name in ["image.tar", "release.json"] {
            let bytes =
                super::super::verification_fixture::read(&cached.candidate.join(name)).unwrap();
            write(
                &public.join("releases").join(&revision).join(name),
                &bytes,
                0o600,
            )
            .unwrap();
        }
        let manifest: serde_json::Value = serde_json::from_slice(
            &super::super::verification_fixture::read(&cached.candidate.join("release.json"))
                .unwrap(),
        )
        .unwrap();
        let outcome: serde_json::Value = serde_json::from_slice(
            &super::super::verification_fixture::read(&cached.candidate.join("outcome.json"))
                .unwrap(),
        )
        .unwrap();
        let image = json!({"id":outcome["exportedImageId"],"os":"linux",
            "architecture":"amd64","user":"10001:10001","labels":{
            "org.opencontainers.image.revision":revision},"cmd":["/app/simplestChat"],
            "entrypoint":null,"workingDir":"/app","hasHealthcheck":false,"envNames":[]});
        let migrations = format!(
            "{}  /app/migrations/001_initial.sql\n",
            manifest["migrations"]["1"].as_str().unwrap()
        )
        .into_bytes();
        let docker = fixture.0.join("docker");
        write(&docker, b"#!/bin/sh\nexit 99\n", 0o700).unwrap();
        drop(cached);
        let access = Access {
            root: public,
            config,
            work,
            docker,
            socket: None,
            uid: rustix::process::getuid().as_raw(),
            free_bytes: Some(u64::MAX),
            fault: None,
        };
        Self {
            _fixture: fixture,
            access,
            image,
            migrations,
        }
    }
    fn options(&self) -> Options {
        Options {
            revision: "a".repeat(40),
            whole: Duration::from_secs(90),
            reserve: Duration::from_secs(10),
        }
    }
}
struct Daemon {
    image: serde_json::Value,
    logs: Vec<u8>,
    roles: Vec<(Role, bool)>,
    fail: Option<Role>,
    unknown: bool,
    expire_on: Option<Role>,
    cleanup_windows: Vec<(Instant, Instant, Instant)>,
    create_stdout: Option<Vec<u8>>,
    wait_stdout: Option<Vec<u8>>,
}
impl Daemon {
    fn new(data: &Data) -> Self {
        Self {
            image: data.image.clone(),
            logs: data.migrations.clone(),
            roles: Vec::new(),
            fail: None,
            unknown: false,
            expire_on: None,
            cleanup_windows: Vec::new(),
            create_stdout: None,
            wait_stdout: None,
        }
    }
}
impl Executor for Daemon {
    async fn run(&mut self, owner: &mut Owner, request: Invocation) -> CommandOutput {
        self.roles.push((request.role, request.cleanup));
        if request.cleanup {
            self.cleanup_windows
                .push((owner.now(), request.body_end, request.settlement_end));
        }
        if self.expire_on == Some(request.role) {
            self.expire_on = None;
            super::super::files::expire_hash_at(owner.body_end() - Duration::from_secs(40));
        }
        let failed = self.fail == Some(request.role) && !request.cleanup;
        if failed {
            self.fail = None;
        }
        let valid = request.arguments.first().is_some_and(|v| v == "--host")
            && request
                .arguments
                .get(1)
                .is_some_and(|v| v == "unix:///var/run/docker.sock")
            && request.body_end > owner.now()
            && request.body_end < request.settlement_end;
        let stdout = match request.role {
            Role::Inspect => serde_json::to_vec(&self.image).unwrap(),
            Role::Create if self.unknown => b"uncertain\n".to_vec(),
            Role::Create => self
                .create_stdout
                .clone()
                .unwrap_or_else(|| format!("{}\n", "b".repeat(64)).into_bytes()),
            Role::Wait => self.wait_stdout.clone().unwrap_or_else(|| b"0\n".to_vec()),
            Role::Logs => self.logs.clone(),
            _ => Vec::new(),
        };
        CommandOutput {
            stdout,
            stderr: Vec::new(),
            status: Some(if failed { 7 } else { 0 }),
            error: (!valid).then_some(Cause::Input),
            settled: valid,
            stdout_complete: true,
        }
    }
}
struct Observed {
    outcome: Outcome,
    cause: Option<Cause>,
    shared: (usize, usize),
    atomic: (usize, usize),
    lock_released: bool,
}
fn operation(data: &Data, daemon: &mut Daemon) -> Observed {
    let started = Instant::now();
    let options = data.options();
    let mut owner = Owner::new(started, options.whole, options.reserve).unwrap();
    super::super::files::start_probe(None);
    files::start_probe();
    let runtime = runtime();
    let outcome = runtime.block_on(execute(&options, &data.access, &mut owner, daemon, started));
    drop(runtime);
    let cause = owner.finish().err();
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(data.access.work.join("workload.lock"));
    let lock_released = match file {
        Ok(file) => {
            let acquired =
                rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive)
                    .is_ok();
            let closed = nix::unistd::close(file).is_ok();
            acquired && closed
        }
        Err(_) => false,
    };
    Observed {
        outcome,
        cause,
        shared: super::super::files::take_probe(),
        atomic: files::take_probe(),
        lock_released,
    }
}
#[test]
fn first_and_repeat_are_complete_and_keep_the_original_stage_record() {
    let data = Data::new();
    let mut daemon = Daemon::new(&data);
    let first = operation(&data, &mut daemon);
    let path = data
        .access
        .root
        .join("releases")
        .join("a".repeat(40))
        .join("staged.json");
    let before = super::super::verification_fixture::read(&path);
    daemon.roles.clear();
    let repeat = operation(&data, &mut daemon);
    let after = super::super::verification_fixture::read(&path);
    let roles = daemon.roles;
    drop(data);
    for observed in [first, repeat] {
        assert!(observed.outcome.passed);
        assert_eq!(observed.cause, None);
        assert!(observed.lock_released);
        assert!(observed.shared.0 > 0 && observed.atomic.0 > 0);
        assert_eq!((observed.shared.1, observed.atomic.1), (0, 0));
    }
    assert_eq!(before.unwrap(), after.unwrap());
    assert_eq!(roles, [(Role::Inspect, false)]);
}
#[test]
fn known_validator_cleanup_is_distinct_from_an_unknown_create_effect() {
    let data = Data::new();
    let mut daemon = Daemon::new(&data);
    daemon.fail = Some(Role::Start);
    let known = operation(&data, &mut daemon);
    let known_roles = daemon.roles;
    drop(data);
    let data = Data::new();
    let mut daemon = Daemon::new(&data);
    daemon.unknown = true;
    let unknown = operation(&data, &mut daemon);
    let unknown_roles = daemon.roles;
    drop(data);
    assert!(!known.outcome.passed && !unknown.outcome.passed);
    assert_eq!(known.cause, Some(Cause::Command));
    assert!(known_roles.contains(&(Role::Remove, true)));
    assert!(!unknown_roles.iter().any(|(role, _)| *role == Role::Remove));
    for observed in [known, unknown] {
        assert!(observed.lock_released);
        assert_eq!((observed.shared.1, observed.atomic.1), (0, 0));
    }
}
#[test]
fn every_atomic_fault_keeps_failure_and_closes_all_known_originals() {
    use files::Fault::*;
    let mut results = Vec::new();
    for fault in [
        ImportJournalWrite,
        ValidateJournalWrite,
        ValidatedJournalWrite,
        FinalJournalWrite,
        ValidationNameWrite,
        StagedWrite,
        OutcomeWrite,
        StagedFileSync,
        StagedDirectorySync,
        OutcomeFileSync,
        OutcomeDirectorySync,
    ] {
        let mut data = Data::new();
        data.access.fault = Some(fault);
        let mut daemon = Daemon::new(&data);
        let observed = operation(&data, &mut daemon);
        let remaining = fs::read_dir(data.access.root.join("results"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        drop(data);
        results.push((fault, observed, remaining.len()));
    }
    for (fault, observed, attempts) in results {
        assert!(!observed.outcome.passed, "{fault:?}");
        assert_eq!(observed.cause, Some(Cause::Io), "{fault:?}");
        assert!(observed.lock_released, "{fault:?}");
        assert_eq!((observed.shared.1, observed.atomic.1), (0, 0), "{fault:?}");
        assert_eq!(attempts, 1);
    }
}
#[test]
fn phase_deadline_is_not_restarted_for_an_expired_body() {
    let data = Data::new();
    let mut daemon = Daemon::new(&data);
    let options = data.options();
    let started = Instant::now();
    let mut owner = Owner::new(started, options.whole, options.reserve).unwrap();
    owner.expire_body();
    super::super::files::start_probe(None);
    files::start_probe();
    let runtime = runtime();
    let outcome = runtime.block_on(execute(
        &options,
        &data.access,
        &mut owner,
        &mut daemon,
        started,
    ));
    drop(runtime);
    let finished = owner.finish();
    let shared = super::super::files::take_probe();
    let atomic = files::take_probe();
    drop(data);
    assert!(!outcome.passed);
    assert_eq!(finished, Err(Cause::Deadline));
    assert!(daemon.roles.is_empty());
    assert_eq!((shared.1, atomic.1), (0, 0));
}

#[test]
fn too_small_body_refuses_before_any_daemon_effect_or_validator_creation() {
    let data = Data::new();
    let mut daemon = Daemon::new(&data);
    let options = Options {
        revision: "a".repeat(40),
        whole: Duration::from_secs(50),
        reserve: Duration::from_secs(10),
    };
    let started = Instant::now();
    let mut owner = Owner::new(started, options.whole, options.reserve).unwrap();
    super::super::files::start_probe(None);
    files::start_probe();
    let runtime = runtime();
    let outcome = runtime.block_on(execute(
        &options,
        &data.access,
        &mut owner,
        &mut daemon,
        started,
    ));
    drop(runtime);
    let finished = owner.finish();
    let shared = super::super::files::take_probe();
    let atomic = files::take_probe();
    drop(data);
    assert!(!outcome.passed);
    assert_eq!(finished, Err(Cause::Deadline));
    assert!(daemon.roles.is_empty());
    assert_eq!((shared.1, atomic.1), (0, 0));
}

#[test]
fn exact_settlement_boundary_does_not_dispatch_a_validator() {
    let data = Data::new();
    let mut daemon = Daemon::new(&data);
    let options = Options {
        revision: "a".repeat(40),
        whole: Duration::from_secs(60),
        reserve: Duration::from_secs(10),
    };
    let started = Instant::now();
    let mut owner = Owner::new(started, options.whole, options.reserve).unwrap();
    super::super::files::start_probe(None);
    files::start_probe();
    let runtime = runtime();
    let outcome = runtime.block_on(execute(
        &options,
        &data.access,
        &mut owner,
        &mut daemon,
        started,
    ));
    drop(runtime);
    let finished = owner.finish();
    let shared = super::super::files::take_probe();
    let atomic = files::take_probe();
    let roles = daemon.roles;
    drop(data);
    assert!(!outcome.passed);
    assert_eq!(finished, Err(Cause::Deadline));
    // B50 minus validator40 minus child settlement10 leaves no positive
    // create body. The prior import/inspection do not create a validator.
    assert_eq!(roles, [(Role::Load, false), (Role::Inspect, false)]);
    assert_eq!((shared.1, atomic.1), (0, 0));
}

#[test]
fn late_actual_hash_keeps_validator_retirement_inside_body_and_preserves_final_reserve() {
    let data = Data::new();
    let mut daemon = Daemon::new(&data);
    daemon.expire_on = Some(Role::Start);
    let options = data.options();
    let started = Instant::now();
    let mut owner = Owner::new(started, options.whole, options.reserve).unwrap();
    let body = owner.body_end();
    let original_end = owner.end();
    super::super::files::start_probe(None);
    files::start_probe();
    let runtime = runtime();
    let outcome = runtime.block_on(execute(
        &options,
        &data.access,
        &mut owner,
        &mut daemon,
        started,
    ));
    drop(runtime);
    let observed_now = owner.now();
    let finished = owner.finish();
    let shared = super::super::files::take_probe();
    let atomic = files::take_probe();
    drop(data);
    assert!(!outcome.passed);
    assert_eq!(finished, Err(Cause::Deadline));
    assert_eq!(observed_now, body - Duration::from_secs(40));
    assert_eq!(original_end.duration_since(body), options.reserve);
    assert!(daemon.roles.contains(&(Role::Start, false)));
    assert!(!daemon.roles.iter().any(|(role, _)| *role == Role::Wait));
    assert_eq!(daemon.cleanup_windows.len(), 1);
    let (entry, child_body, settlement) = daemon.cleanup_windows[0];
    assert!(entry < child_body && child_body < settlement && settlement <= body);
    assert_eq!((shared.1, atomic.1), (0, 0));
}

struct CloseCreate {
    daemon: Daemon,
    cleanup_failure: bool,
    actual_create: Option<(bool, bool, bool, bool, Option<i32>)>,
}
impl Executor for CloseCreate {
    async fn run(&mut self, owner: &mut Owner, request: Invocation) -> CommandOutput {
        if request.role == Role::Create {
            self.daemon.roles.push((Role::Create, false));
            let completed = process::run_stderr_close_fault(
                owner,
                process::Invocation {
                    program: "/bin/sh".into(),
                    arguments: vec![
                        "-c".into(),
                        format!("printf '{}\\n'; printf peer >&2", "b".repeat(64)).into(),
                    ],
                    environment: BTreeMap::from([("PATH".into(), "/usr/bin:/bin".into())]),
                    body_end: request.body_end,
                    settlement_end: request.settlement_end,
                    cleanup: false,
                },
            )
            .await;
            let facts = &completed.observation;
            self.actual_create = Some((
                facts.group_retired,
                facts.reaped,
                facts.stdout_eof && facts.stderr_eof,
                facts.stdout_body_eof,
                facts.exit_status,
            ));
            Native::project(completed, false)
        } else if request.role == Role::Remove && request.cleanup && self.cleanup_failure {
            self.daemon.roles.push((Role::Remove, true));
            CommandOutput {
                stdout: Vec::new(),
                stderr: b"inert cleanup refusal".to_vec(),
                status: Some(7),
                error: Some(Cause::Command),
                settled: true,
                stdout_complete: true,
            }
        } else {
            self.daemon.run(owner, request).await
        }
    }
}
#[test]
fn actual_create_zero_with_peer_close_failure_still_owns_its_exact_validator() {
    let mut observations = Vec::new();
    for cleanup_failure in [false, true] {
        let data = Data::new();
        let options = data.options();
        let started = Instant::now();
        let mut owner = Owner::new(started, options.whole, options.reserve).unwrap();
        let mut executor = CloseCreate {
            daemon: Daemon::new(&data),
            cleanup_failure,
            actual_create: None,
        };
        super::super::files::start_probe(None);
        files::start_probe();
        let runtime = runtime();
        let outcome = runtime.block_on(execute(
            &options,
            &data.access,
            &mut owner,
            &mut executor,
            started,
        ));
        drop(runtime);
        let finished = owner.finish();
        let shared = super::super::files::take_probe();
        let atomic = files::take_probe();
        let facts = executor.actual_create;
        let roles = executor.daemon.roles;
        drop(data);
        observations.push((outcome, finished, shared, atomic, facts, roles));
    }
    for (outcome, finished, shared, atomic, facts, roles) in observations {
        assert!(!outcome.passed);
        assert_eq!(finished, Err(Cause::Close));
        assert_eq!(facts, Some((true, true, true, true, Some(0))));
        assert!(roles.contains(&(Role::Remove, true)));
        assert!(!roles.iter().any(|(role, _)| *role == Role::Start));
        let report: serde_json::Value = serde_json::from_slice(&outcome.bytes).unwrap();
        assert_eq!(report["failure"], "native_close_refused");
        assert_eq!((shared.1, atomic.1), (0, 0));
    }
}

struct HeldCreateStdout {
    daemon: Daemon,
    facts: Option<(Option<i32>, bool, bool, bool, bool, bool)>,
    semantic_complete: bool,
}
impl Executor for HeldCreateStdout {
    async fn run(&mut self, owner: &mut Owner, request: Invocation) -> CommandOutput {
        if request.role == Role::Create {
            self.daemon.roles.push((Role::Create, false));
            let body_end = owner.now() + Duration::from_millis(80);
            let completion = process::run(
                owner,
                process::Invocation {
                    program: "/bin/sh".into(),
                    arguments: vec![
                        "-c".into(),
                        format!(
                            "printf '{}\\n'; (trap '' TERM; exec /bin/sleep 60) & exit 0",
                            "b".repeat(64),
                        )
                        .into(),
                    ],
                    environment: BTreeMap::from([("PATH".into(), "/usr/bin:/bin".into())]),
                    body_end,
                    settlement_end: (body_end + Duration::from_secs(10))
                        .min(request.settlement_end),
                    cleanup: false,
                },
            )
            .await;
            let o = &completion.observation;
            self.facts = Some((
                o.exit_status,
                o.stdout_eof,
                o.stdout_body_eof,
                o.group_retired,
                o.reaped,
                o.stdin_closed && o.stdout_closed && o.stderr_closed,
            ));
            let output = Native::project(completion, false);
            self.semantic_complete = output.stdout_complete;
            output
        } else {
            self.daemon.run(owner, request).await
        }
    }
}
#[test]
fn cleanup_generated_eof_does_not_authorize_a_create_id_from_a_terminal_leader() {
    let data = Data::new();
    let options = data.options();
    let started = Instant::now();
    let mut owner = Owner::new(started, options.whole, options.reserve).unwrap();
    let mut executor = HeldCreateStdout {
        daemon: Daemon::new(&data),
        facts: None,
        semantic_complete: false,
    };
    super::super::files::start_probe(None);
    files::start_probe();
    let runtime = runtime();
    let outcome = runtime.block_on(execute(
        &options,
        &data.access,
        &mut owner,
        &mut executor,
        started,
    ));
    drop(runtime);
    let finished = owner.finish();
    let shared = super::super::files::take_probe();
    let atomic = files::take_probe();
    let roles = executor.daemon.roles;
    let facts = executor.facts;
    let complete = executor.semantic_complete;
    let paths = fs::read_dir(data.access.root.join("results"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    let retained = paths
        .first()
        .map(|path| super::super::verification_fixture::read(&path.join("003.stdout")));
    drop(data);
    assert!(!outcome.passed && !complete);
    assert_eq!(finished, Err(Cause::Deadline));
    assert_eq!(facts, Some((Some(0), true, false, true, true, true)));
    assert_eq!(
        roles,
        [
            (Role::Load, false),
            (Role::Inspect, false),
            (Role::Create, false)
        ]
    );
    assert_eq!(
        retained.unwrap().unwrap(),
        format!("{}\n", "b".repeat(64)).as_bytes()
    );
    assert_eq!((shared.1, atomic.1), (0, 0));
}

struct CloseRemove {
    daemon: Daemon,
}
impl Executor for CloseRemove {
    async fn run(&mut self, owner: &mut Owner, request: Invocation) -> CommandOutput {
        if request.role == Role::Remove {
            self.daemon.roles.push((Role::Remove, request.cleanup));
            let completion = process::run_stderr_close_fault(
                owner,
                process::Invocation {
                    program: "/bin/sh".into(),
                    arguments: vec!["-c".into(), "printf removed".into()],
                    environment: BTreeMap::from([("PATH".into(), "/usr/bin:/bin".into())]),
                    body_end: request.body_end,
                    settlement_end: request.settlement_end,
                    cleanup: false,
                },
            )
            .await;
            Native::project(completion, false)
        } else {
            self.daemon.run(owner, request).await
        }
    }
}
#[test]
fn known_zero_remove_is_not_repeated_after_an_independent_peer_close_refusal() {
    let data = Data::new();
    let options = data.options();
    let started = Instant::now();
    let mut owner = Owner::new(started, options.whole, options.reserve).unwrap();
    let mut executor = CloseRemove {
        daemon: Daemon::new(&data),
    };
    super::super::files::start_probe(None);
    files::start_probe();
    let runtime = runtime();
    let outcome = runtime.block_on(execute(
        &options,
        &data.access,
        &mut owner,
        &mut executor,
        started,
    ));
    drop(runtime);
    let finished = owner.finish();
    let shared = super::super::files::take_probe();
    let atomic = files::take_probe();
    let removes = executor
        .daemon
        .roles
        .iter()
        .filter(|(role, _)| *role == Role::Remove)
        .copied()
        .collect::<Vec<_>>();
    drop(data);
    assert!(!outcome.passed);
    assert_eq!(finished, Err(Cause::Close));
    assert_eq!(removes, [(Role::Remove, false)]);
    assert_eq!((shared.1, atomic.1), (0, 0));
}

#[test]
fn owned_atomic_record_replacement_refuses_an_equal_byte_foreign_inode() {
    let fixture = Fixture::new();
    let path = fixture.0.join("staged.json");
    let started = Instant::now();
    let mut owner = Owner::new(started, Duration::from_secs(30), Duration::from_secs(5)).unwrap();
    super::super::files::start_probe(None);
    files::start_probe();
    let dir = Dir::open(&fixture.0, &mut owner).unwrap();
    let mut records = files::Records::default();
    let written = records.write(&dir, "staged.json", b"retained bytes", &mut owner, false);
    let moved = fs::rename(&path, fixture.0.join("retained-original"));
    let foreign = write(&path, b"retained bytes", 0o600);
    records.finish(&mut owner);
    dir.finish(&mut owner);
    let finished = owner.finish();
    let shared = super::super::files::take_probe();
    let atomic = files::take_probe();
    let remaining = super::super::verification_fixture::read(&path);
    drop(fixture);
    assert!(written.is_ok() && moved.is_ok() && foreign.is_ok());
    assert_eq!(finished, Err(Cause::Changed));
    assert_eq!(remaining.unwrap(), b"retained bytes");
    assert_eq!((shared.1, atomic.1), (0, 0));
    assert!(atomic.0 > 0);
}

fn lifecycle_worker() {
    let caller = nix::sys::stat::umask(nix::sys::stat::Mode::from_bits_truncate(0o027));
    let mut observations = Vec::new();
    for (offset, signal) in [
        (None, None),
        (Some(Duration::ZERO), None),
        (Some(Duration::from_secs(1)), None),
        (None, Some(signal_hook::consts::SIGINT)),
        (None, Some(signal_hook::consts::SIGTERM)),
    ] {
        let data = Data::new();
        let options = data.options();
        let started = Instant::now();
        let mut daemon = Daemon::new(&data);
        let mut facts = None;
        COMPLETION_OFFSET.set(offset);
        FRAME_CLOSED.set(false);
        COMPLETION_AFTER_FRAME.set(false);
        super::super::control::FINISH_SIGNAL.set(signal);
        let status = lifecycle(started, options.whole, || {
            let mut owner = Owner::new(started, options.whole, options.reserve).unwrap();
            super::super::files::start_probe(None);
            files::start_probe();
            let runtime = runtime();
            let outcome = runtime.block_on(execute(
                &options,
                &data.access,
                &mut owner,
                &mut daemon,
                started,
            ));
            let sink = data.fixture_path().join("emitted.json");
            let emitted = write(&sink, &outcome.bytes, 0o600);
            let readback = super::super::verification_fixture::read(&sink);
            let shared = super::super::files::take_probe();
            let atomic = files::take_probe();
            drop(runtime);
            FRAME_CLOSED.set(true);
            facts = Some((outcome.passed, emitted, readback, shared, atomic));
            (outcome.passed, Some(owner))
        });
        let observed_mask = nix::sys::stat::umask(nix::sys::stat::Mode::empty());
        nix::sys::stat::umask(observed_mask);
        let frame = FRAME_CLOSED.get();
        let after = COMPLETION_AFTER_FRAME.get();
        let finished = super::super::control::FINISHED.take();
        COMPLETION_OFFSET.set(None);
        drop(data);
        observations.push((
            offset,
            signal,
            status,
            observed_mask,
            frame,
            after,
            finished,
            facts,
        ));
    }
    nix::sys::stat::umask(caller);
    for (offset, signal, status, mask, frame, after, finished, facts) in observations {
        let (passed, emitted, readback, shared, atomic) = facts.unwrap();
        assert!(passed && emitted.is_ok() && frame);
        assert_eq!(mask, nix::sys::stat::Mode::from_bits_truncate(0o027));
        assert_eq!((shared.1, atomic.1), (0, 0));
        let report: serde_json::Value = serde_json::from_slice(&readback.unwrap()).unwrap();
        assert_eq!(report["passed"], true);
        assert_eq!(report["phase"], "complete");
        if let Some(signal) = signal {
            assert_eq!(status, ExitCode::FAILURE);
            assert_eq!(
                finished,
                Some(Err(if signal == signal_hook::consts::SIGINT {
                    Cause::Interrupt
                } else {
                    Cause::Terminate
                }))
            );
        } else if offset.is_some() {
            assert_eq!(status, ExitCode::FAILURE);
            assert!(after);
        } else {
            assert_eq!(status, ExitCode::SUCCESS);
        }
    }
}
impl Data {
    fn fixture_path(&self) -> &std::path::Path {
        &self._fixture.0
    }
}
#[test]
fn same_whole_lifecycle_fences_emitted_reports_after_runtime_and_mask_restoration() {
    const ROLE: &str = "STAGE_LIFECYCLE_CHILD";
    if std::env::var_os(ROLE).is_some() {
        return lifecycle_worker();
    }
    let program = std::env::current_exe().unwrap();
    let test = format!(
        "{}::same_whole_lifecycle_fences_emitted_reports_after_runtime_and_mask_restoration",
        module_path!().split_once("::").unwrap().1
    );
    let started = Instant::now();
    let mut owner = Owner::new(started, Duration::from_secs(60), Duration::from_secs(10)).unwrap();
    let runtime = runtime();
    let end = owner.body_end();
    let completed = runtime.block_on(process::run(
        &mut owner,
        process::Invocation {
            program,
            arguments: vec![
                "--exact".into(),
                test.into(),
                "--nocapture".into(),
                "--test-threads=1".into(),
            ],
            environment: BTreeMap::from([
                ("PATH".into(), "/usr/bin:/bin".into()),
                (ROLE.into(), "1".into()),
            ]),
            body_end: end - Duration::from_secs(10),
            settlement_end: end,
            cleanup: false,
        },
    ));
    drop(runtime);
    let finished = owner.finish();
    let o = &completed.observation;
    assert!(o.group_retired && o.reaped && o.stdout_eof && o.stderr_eof);
    assert!(o.stdin_closed && o.stdout_closed && o.stderr_closed);
    assert_eq!(o.exit_status, Some(0));
    assert!(finished.is_ok());
    assert!(completed.result.is_ok());
    assert!(
        String::from_utf8_lossy(&completed.stdout).contains("test result: ok. 1 passed; 0 failed")
    );
}

#[test]
fn python_tool_output_whitespace_is_accepted_by_the_complete_stage() {
    let mut observations = Vec::new();
    for case in ["create-c0", "wait-vt", "logs-nel", "logs-ls", "logs-crlf"] {
        let data = Data::new();
        let mut daemon = Daemon::new(&data);
        match case {
            "create-c0" => {
                daemon.create_stdout =
                    Some(format!("\u{1c}{}\u{1f}\n", "b".repeat(64)).into_bytes())
            }
            "wait-vt" => daemon.wait_stdout = Some(b"\x0b0\x0b".to_vec()),
            _ => {
                let ending = match case {
                    "logs-nel" => "\u{85}",
                    "logs-ls" => "\u{2028}",
                    _ => "\r\n",
                };
                let line = std::str::from_utf8(&data.migrations)
                    .unwrap()
                    .trim_end_matches('\n');
                daemon.logs = format!("{line}{ending}").into_bytes();
            }
        }
        let observed = operation(&data, &mut daemon);
        let roles = daemon.roles;
        drop(data);
        observations.push((case, observed, roles));
    }
    for (case, observed, roles) in observations {
        assert!(
            observed.outcome.passed,
            "{case}: original Python tool-output domain"
        );
        assert_eq!(observed.cause, None, "{case}");
        assert!(roles.contains(&(Role::Remove, false)), "{case}");
        assert!(!roles.contains(&(Role::Remove, true)), "{case}");
        assert!(observed.lock_released);
        assert_eq!((observed.shared.1, observed.atomic.1), (0, 0));
    }
}
#[test]
fn python_tool_output_decimal_versions_keep_conversion_limit_and_refusal_order() {
    let mut observations = Vec::new();
    for (version, passed, message, forced) in [
        (
            "184467440737095516160".to_owned(),
            false,
            "Archive migration manifest differs from image",
            false,
        ),
        (format!("{}1", "0".repeat(4299)), true, "", false),
        (format!("{}1", "0".repeat(4300)), false, "ValueError", true),
    ] {
        let data = Data::new();
        let mut daemon = Daemon::new(&data);
        let checksum = std::str::from_utf8(&data.migrations)
            .unwrap()
            .split_once("  ")
            .unwrap()
            .0;
        daemon.logs = format!("{checksum}  /app/migrations/{version}_wide.sql\n").into_bytes();
        let observed = operation(&data, &mut daemon);
        let roles = daemon.roles;
        drop(data);
        observations.push((passed, message, forced, observed, roles));
    }
    for (passed, message, forced, observed, roles) in observations {
        assert_eq!(observed.outcome.passed, passed, "{message}");
        let report: serde_json::Value = serde_json::from_slice(&observed.outcome.bytes).unwrap();
        if !passed {
            assert_eq!(report["failure"], message);
        }
        assert!(
            roles.contains(&(Role::Remove, forced)),
            "{message}: exact retirement phase"
        );
        assert!(
            !roles.contains(&(Role::Remove, !forced)),
            "{message}: no alternate retirement"
        );
        assert!(observed.lock_released);
        assert_eq!((observed.shared.1, observed.atomic.1), (0, 0));
    }
}

#[test]
fn python_tool_output_line_boundaries_keep_crlf_single_and_us_as_data() {
    let text = concat!(
        "alpha\r\nbravo\u{1c}charlie\u{1d}delta\u{1e}echo\u{b}",
        "foxtrot\u{c}golf\u{85}hotel\u{2028}india\u{2029}juliet\u{1f}kept",
    );
    assert_eq!(
        python_lines(text).collect::<Vec<_>>(),
        [
            "alpha",
            "bravo",
            "charlie",
            "delta",
            "echo",
            "foxtrot",
            "golf",
            "hotel",
            "india",
            "juliet\u{1f}kept"
        ],
    );
    assert!(python_lines("").next().is_none());
    assert_eq!(python_lines("\r\n").collect::<Vec<_>>(), [""]);
    assert_eq!(python_lines("one\n\n").collect::<Vec<_>>(), ["one", ""]);
    assert_eq!(python_strip("\u{1c}id\u{1f}"), "id");
    assert_eq!(python_byte_strip(b"\x0b0\x0c"), b"0");
    assert_eq!(python_byte_strip(b"\x1f0\x1f"), b"\x1f0\x1f");
}
