//! Source admission and real inert child ownership; no unittest or service runs.

use super::*;
use std::{
    collections::BTreeSet,
    fs, io,
    os::unix::fs::{DirBuilderExt, symlink},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicI32, AtomicU8, AtomicU64, Ordering},
    },
    thread,
};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    path: PathBuf,
    root: PathBuf,
    bin: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "python-shard-fixture-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        let root = path.join("source");
        let bin = path.join("bin");
        fs::create_dir_all(root.join("ops/ansible/tests")).unwrap();
        fs::create_dir(&bin).unwrap();
        let fixture = Self { path, root, bin };
        for name in [
            "test_zeta.py",
            "test_beta.py",
            "test_alpha.py",
            "test_gamma.py",
        ] {
            fixture.test_file(name, "");
        }
        for (name, bytes) in [
            (
                "test_release_playbook.py",
                include_bytes!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../ops/ansible/tests/test_release_playbook.py"
                ))
                .as_slice(),
            ),
            (
                "test_python_quality.py",
                include_bytes!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../ops/ansible/tests/test_python_quality.py"
                ))
                .as_slice(),
            ),
            (
                "test_ci_verified.py",
                include_bytes!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../ops/ansible/tests/test_ci_verified.py"
                ))
                .as_slice(),
            ),
            (
                "test_release_container_ci.py",
                include_bytes!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../ops/ansible/tests/test_release_container_ci.py"
                ))
                .as_slice(),
            ),
            (
                "test_support.py",
                include_bytes!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../ops/ansible/tests/test_support.py"
                ))
                .as_slice(),
            ),
        ] {
            fs::write(fixture.root.join("ops/ansible/tests").join(name), bytes).unwrap();
        }
        let reference = fixture.root.join("xtask/src/ci/automation/reference");
        fs::create_dir_all(&reference).unwrap();
        fs::write(
            reference.join("maintenance-python.yml"),
            include_bytes!("reference/maintenance-python.yml"),
        )
        .unwrap();
        fs::write(
            reference.join("source-python.yml"),
            include_bytes!("reference/source-python.yml"),
        )
        .unwrap();
        fs::write(
            reference.join("release-python.yml"),
            include_bytes!("reference/release-python.yml"),
        )
        .unwrap();
        fs::write(
            reference.join("ci-python.yml"),
            include_bytes!("reference/ci-python.yml"),
        )
        .unwrap();
        fixture
    }

    fn test_file(&self, name: &str, body: &str) {
        fs::write(self.root.join("ops/ansible/tests").join(name), body).unwrap();
    }

    fn environment(&self) -> Environment {
        Environment::from([
            ("PATH".into(), self.bin.as_os_str().to_owned()),
            ("RUNNER_TEMP".into(), self.path.as_os_str().to_owned()),
            (
                "TMPDIR".into(),
                self.path.parent().unwrap().as_os_str().to_owned(),
            ),
            ("FIXTURE_ROOT".into(), self.path.as_os_str().to_owned()),
            ("SIMPLESTCHAT_TEST_SHARD_MODE".into(), "Complete".into()),
            ("KEEP_LITERAL".into(), "literal value with spaces".into()),
        ])
    }

    fn control(&self) -> Control {
        let now = Instant::now();
        let mut control = Control::new(now).unwrap();
        control.body = now + Duration::from_secs(10);
        control.end = now + Duration::from_secs(20);
        control
    }

    fn observed_control(&self) -> (Control, Arc<[AtomicI32; SLOTS]>) {
        let admitted = Arc::new([AtomicI32::new(0), AtomicI32::new(0), AtomicI32::new(0)]);
        let mut control = self.control();
        control.admitted = Some(admitted.clone());
        control.worker = true;
        (control, admitted)
    }

    fn workers_gone(&self) -> bool {
        (0..SLOTS).all(|ordinal| {
            let path = self.path.join(format!("{ordinal}.started"));
            if !path.exists() {
                return true;
            }
            let pid = fs::read_to_string(path)
                .ok()
                .and_then(|value| value.parse::<i32>().ok())
                .and_then(rustix::process::Pid::from_raw);
            pid.is_some_and(|pid| {
                // Read-only observation of a recorded inert actor, never signal authority.
                rustix::process::test_kill_process(pid) == Err(rustix::io::Errno::SRCH)
            })
        })
    }

    fn captures_removed(&self) -> bool {
        fs::read_dir(&self.path).unwrap().all(|entry| {
            let name = entry.unwrap().file_name().to_string_lossy().into_owned();
            !name.starts_with("simplestchat-python-shards-")
                && !name.starts_with("simplestchat-maintenance-reference-")
        })
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        // Every worker has its own finite watchdog. No saved PID authorizes fallback signals.
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn originals_reaped(admitted: &[AtomicI32; SLOTS], expected: usize) -> bool {
    let observed = admitted
        .iter()
        .map(|value| value.load(Ordering::SeqCst))
        .filter(|value| *value != 0)
        .collect::<Vec<_>>();
    observed.len() == expected
        && observed.into_iter().all(|pid| {
            rustix::process::Pid::from_raw(pid)
                .is_some_and(|pid| !simplestchat_ops::owned_process::reserved(pid))
        })
}

fn rejected_inventory(fixture: &Fixture) -> Option<Cause> {
    match Inventory::read(&fixture.root) {
        Ok(mut inventory) => {
            let closed = inventory.close();
            assert!(closed.is_ok());
            None
        }
        Err(cause) => Some(cause),
    }
}

fn strings(arguments: &[OsString]) -> Vec<&str> {
    arguments
        .iter()
        .map(|value| value.to_str().unwrap())
        .collect()
}

#[test]
fn flat_inventory_covers_each_module_once_and_preserves_the_discovery_contract() {
    let fixture = Fixture::new();
    fixture.test_file("helper.py", "");
    let mut inventory = Inventory::read(&fixture.root).unwrap();
    let groups = inventory.groups.clone();
    let commands = Profile::Parallel.arguments(&inventory).unwrap();
    let current = inventory.current();
    let closed = inventory.close();
    assert!(current.is_ok());
    assert!(closed.is_ok());
    assert_eq!(
        groups,
        [
            vec![
                "test_alpha",
                "test_ci_verified",
                "test_python_quality",
                "test_release_playbook",
                "test_zeta"
            ],
            vec![
                "test_beta",
                "test_gamma",
                "test_release_container_ci",
                "test_support"
            ]
        ]
    );
    let first: BTreeSet<_> = groups[0].iter().collect();
    let second: BTreeSet<_> = groups[1].iter().collect();
    assert!(first.is_disjoint(&second));
    assert_eq!(first.union(&second).count(), 9);
    assert_eq!(Profile::Parallel.roles().len(), 3);
    assert_eq!(commands.len(), 3);
    for (role, command) in &commands {
        if *role == Role::MaintenanceLegacy {
            assert_eq!(*command, legacy::routing::legacy_arguments());
            continue;
        }
        assert_eq!(&strings(command)[..6], BASE_ARGUMENTS);
        assert!(!strings(command).contains(&"test_release_playbook.*"));
    }
}

#[test]
fn disposable_database_keeps_one_current_discovery_and_one_frozen_legacy_class() {
    let fixture = Fixture::new();
    let mut inventory = Inventory::read(&fixture.root).unwrap();
    let mut environment = fixture.environment();
    let default = Profile::from_env(&environment);
    environment.insert("DISPOSABLE_TEST_DATABASE".into(), "1".into());
    let selected = Profile::from_env(&environment);
    let arguments = selected.arguments(&inventory).unwrap();
    let closed = inventory.close();
    assert!(closed.is_ok());
    assert_eq!(default, Profile::Parallel);
    assert_eq!(selected, Profile::Database);
    assert_eq!(
        selected.roles(),
        &[Role::CurrentFirst, Role::MaintenanceLegacy]
    );
    assert_eq!(arguments.len(), 2);
    assert_eq!(&strings(&arguments[0].1)[..6], BASE_ARGUMENTS);
    assert!(!strings(&arguments[0].1).contains(&"test_release_playbook.*"));
    assert_eq!(arguments[1].1, legacy::routing::legacy_arguments());
    environment.insert("DISPOSABLE_TEST_DATABASE".into(), "true".into());
    assert_eq!(Profile::from_env(&environment), Profile::Parallel);
}

#[test]
fn only_three_changed_release_methods_use_the_original_reference_in_both_profiles() {
    let fixture = Fixture::new();
    let mut inventory = Inventory::read(&fixture.root).unwrap();
    let prefix = "test_release_playbook.ReleasePlaybookTests.";
    let historical = [
        "test_stage_is_default_and_deployment_is_an_explicit_separate_job",
        "test_routine_release_has_no_host_maintenance_or_configuration_rendering",
        "test_attestation_precedes_every_remote_preflight_and_cached_bytes_are_reverified",
    ];
    for profile in [Profile::Parallel, Profile::Database] {
        let commands = profile.arguments(&inventory).unwrap();
        let current = commands
            .iter()
            .filter(|(role, _)| *role != Role::MaintenanceLegacy)
            .flat_map(|(_, args)| args.windows(2))
            .filter(|pair| pair[0] == "-k")
            .filter_map(|pair| pair[1].to_str())
            .filter_map(|selector| selector.strip_prefix(prefix))
            .collect::<BTreeSet<_>>();
        let legacy = commands
            .iter()
            .find(|(role, _)| *role == Role::MaintenanceLegacy)
            .unwrap()
            .1
            .iter()
            .filter_map(|argument| argument.to_str())
            .filter_map(|selector| selector.strip_prefix(prefix))
            .collect::<BTreeSet<_>>();
        assert_eq!(current.len(), 7);
        assert_eq!(legacy, historical.into_iter().collect());
        assert!(current.is_disjoint(&legacy));
        assert_eq!(current.union(&legacy).count(), 10);
        assert!(current.iter().all(|method| !method.contains('*')));
    }
    inventory.close().unwrap();
}

#[test]
fn symlink_fifo_invalid_names_and_importable_directories_are_refused() {
    for case in 0..5 {
        let fixture = Fixture::new();
        let tests = fixture.root.join("ops/ansible/tests");
        match case {
            0 => symlink(tests.join("test_alpha.py"), tests.join("test_link.py")).unwrap(),
            1 => nix::unistd::mkfifo(&tests.join("test_pipe.py"), nix::sys::stat::Mode::S_IRUSR)
                .unwrap(),
            2 => fixture.test_file("test_bad-name.py", ""),
            3 => {
                fs::create_dir(tests.join("nested")).unwrap();
                fs::write(tests.join("nested/__init__.py"), "").unwrap();
            }
            _ => {
                fs::create_dir(tests.join("__pycache__")).unwrap();
                fs::write(tests.join("__pycache__/__init__.py"), "").unwrap();
            }
        }
        let observed = rejected_inventory(&fixture);
        assert_eq!(observed, Some(Cause::Inventory), "case {case}");
    }
}

#[test]
fn ordinary_bytecode_cache_is_allowed_but_source_or_import_topology_changes_refuse() {
    let fixture = Fixture::new();
    let cache = fixture.root.join("ops/ansible/tests/__pycache__");
    fs::create_dir(&cache).unwrap();
    let mut inventory = Inventory::read(&fixture.root).unwrap();
    fs::write(cache.join("test_alpha.cpython-314.pyc"), b"inert cache").unwrap();
    let ordinary = inventory.current();
    fs::write(cache.join("__init__.py"), b"").unwrap();
    let importable = inventory.current();
    fs::remove_file(cache.join("__init__.py")).unwrap();
    fixture.test_file(
        "test_alpha.py",
        "# Changed bytes with a different length.\n",
    );
    let changed = inventory.current();
    let closed = inventory.close();
    assert!(ordinary.is_ok());
    assert_eq!(importable, Err(Cause::Inventory));
    assert_eq!(changed, Err(Cause::Inventory));
    assert!(closed.is_ok());
}

#[test]
fn changed_inventory_directory_cannot_replace_the_retained_original() {
    let fixture = Fixture::new();
    let tests = fixture.root.join("ops/ansible/tests");
    fs::rename(&tests, fixture.root.join("ops/ansible/original-tests")).unwrap();
    fs::create_dir(&tests).unwrap();
    for name in ["test_alpha.py", "test_beta.py"] {
        fixture.test_file(name, "");
    }
    let mut inventory = Inventory::read(&fixture.root).unwrap();
    fs::rename(&tests, fixture.root.join("ops/ansible/replaced-tests")).unwrap();
    fs::create_dir(&tests).unwrap();
    let changed = inventory.current();
    let closed = inventory.close();
    assert_eq!(changed, Err(Cause::Inventory));
    assert!(closed.is_ok());
}

const WORKER_FILTER: &str = "ci::automation::python_shards::tests::rust_inert_worker";

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkerPlan {
    program: String,
    arguments: Vec<String>,
}

fn worker_signal(signals: &mut CompletionSignals, root: &Path, ordinal: usize) -> bool {
    if let Some(code) = signals.status() {
        fs::write(root.join(format!("{ordinal}.term")), code.to_string()).unwrap();
        assert!(signals.finish().is_ok());
        true
    } else {
        false
    }
}

fn worker_expired(signals: &mut CompletionSignals, status: i32) -> ! {
    let _ = signals.finish();
    std::process::exit(status)
}

/// A fixed Rust-only helper: ordinary test discovery does no fixture work.
#[test]
fn rust_inert_worker() {
    let Some(encoded) = std::env::var_os("SIMPLESTCHAT_TEST_SHARD_PLAN") else {
        return;
    };
    let invocation = std::env::args().skip(1).collect::<Vec<_>>();
    assert_eq!(
        invocation.iter().map(String::as_str).collect::<Vec<_>>(),
        ["--exact", WORKER_FILTER, "--nocapture", "--test-threads=1"]
    );
    let plan: WorkerPlan = serde_json::from_str(encoded.to_str().unwrap()).unwrap();
    assert_eq!(plan.program, "python");
    let ordinal = if plan.arguments == strings(&legacy::routing::legacy_arguments()) {
        2
    } else {
        assert_eq!(
            plan.arguments[..6]
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            BASE_ARGUMENTS
        );
        if plan.arguments.iter().any(|value| value == "test_alpha.*") {
            0
        } else {
            1
        }
    };
    let active = if std::env::var_os("DISPOSABLE_TEST_DATABASE").as_deref()
        == Some(std::ffi::OsStr::new("1"))
    {
        vec![0, 2]
    } else {
        vec![0, 1, 2]
    };
    let mode = std::env::var("SIMPLESTCHAT_TEST_SHARD_MODE").unwrap();
    assert!(matches!(
        mode.as_str(),
        "Complete" | "FailFirst" | "FailLegacy" | "WaitCancel" | "OverflowLegacy"
    ));
    let root = PathBuf::from(std::env::var_os("FIXTURE_ROOT").unwrap())
        .canonicalize()
        .unwrap();
    let parent = rustix::process::getppid().unwrap().as_raw_nonzero().get();
    let prefix = format!("python-shard-fixture-{parent}-");
    assert!(
        root.file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .strip_prefix(&prefix)
            .is_some_and(
                |counter| !counter.is_empty() && counter.bytes().all(|byte| byte.is_ascii_digit())
            )
    );
    assert!(root.starts_with(std::env::temp_dir().canonicalize().unwrap()));
    assert_eq!(std::env::current_dir().unwrap(), root.join("source"));
    assert_eq!(
        std::env::var("KEEP_LITERAL").unwrap(),
        "literal value with spaces"
    );
    let record = serde_json::json!({
        "arguments": &plan.arguments, "cwd": std::env::current_dir().unwrap(),
        "literal": std::env::var("KEEP_LITERAL").unwrap(), "program": &plan.program,
        "pythonPathPresent": std::env::var_os("PYTHONPATH").is_some(),
    });
    fs::write(
        root.join(format!("{ordinal}.arguments.json")),
        serde_json::to_vec(&record).unwrap(),
    )
    .unwrap();
    let mut signals = CompletionSignals::new().unwrap();
    fs::write(
        root.join(format!("{ordinal}.started")),
        std::process::id().to_string(),
    )
    .unwrap();
    let end = Instant::now() + Duration::from_secs(6);
    while !active
        .iter()
        .all(|peer| root.join(format!("{peer}.started")).is_file())
    {
        if worker_signal(&mut signals, &root, ordinal) {
            return;
        }
        if Instant::now() >= end {
            worker_expired(&mut signals, 97);
        }
        thread::sleep(Duration::from_millis(5));
    }
    if mode == "WaitCancel" {
        loop {
            if worker_signal(&mut signals, &root, ordinal) {
                return;
            }
            if Instant::now() >= end {
                worker_expired(&mut signals, 98);
            }
            thread::sleep(Duration::from_millis(5));
        }
    }
    if mode == "OverflowLegacy" && ordinal == 2 {
        let _ = std::io::stdout().write_all(&vec![b'x'; 1024 * 1024 + 4096]);
        let _ = std::io::stdout().flush();
        let _ = signals.finish();
        return;
    }
    if mode == "FailLegacy" && ordinal == 2 {
        worker_expired(&mut signals, 23);
    }
    if mode == "FailFirst" && ordinal == 0 {
        fs::write(root.join("0.failed"), b"actual exit23").unwrap();
        worker_expired(&mut signals, 23);
    }
    if mode == "FailFirst" {
        // Finite actual peer work, after the other shard's deliberate refusal.
        let work_end = (Instant::now() + Duration::from_millis(200)).min(end);
        while Instant::now() < work_end {
            if worker_signal(&mut signals, &root, ordinal) {
                return;
            }
            thread::sleep(Duration::from_millis(5));
        }
    }
    fs::write(root.join(format!("{ordinal}.completed")), b"natural").unwrap();
    if mode == "FailFirst" {
        println!("peer-completed-naturally");
    } else {
        println!("shard-{ordinal}-complete");
    }
    std::io::stdout().flush().unwrap();
    assert!(signals.finish().is_ok());
}

#[test]
fn actual_workers_overlap_preserve_context_and_replay_in_shard_order() {
    let fixture = Fixture::new();
    let (control, admitted) = fixture.observed_control();
    let mut output = Vec::new();
    let result = run_with(&fixture.root, &fixture.environment(), control, &mut output);
    let gone = fixture.workers_gone();
    let removed = fixture.captures_removed();
    let records = (0..SLOTS)
        .map(|ordinal| {
            serde_json::from_slice::<serde_json::Value>(
                &fs::read(fixture.path.join(format!("{ordinal}.arguments.json"))).unwrap(),
            )
            .unwrap()
        })
        .collect::<Vec<_>>();
    assert!(originals_reaped(&admitted, 3));
    assert_eq!(result, Ok(()));
    assert!(gone);
    assert!(removed);
    let replay = String::from_utf8(output).unwrap();
    let first = replay.find("shard-0-complete").unwrap();
    let second = replay.find("shard-1-complete").unwrap();
    assert!(first < second);
    for (ordinal, record) in records.into_iter().enumerate() {
        assert_eq!(record["cwd"], fixture.root.to_string_lossy().as_ref());
        assert_eq!(record["literal"], "literal value with spaces");
        assert_eq!(record["program"], "python");
        assert_eq!(record["pythonPathPresent"], ordinal == 2);
    }
}

#[test]
fn failed_child_does_not_cancel_its_peer_and_both_originals_settle() {
    let fixture = Fixture::new();
    let mut environment = fixture.environment();
    environment.insert("SIMPLESTCHAT_TEST_SHARD_MODE".into(), "FailFirst".into());
    let (control, admitted) = fixture.observed_control();
    let mut output = Vec::new();
    let result = run_with(&fixture.root, &environment, control, &mut output);
    let gone = fixture.workers_gone();
    let completed = fixture.path.join("1.completed").is_file();
    let removed = fixture.captures_removed();
    assert!(originals_reaped(&admitted, 3));
    assert_eq!(
        result,
        Err(Cause::Child {
            ordinal: 0,
            status: 23
        })
    );
    assert!(completed);
    assert!(gone);
    assert!(removed);
    assert!(
        String::from_utf8(output)
            .unwrap()
            .contains("peer-completed-naturally")
    );
}

#[test]
fn first_spawn_refusal_still_closes_the_prepared_capture_and_inventory() {
    let fixture = Fixture::new();
    let (mut control, admitted) = fixture.observed_control();
    control.worker = false;
    let mut output = Vec::new();
    let result = run_with(&fixture.root, &fixture.environment(), control, &mut output);
    let removed = fixture.captures_removed();
    assert!(originals_reaped(&admitted, 0));
    assert_eq!(result, Err(Cause::Spawn(127)));
    assert!(removed);
    assert!(!fixture.path.join("0.started").exists());
    assert!(!fixture.path.join("1.started").exists());
}

#[test]
fn second_spawn_refusal_retires_the_actual_already_admitted_first_leader() {
    let fixture = Fixture::new();
    let (mut control, admitted) = fixture.observed_control();
    control.spawn_refusal = Some(1);
    let mut output = Vec::new();
    let result = run_with(&fixture.root, &fixture.environment(), control, &mut output);
    let reaped = originals_reaped(&admitted, 1);
    let gone = fixture.workers_gone();
    let removed = fixture.captures_removed();
    assert_eq!(result, Err(Cause::Spawn(127)));
    assert!(admitted[0].load(Ordering::SeqCst) > 0);
    assert_eq!(admitted[1].load(Ordering::SeqCst), 0);
    assert!(reaped);
    assert!(gone);
    assert!(removed);
}

#[test]
fn injected_cancellation_retires_both_original_groups_before_return() {
    let fixture = Fixture::new();
    let mut environment = fixture.environment();
    environment.insert("SIMPLESTCHAT_TEST_SHARD_MODE".into(), "WaitCancel".into());
    let sticky = Arc::new(AtomicU8::new(0));
    let (mut control, admitted) = fixture.observed_control();
    control.signal = Some(sticky.clone());
    let path = fixture.path.clone();
    let mut output = Vec::new();
    let (result, both_started) = thread::scope(|scope| {
        let coordinator = scope.spawn(move || {
            let end = Instant::now() + Duration::from_secs(5);
            let mut ready = false;
            while Instant::now() < end {
                if (0..SLOTS).all(|ordinal| path.join(format!("{ordinal}.started")).is_file()) {
                    ready = true;
                    break;
                }
                thread::sleep(Duration::from_millis(5));
            }
            sticky.store(143, Ordering::SeqCst);
            ready
        });
        let result = run_with(&fixture.root, &environment, control, &mut output);
        (result, coordinator.join().unwrap())
    });
    let gone = fixture.workers_gone();
    let terms = (0..SLOTS).all(|ordinal| fixture.path.join(format!("{ordinal}.term")).is_file());
    let removed = fixture.captures_removed();
    assert!(originals_reaped(&admitted, 3));
    assert!(both_started);
    assert_eq!(result, Err(Cause::Signal(143)));
    assert!(terms);
    assert!(gone);
    assert!(removed);
}

struct RefusingWriter {
    writes: usize,
}
impl Write for RefusingWriter {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        self.writes += 1;
        Err(io::Error::other("inert replay refusal"))
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn replay_io_refusal_cannot_hide_peer_retirement_or_leave_captures() {
    let fixture = Fixture::new();
    let (control, admitted) = fixture.observed_control();
    let mut output = RefusingWriter { writes: 0 };
    let result = run_with(&fixture.root, &fixture.environment(), control, &mut output);
    let gone = fixture.workers_gone();
    let completed =
        (0..SLOTS).all(|ordinal| fixture.path.join(format!("{ordinal}.completed")).is_file());
    let removed = fixture.captures_removed();
    assert!(originals_reaped(&admitted, 3));
    assert_eq!(result, Err(Cause::Io));
    assert!(output.writes > 0);
    assert!(completed);
    assert!(gone);
    assert!(removed);
}

#[test]
fn expired_original_body_refuses_before_spawning_any_worker() {
    let fixture = Fixture::new();
    let mut control = fixture.control();
    control.body = Instant::now() - Duration::from_secs(1);
    let mut output = Vec::new();
    let result = run_with(&fixture.root, &fixture.environment(), control, &mut output);
    assert_eq!(result, Err(Cause::Deadline));
    assert!(!fixture.path.join("0.started").exists());
    assert!(!fixture.path.join("1.started").exists());
    assert!(fixture.captures_removed());
}

#[test]
fn third_spawn_refusal_retires_both_admitted_current_groups() {
    let fixture = Fixture::new();
    let (mut control, admitted) = fixture.observed_control();
    control.spawn_refusal = Some(2);
    let result = run_with(
        &fixture.root,
        &fixture.environment(),
        control,
        &mut Vec::new(),
    );
    assert_eq!(result, Err(Cause::Spawn(127)));
    assert!(originals_reaped(&admitted, 2));
    assert!(fixture.workers_gone());
    assert!(fixture.captures_removed());
}
#[test]
fn database_profile_keeps_one_current_group_and_one_legacy_group() {
    let fixture = Fixture::new();
    let mut environment = fixture.environment();
    environment.insert("DISPOSABLE_TEST_DATABASE".into(), "1".into());
    let (control, admitted) = fixture.observed_control();
    assert_eq!(
        run_with(&fixture.root, &environment, control, &mut Vec::new()),
        Ok(())
    );
    assert!(originals_reaped(&admitted, 2));
    assert_eq!(admitted[1].load(Ordering::SeqCst), 0);
    assert!(fixture.path.join("0.completed").is_file());
    assert!(fixture.path.join("2.completed").is_file());
    assert!(fixture.captures_removed());
}
#[test]
fn legacy_failure_does_not_suppress_current_peer_work_or_cleanup() {
    let fixture = Fixture::new();
    let mut environment = fixture.environment();
    environment.insert("SIMPLESTCHAT_TEST_SHARD_MODE".into(), "FailLegacy".into());
    let (control, admitted) = fixture.observed_control();
    assert_eq!(
        run_with(&fixture.root, &environment, control, &mut Vec::new()),
        Err(Cause::Child {
            ordinal: 2,
            status: 23
        })
    );
    assert!(originals_reaped(&admitted, 3));
    assert!(fixture.path.join("0.completed").is_file());
    assert!(fixture.path.join("1.completed").is_file());
    assert!(fixture.captures_removed());
}
#[test]
fn legacy_capture_overflow_cannot_report_success_or_leave_workers() {
    let fixture = Fixture::new();
    let mut environment = fixture.environment();
    environment.insert(
        "SIMPLESTCHAT_TEST_SHARD_MODE".into(),
        "OverflowLegacy".into(),
    );
    let (control, admitted) = fixture.observed_control();
    assert!(run_with(&fixture.root, &environment, control, &mut Vec::new()).is_err());
    assert!(originals_reaped(&admitted, 3));
    assert!(fixture.workers_gone());
    assert!(fixture.captures_removed());
}
#[test]
fn legacy_original_and_copy_pins_are_observed_and_all_known_closes_are_attempted() {
    let fixture = Fixture::new();
    let mut control = fixture.control();
    let mut owner = LegacyMaintenance::empty();
    owner
        .prepare(&fixture.root, &fixture.environment(), &mut control)
        .unwrap();
    let private = owner.private_root().unwrap().to_owned();
    let copied = private.join("ops/ansible/tests/test_release_playbook.py");
    assert_eq!(
        fs::read(&copied).unwrap(),
        fs::read(
            fixture
                .root
                .join("ops/ansible/tests/test_release_playbook.py")
        )
        .unwrap()
    );
    let mut bytes = fs::read(&copied).unwrap();
    bytes[0] ^= 1;
    fs::write(&copied, bytes).unwrap();
    assert_eq!(owner.current(&mut control), Err(Cause::Inventory));
    owner.inject_close_refusal(0);
    assert_eq!(owner.close(&mut control), Err(Cause::Inventory));
    assert!(owner.all_known_handles_closed());
    assert_eq!(owner.close_attempts(), 26);
    assert!(!private.exists());
    assert!(control.finish().is_ok());
}
#[test]
fn unknown_legacy_namespace_is_preserved_and_cleanup_refuses() {
    let fixture = Fixture::new();
    let mut control = fixture.control();
    let mut owner = LegacyMaintenance::empty();
    owner
        .prepare(&fixture.root, &fixture.environment(), &mut control)
        .unwrap();
    let private = owner.private_root().unwrap().to_owned();
    fs::write(private.join("ops/ansible/tests/unknown"), b"unowned").unwrap();
    assert_eq!(owner.close(&mut control), Err(Cause::Cleanup));
    assert!(owner.all_known_handles_closed());
    assert!(private.join("ops/ansible/tests/unknown").is_file());
    assert!(control.finish().is_ok());
}
#[test]
fn source_pin_failure_never_starts_a_child_and_retains_first_cause() {
    let fixture = Fixture::new();
    let path = fixture
        .root
        .join("ops/ansible/tests/test_release_playbook.py");
    let mut bytes = fs::read(&path).unwrap();
    bytes[0] ^= 1;
    fs::write(&path, bytes).unwrap();
    let (control, admitted) = fixture.observed_control();
    assert_eq!(
        run_with(
            &fixture.root,
            &fixture.environment(),
            control,
            &mut Vec::new()
        ),
        Err(Cause::Inventory)
    );
    assert!(originals_reaped(&admitted, 0));
    assert!(fixture.captures_removed());
}
#[test]
fn checked_close_refusal_still_closes_every_known_file_and_keeps_prior_cause() {
    let fixture = Fixture::new();
    let mut control = fixture.control();
    let mut owner = LegacyMaintenance::empty();
    owner
        .prepare(&fixture.root, &fixture.environment(), &mut control)
        .unwrap();
    let private = owner.private_root().unwrap().to_owned();
    owner.inject_close_refusal(0);
    control.note(Cause::Child {
        ordinal: 2,
        status: 23,
    });
    let closed = owner.close(&mut control);
    assert_eq!(closed, Err(Cause::Close));
    control.remember(closed);
    assert!(owner.all_known_handles_closed());
    assert_eq!(owner.close_attempts(), 26);
    assert!(!private.exists());
    assert_eq!(
        control.finish(),
        Err(Cause::Child {
            ordinal: 2,
            status: 23
        })
    );
}
#[test]
fn symlink_and_hardlinked_frozen_sources_refuse_without_a_child() {
    for hard in [false, true] {
        let fixture = Fixture::new();
        let source = fixture
            .root
            .join("ops/ansible/tests/test_release_playbook.py");
        let saved = fixture.path.join("held-source");
        fs::rename(&source, &saved).unwrap();
        if hard {
            fs::hard_link(&saved, &source).unwrap();
        } else {
            symlink(&saved, &source).unwrap();
        }
        let mut control = fixture.control();
        let mut owner = LegacyMaintenance::empty();
        assert_eq!(
            owner.prepare(&fixture.root, &fixture.environment(), &mut control),
            Err(Cause::Inventory)
        );
        assert!(owner.close(&mut control).is_ok());
        assert!(owner.all_known_handles_closed());
        assert!(!fixture.path.join("0.started").exists());
        assert!(fixture.captures_removed());
        assert!(control.finish().is_ok());
    }
}

#[test]
fn workflow_reference_copies_support_and_yaml_and_owns_results_until_checked_close() {
    let fixture = Fixture::new();
    let mut control = fixture.control();
    let mut owner = LegacyMaintenance::empty();
    owner
        .prepare(&fixture.root, &fixture.environment(), &mut control)
        .unwrap();
    let private = owner.private_root().unwrap().to_owned();
    for (source, copied) in [
        (
            "ops/ansible/tests/test_python_quality.py",
            "ops/ansible/tests/test_python_quality.py",
        ),
        (
            "ops/ansible/tests/test_ci_verified.py",
            "ops/ansible/tests/test_ci_verified.py",
        ),
        (
            "ops/ansible/tests/test_release_container_ci.py",
            "ops/ansible/tests/test_release_container_ci.py",
        ),
        (
            "ops/ansible/tests/test_support.py",
            "ops/ansible/tests/test_support.py",
        ),
        (
            "xtask/src/ci/automation/reference/ci-python.yml",
            ".github/workflows/ci.yml",
        ),
        (
            "xtask/src/ci/automation/reference/release-python.yml",
            "ops/ansible/release.yml",
        ),
    ] {
        assert_eq!(
            fs::read(fixture.root.join(source)).unwrap(),
            fs::read(private.join(copied)).unwrap()
        );
    }
    assert!(private.join("results").is_dir());
    let environment = owner.environment(&fixture.environment()).unwrap();
    assert_eq!(
        environment[std::ffi::OsStr::new("KEEP_LITERAL")],
        "literal value with spaces"
    );
    let paths =
        std::env::split_paths(&environment[std::ffi::OsStr::new("PYTHONPATH")]).collect::<Vec<_>>();
    assert_eq!(
        paths,
        [
            private.join("ops/ansible/tests"),
            fixture.root.join("ops/ansible/tests"),
            fixture.root.join("build"),
            fixture.root.join("ops/ansible/files"),
            fixture.root.join("ops/ansible/callback_plugins"),
            fixture.root.join("ops/ansible/filter_plugins"),
        ]
    );
    let path = private.join(".github/workflows/ci.yml");
    let mut bytes = fs::read(&path).unwrap();
    bytes[0] ^= 1;
    fs::write(path, bytes).unwrap();
    assert_eq!(owner.current(&mut control), Err(Cause::Inventory));
    assert_eq!(owner.close(&mut control), Err(Cause::Inventory));
    assert!(owner.all_known_handles_closed());
    assert_eq!(owner.close_attempts(), 26);
    assert!(!private.exists());
    assert!(control.finish().is_ok());
}

#[test]
fn every_added_reference_source_pin_refuses_before_any_child() {
    for source in [
        "ops/ansible/tests/test_python_quality.py",
        "ops/ansible/tests/test_ci_verified.py",
        "ops/ansible/tests/test_release_container_ci.py",
        "ops/ansible/tests/test_support.py",
        "xtask/src/ci/automation/reference/ci-python.yml",
        "xtask/src/ci/automation/reference/release-python.yml",
    ] {
        let fixture = Fixture::new();
        let path = fixture.root.join(source);
        let mut bytes = fs::read(&path).unwrap();
        bytes[0] ^= 1;
        fs::write(path, bytes).unwrap();
        let (control, admitted) = fixture.observed_control();
        assert_eq!(
            run_with(
                &fixture.root,
                &fixture.environment(),
                control,
                &mut Vec::new()
            ),
            Err(Cause::Inventory),
            "{source}"
        );
        assert!(originals_reaped(&admitted, 0));
        assert!(fixture.captures_removed());
    }
}
