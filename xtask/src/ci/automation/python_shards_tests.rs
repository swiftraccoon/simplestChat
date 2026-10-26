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
        control.end = now + Duration::from_secs(15);
        control
    }

    fn observed_control(&self) -> (Control, Arc<[AtomicI32; 2]>) {
        let admitted = Arc::new([AtomicI32::new(0), AtomicI32::new(0)]);
        let mut control = self.control();
        control.admitted = Some(admitted.clone());
        control.worker = true;
        (control, admitted)
    }

    fn workers_gone(&self) -> bool {
        (0..2).all(|ordinal| {
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
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("simplestchat-python-shards-")
        })
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        // Every worker has its own finite watchdog. No saved PID authorizes fallback signals.
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn originals_reaped(admitted: &[AtomicI32; 2], expected: usize) -> bool {
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
    let commands = Profile::Parallel.arguments(&inventory);
    let current = inventory.current();
    let closed = inventory.close();
    assert!(current.is_ok());
    assert!(closed.is_ok());
    assert_eq!(
        groups,
        [
            vec!["test_alpha", "test_gamma"],
            vec!["test_beta", "test_zeta"]
        ]
    );
    let first: BTreeSet<_> = groups[0].iter().collect();
    let second: BTreeSet<_> = groups[1].iter().collect();
    assert!(first.is_disjoint(&second));
    assert_eq!(first.union(&second).count(), 4);
    assert_eq!(Profile::Parallel.count(), 2);
    assert_eq!(commands.len(), 2);
    for (ordinal, command) in commands.iter().enumerate() {
        assert_eq!(&strings(command)[..6], BASE_ARGUMENTS);
        let expected = groups[ordinal]
            .iter()
            .flat_map(|name| ["-k".to_owned(), format!("{name}.*")])
            .collect::<Vec<_>>();
        assert_eq!(strings(&command[6..]), expected);
    }
}

#[test]
fn disposable_database_keeps_one_original_discovery_without_selectors() {
    let fixture = Fixture::new();
    let mut inventory = Inventory::read(&fixture.root).unwrap();
    let mut environment = fixture.environment();
    let default = Profile::from_env(&environment);
    environment.insert("DISPOSABLE_TEST_DATABASE".into(), "1".into());
    let selected = Profile::from_env(&environment);
    let arguments = selected.arguments(&inventory);
    let closed = inventory.close();
    assert!(closed.is_ok());
    assert_eq!(default, Profile::Parallel);
    assert_eq!(selected, Profile::Database);
    assert_eq!(selected.count(), 1);
    assert_eq!(arguments.len(), 1);
    assert_eq!(strings(&arguments[0]), BASE_ARGUMENTS);
    environment.insert("DISPOSABLE_TEST_DATABASE".into(), "true".into());
    assert_eq!(Profile::from_env(&environment), Profile::Parallel);
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
    assert!(plan.arguments.len() >= BASE_ARGUMENTS.len());
    assert_eq!(
        plan.arguments[..BASE_ARGUMENTS.len()]
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        BASE_ARGUMENTS,
    );
    let tail = &plan.arguments[BASE_ARGUMENTS.len()..];
    let (ordinal, count) = if tail.is_empty() {
        (0, 1)
    } else {
        assert_eq!(tail.len(), 4);
        assert_eq!(tail[0], "-k");
        assert_eq!(tail[2], "-k");
        let patterns = [tail[1].as_str(), tail[3].as_str()];
        if patterns == ["test_alpha.*", "test_gamma.*"] {
            (0, 2)
        } else {
            assert_eq!(patterns, ["test_beta.*", "test_zeta.*"]);
            (1, 2)
        }
    };
    let mode = std::env::var("SIMPLESTCHAT_TEST_SHARD_MODE").unwrap();
    assert!(matches!(
        mode.as_str(),
        "Complete" | "FailFirst" | "WaitCancel"
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
    while count == 2 && !root.join(format!("{}.started", 1 - ordinal)).is_file() {
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
    let records = (0..2)
        .map(|ordinal| {
            serde_json::from_slice::<serde_json::Value>(
                &fs::read(fixture.path.join(format!("{ordinal}.arguments.json"))).unwrap(),
            )
            .unwrap()
        })
        .collect::<Vec<_>>();
    assert!(originals_reaped(&admitted, 2));
    assert_eq!(result, Ok(()));
    assert!(gone);
    assert!(removed);
    let replay = String::from_utf8(output).unwrap();
    let first = replay.find("shard-0-complete").unwrap();
    let second = replay.find("shard-1-complete").unwrap();
    assert!(first < second);
    for record in records {
        assert_eq!(record["cwd"], fixture.root.to_string_lossy().as_ref());
        assert_eq!(record["literal"], "literal value with spaces");
        assert_eq!(record["program"], "python");
        assert_eq!(record["pythonPathPresent"], false);
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
    assert!(originals_reaped(&admitted, 2));
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
                if path.join("0.started").is_file() && path.join("1.started").is_file() {
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
    let terms = (0..2).all(|ordinal| fixture.path.join(format!("{ordinal}.term")).is_file());
    let removed = fixture.captures_removed();
    assert!(originals_reaped(&admitted, 2));
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
        (0..2).all(|ordinal| fixture.path.join(format!("{ordinal}.completed")).is_file());
    let removed = fixture.captures_removed();
    assert!(originals_reaped(&admitted, 2));
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
