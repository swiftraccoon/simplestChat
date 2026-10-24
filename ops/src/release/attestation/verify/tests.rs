//! Complete cached-byte verification, late guards and actual native frame teardown.

use super::fixture::{self, Fixture};
use super::*;
use serde_json::{Value, json};
use std::{fs::OpenOptions, io::Write, os::unix::fs::OpenOptionsExt, path::Path};

const WHOLE: Duration = Duration::from_secs(45);
const RESERVE: Duration = Duration::from_secs(10);
fn options(fixture: &Fixture) -> Options {
    let mut options = Options::parse(&[
        "--directory".into(),
        fixture.candidate.as_os_str().into(),
        "--selection-json".into(),
        serde_json::to_string(&fixture.selection).unwrap().into(),
        "--timeout-seconds".into(),
        "45".into(),
        "--finalization-seconds".into(),
        "10".into(),
    ])
    .unwrap()
    .unwrap();
    options.environment = Some(BTreeMap::from([
        ("PATH".into(), fixture.bin.as_os_str().into()),
        ("HOME".into(), fixture.home.as_os_str().into()),
        ("UNRELATED".into(), "must-not-inherit".into()),
        ("LC_ALL".into(), "must-be-overridden".into()),
    ]));
    options
}
#[derive(Clone, Copy)]
enum Effect {
    None,
    Change,
    Expire,
    Command,
}
struct Inert<'a> {
    fixture: &'a Fixture,
    calls: Vec<process::Invocation>,
    effect: Effect,
}
impl Executor for Inert<'_> {
    async fn run(&mut self, owner: &mut Owner, invocation: process::Invocation) -> Result<Vec<u8>> {
        self.calls.push(invocation);
        if matches!(self.effect, Effect::Change) && self.calls.len() == 1 {
            let path = self.fixture.candidate.join("source.json");
            let mut file = OpenOptions::new()
                .append(true)
                .mode(0o600)
                .open(path)
                .map_err(|_| Cause::Io)?;
            let written = file.write_all(b"changed").map_err(|_| Cause::Io);
            let closed = nix::unistd::close(file).map_err(|_| Cause::Close);
            written.and(closed)?;
        }
        if matches!(self.effect, Effect::Expire) && self.calls.len() == 4 {
            owner.elapsed = Some(WHOLE);
        }
        if matches!(self.effect, Effect::Command) {
            owner.record(Cause::Command);
            return Err(Cause::Command);
        }
        Ok(self.fixture.response.clone())
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn whole_cached_fixture_returns_fresh_four_subject_proofs_on_every_use() {
    let fixture = Fixture::new();
    let before = fixture.snapshot().unwrap();
    let options = options(&fixture);
    let mut owner = Owner::new(Instant::now(), WHOLE, RESERVE).unwrap();
    let mut executor = Inert {
        fixture: &fixture,
        calls: Vec::new(),
        effect: Effect::None,
    };
    files::start_probe(None);
    let first = execute(&options, &mut owner, &mut executor).await;
    let second = execute(&options, &mut owner, &mut executor).await;
    let probes = files::take_probe();
    let after = fixture.snapshot();
    let trace = fixture::read(&fixture.home.join("argv"));
    let cause = owner.error();
    drop(owner);
    assert_eq!(probes.1, 0);
    assert!(probes.0 > 0);
    assert_eq!(cause, None);
    assert_eq!(before, after.unwrap());
    assert!(
        trace.unwrap().is_empty(),
        "Inert never executes the admitted program"
    );
    assert_eq!(executor.calls.len(), 8);
    let first: Value = serde_json::from_slice(&first.unwrap()).unwrap();
    let second: Value = serde_json::from_slice(&second.unwrap()).unwrap();
    assert_eq!(first, second);
    assert_eq!(first.as_object().unwrap().len(), 10);
    assert_eq!(first["attestation"]["predicate"], fixture.claim);
    let verification = first["attestation"]["verificationSha256"]
        .as_object()
        .unwrap();
    assert_eq!(verification.len(), 4);
    for (index, call) in executor.calls.iter().enumerate() {
        let name = trust::SUBJECTS[index % 4];
        assert_eq!(call.program, fixture.bin.join("gh"));
        assert_eq!(call.arguments[2], fixture.candidate.join(name).as_os_str());
        assert_eq!(call.environment.len(), 3);
        assert_eq!(
            call.environment.get(std::ffi::OsStr::new("LC_ALL")),
            Some(&"C".into())
        );
        assert!(call.body_end < call.settlement_end);
        assert_eq!(
            verification[name],
            files::hex(&Sha256::digest(&fixture.response))
        );
    }
    assert!(fixture.root.is_absolute() && fixture.cwd.is_dir());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn semantic_initial_and_late_refusals_keep_all_original_peers_closed() {
    for (name, late) in [
        ("image-security.json", false),
        ("source.json", true),
        ("outcome.json", true),
        ("release.json", true),
    ] {
        let mut fixture = Fixture::new();
        let mut value: Value =
            serde_json::from_slice(&fixture::read(&fixture.candidate.join(name)).unwrap()).unwrap();
        match name {
            "image-security.json" => value["passed"] = false.into(),
            "source.json" => value["inputsSha256"] = json!({}),
            "outcome.json" => value["error"] = "inert late error".into(),
            "release.json" => value["createdAt"] = "not-a-UTC-date".into(),
            _ => unreachable!(),
        }
        fixture
            .rewrite(name, &serde_json::to_vec(&value).unwrap())
            .unwrap();
        fixture.rebuild().unwrap();
        let before = fixture.snapshot().unwrap();
        let options = options(&fixture);
        let mut owner = Owner::new(Instant::now(), WHOLE, RESERVE).unwrap();
        let mut executor = Inert {
            fixture: &fixture,
            calls: Vec::new(),
            effect: Effect::None,
        };
        files::start_probe(None);
        let result = execute(&options, &mut owner, &mut executor).await;
        let probes = files::take_probe();
        let after = fixture.snapshot();
        let cause = owner.error();
        drop(owner);
        assert!(result.is_err(), "{name}");
        assert!(cause.is_some());
        assert_eq!(executor.calls.len(), if late { 4 } else { 0 }, "{name}");
        assert!(probes.0 > 0);
        assert_eq!(probes.1, 0, "{name}: all admitted original owners finished");
        assert_eq!(before, after.unwrap());
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn changed_original_or_original_clock_expiry_refuses_a_passive_envelope() {
    for (effect, expected, count) in [
        (Effect::Change, Cause::Changed, 1),
        (Effect::Expire, Cause::Deadline, 4),
        (Effect::Command, Cause::Command, 1),
    ] {
        let fixture = Fixture::new();
        let options = options(&fixture);
        let mut owner = Owner::new(Instant::now(), WHOLE, RESERVE).unwrap();
        let mut executor = Inert {
            fixture: &fixture,
            calls: Vec::new(),
            effect,
        };
        files::start_probe(None);
        let result = execute(&options, &mut owner, &mut executor).await;
        let probes = files::take_probe();
        let cause = owner.error();
        drop(owner);
        assert!(matches!(result, Err(actual) if actual == expected));
        assert_eq!(cause, Some(expected));
        assert_eq!(executor.calls.len(), count);
        assert!(probes.0 > 0);
        assert_eq!(probes.1, 0);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_first_checked_close_refusal_is_retained_and_all_known_peers_are_attempted() {
    let fixture = Fixture::new();
    let before = fixture.snapshot().unwrap();
    let options = options(&fixture);
    let mut owner = Owner::new(Instant::now(), WHOLE, RESERVE).unwrap();
    let mut executor = Inert {
        fixture: &fixture,
        calls: Vec::new(),
        effect: Effect::None,
    };
    files::start_probe(Some(1));
    let result = execute(&options, &mut owner, &mut executor).await;
    let probes = files::take_probe();
    let after = fixture.snapshot();
    let cause = owner.error();
    drop(owner);
    assert!(matches!(result, Err(Cause::Close)));
    assert_eq!(cause, Some(Cause::Close));
    assert!(probes.0 > 1);
    assert_eq!(probes.1, 0);
    assert!(executor.calls.is_empty());
    assert_eq!(before, after.unwrap());
}

fn emit_file(path: &Path, bytes: &[u8]) -> Result<Vec<u8>> {
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(path)
        .map_err(|_| Cause::Io)?;
    let written = file
        .write_all(bytes)
        .and_then(|()| file.flush())
        .map_err(|_| Cause::Io);
    let closed = nix::unistd::close(file).map_err(|_| Cause::Close);
    written.and(closed)?;
    fixture::read(path).map_err(|_| Cause::Io)
}

#[test]
fn complete_native_verification_closes_its_frame_before_the_original_outer_end_fence() {
    let mut observed = Vec::new();
    for offset in [None, Some(Duration::ZERO), Some(Duration::from_secs(1))] {
        let fixture = Fixture::new();
        let before = fixture.snapshot().unwrap();
        let options = options(&fixture);
        super::COMPLETION_PROBE.set(true);
        super::COMPLETION_OFFSET.set(offset);
        super::FRAME_CLOSED.set(false);
        super::COMPLETION_AFTER_FRAME.set(false);
        let mut operation = None;
        let started = Instant::now();
        let status = super::lifecycle(started, WHOLE, || {
            let mut owner = Owner::new(started, WHOLE, RESERVE).unwrap();
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap();
            let result = runtime.block_on(async {
                let mut native = Native::default();
                files::start_probe(None);
                let result = execute(&options, &mut owner, &mut native).await;
                let probes = files::take_probe();
                let emitted = result.as_ref().map_err(|cause| *cause).and_then(|bytes| {
                    owner.check(Window::Final)?;
                    emit_file(&fixture.root.join("emitted-envelope.json"), bytes)
                });
                if let Err(cause) = &emitted {
                    owner.record(*cause);
                }
                let final_check = owner.check(Window::Final);
                let cause = owner.error();
                (
                    result,
                    emitted.and_then(|bytes| final_check.map(|()| bytes)),
                    probes,
                    cause,
                    native.observations,
                )
            });
            drop(runtime);
            let passed = result.0.is_ok() && result.1.is_ok();
            operation = Some(result);
            super::FRAME_CLOSED.set(true);
            (passed, Some(owner))
        });
        let after = fixture.snapshot();
        let trace = fixture::read(&fixture.home.join("argv"));
        observed.push((
            offset,
            status,
            operation,
            before,
            after,
            trace,
            super::COMPLETION_AFTER_FRAME.get(),
        ));
        super::COMPLETION_PROBE.set(false);
        super::COMPLETION_OFFSET.set(None);
    }
    for (offset, status, operation, before, after, trace, closed) in observed {
        let (result, emitted, probes, cause, native) = operation.unwrap();
        assert!(closed);
        assert!(result.is_ok());
        let emitted = emitted.unwrap();
        assert_eq!(emitted, result.unwrap());
        let envelope: Value = serde_json::from_slice(&emitted).unwrap();
        assert_eq!(envelope.as_object().unwrap().len(), 10);
        assert!(
            envelope["attestation"]["verificationSha256"]
                .as_object()
                .is_some_and(|hashes| hashes.len() == 4)
        );
        assert_eq!(cause, None);
        assert!(probes.0 > 0);
        assert_eq!(probes.1, 0);
        assert_eq!(before, after.unwrap());
        let trace = String::from_utf8(trace.unwrap()).unwrap();
        assert_eq!(trace.lines().filter(|line| *line == "--call--").count(), 4);
        assert_eq!(native.len(), 4);
        assert!(native.iter().all(|o| o.group_retired
            && o.reaped
            && o.stdin_closed
            && o.stdout_closed
            && o.stderr_closed
            && o.stdout_eof
            && o.stderr_eof));
        assert_eq!(
            status,
            if offset.is_some() {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }
        );
    }
}

#[test]
fn late_real_signals_after_runtime_restore_refuse_cached_success() {
    use super::super::control::signal_tests::{self, Entry};
    let Some(expected) = signal_tests::worker(Entry::Cached) else {
        return signal_tests::supervise(Entry::Cached);
    };
    let previous = nix::sys::stat::umask(nix::sys::stat::Mode::from_bits_truncate(0o027));
    let fixture = Fixture::new();
    let options = options(&fixture);
    let started = Instant::now();
    let mut facts = None;
    let code = lifecycle(started, WHOLE, || {
        let mut owner = Owner::new(started, WHOLE, RESERVE).unwrap();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let result = runtime.block_on(async {
            files::start_probe(None);
            let mut native = Native::default();
            let result = execute(&options, &mut owner, &mut native).await;
            let emitted = result
                .as_ref()
                .map_err(|cause| *cause)
                .and_then(|bytes| emit_file(&fixture.root.join("emitted-envelope.json"), bytes));
            if let Err(cause) = &emitted {
                owner.record(*cause);
            }
            let passed = result.is_ok() && emitted.is_ok() && owner.check(Window::Final).is_ok();
            (passed, emitted, files::take_probe(), native.observations)
        });
        drop(runtime);
        let passed = result.0;
        facts = Some(result);
        (passed, Some(owner))
    });
    drop(fixture);
    let restored = nix::sys::stat::umask(previous);
    signal_tests::assert_finished(
        code,
        expected,
        nix::sys::stat::Mode::from_bits_truncate(0o027),
    );
    let (passed, emitted, probes, observations) = facts.unwrap();
    assert!(passed);
    let envelope: Value = serde_json::from_slice(&emitted.unwrap()).unwrap();
    assert_eq!(envelope["schemaVersion"], 1);
    assert_eq!(observations.len(), 4);
    assert!(observations.iter().all(|o| o.group_retired
        && o.reaped
        && o.stdin_closed
        && o.stdout_closed
        && o.stderr_closed));
    assert!(probes.0 > 0 && probes.1 == 0);
    assert_eq!(restored, nix::sys::stat::Mode::from_bits_truncate(0o027));
}
