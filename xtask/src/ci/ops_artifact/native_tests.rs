//! Inert selected-tool supervision regressions.

use super::{fixture::*, namespace::*, *};
use crate::{
    ci::{
        command::{Command, Program},
        process::{Executor, Native},
    },
    owned_process::Signals,
};
use std::{ffi::OsString, fs, os::unix::fs::PermissionsExt};

struct Runner<'a> {
    native: Native<'a>,
    calls: Vec<(Program, Vec<OsString>)>,
    replayed: Vec<u8>,
    replay_code: Option<u8>,
}

/// Inject while cc's completed copy is still in its open parent frame. The
/// destination directory's first sampled stamp can therefore include `extra`;
/// only independent complete membership verification rejects it before tools.
struct InjectExtra<E> {
    inner: E,
    directory: std::path::PathBuf,
    injected: bool,
}
impl<E: Executor> Executor for InjectExtra<E> {
    async fn run(&mut self, command: Command) -> Result<Vec<u8>> {
        self.inner.run(command).await
    }
    async fn ready(&mut self) -> Result<()> {
        if !self.injected
            && fs::metadata(self.directory.join("cc")).is_ok_and(|metadata| metadata.len() > 0)
        {
            fs::write(self.directory.join("extra"), b"unmeasured").unwrap();
            self.injected = true;
        }
        self.inner.ready().await
    }
    fn cleanup_passed(&self) -> bool {
        self.inner.cleanup_passed()
    }
}

/// This interleaving runs after destination bytes have been written but before
/// file sync/hash admission or the parent directory's first metadata sample.
struct InjectMode<E> {
    inner: E,
    trigger: std::path::PathBuf,
    selected: std::path::PathBuf,
    mode: u32,
    injected: bool,
}
impl<E: Executor> Executor for InjectMode<E> {
    async fn run(&mut self, command: Command) -> Result<Vec<u8>> {
        self.inner.run(command).await
    }
    async fn ready(&mut self) -> Result<()> {
        if !self.injected && fs::metadata(&self.trigger).is_ok_and(|value| value.len() > 0) {
            fs::set_permissions(&self.selected, fs::Permissions::from_mode(self.mode)).unwrap();
            self.injected = true;
        }
        self.inner.ready().await
    }
    fn cleanup_passed(&self) -> bool {
        self.inner.cleanup_passed()
    }
}

async fn mode_adoption(directory: bool, executable: bool, mode: u32) {
    let fixture = Fixture::new();
    let relative = "toolchain/lib/rustlib/x86_64-unknown-linux-musl/lib/fake.rlib";
    if executable {
        fs::set_permissions(
            fixture.kit.join(relative),
            fs::Permissions::from_mode(0o700),
        )
        .unwrap();
    }
    let budget = Budget::default();
    let request = fixture.request(&budget).await;
    let run = OwnedRun::fixture(&fixture.output, &budget).unwrap();
    let cwd = run.path.clone();
    let environment = recipe::Recipe::environment(&run).unwrap();
    let (_sender, signals) = signals();
    let clock = NativeClock;
    let deadline = Deadline::new(&clock);
    let runner = Runner {
        native: Native::new(&cwd, &environment, signals),
        calls: vec![],
        replayed: vec![],
        replay_code: None,
    };
    let trigger = run.path.join("kit").join(if directory {
        "host-tools/bin/cc"
    } else {
        relative
    });
    let selected = if directory {
        trigger.parent().unwrap().to_path_buf()
    } else {
        trigger.clone()
    };
    let mut runner = InjectMode {
        inner: runner,
        trigger,
        selected,
        mode,
        injected: false,
    };
    let result = execute::produce(&request, &run, &mut runner, &deadline).await;
    assert_eq!(result.unwrap_err().code, 1);
    assert!(runner.injected);
    assert_eq!(
        fs::metadata(&runner.selected).unwrap().permissions().mode() & 0o7777,
        mode
    );
    assert!(runner.inner.calls.is_empty());
    assert!(runner.cleanup_passed());
    assert!(!fixture.output.join("unsigned").exists());
    run.finish().unwrap();
    assert_eq!(budget.count(), 0);
}

#[tokio::test]
async fn staged_directory_mode_changes_before_first_stamp_start_no_cli() {
    for mode in [0o777, 0o1700] {
        mode_adoption(true, false, mode).await;
    }
}

#[tokio::test]
async fn staged_executable_and_data_modes_cannot_adopt_post_write_changes() {
    for (executable, mode) in [
        (true, 0o777),
        (true, 0o1700),
        (false, 0o666),
        (false, 0o1600),
    ] {
        mode_adoption(false, executable, mode).await;
    }
}
impl Executor for Runner<'_> {
    async fn run(&mut self, command: Command) -> Result<Vec<u8>> {
        assert!(matches!(
            command.program,
            Program::OpsCargo | Program::OpsRustc
        ));
        self.calls
            .push((command.program, command.arguments.clone()));
        self.native.run(command).await
    }
    async fn ready(&mut self) -> Result<()> {
        self.native.ready().await
    }
    fn cleanup_passed(&self) -> bool {
        self.native.cleanup_passed()
    }
    async fn replay(&mut self, bytes: &[u8], stderr: bool, _: Instant) -> Result<()> {
        assert!(!stderr);
        assert!(self.replayed.is_empty());
        self.replayed = bytes.to_vec();
        if let Some(code) = self.replay_code {
            Err(failure(code))
        } else {
            Ok(())
        }
    }
}
fn signals() -> (tokio::sync::mpsc::UnboundedSender<u8>, Signals) {
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    (sender, Signals::injected(receiver))
}
#[tokio::test]
async fn fixed_inert_tools_emit_a_closed_unsigned_bundle_without_running_candidate() {
    let fixture = Fixture::new();
    let budget = Budget::default();
    let request = fixture.request(&budget).await;
    let run = OwnedRun::fixture(&fixture.output, &budget).unwrap();
    let clock = NativeClock;
    let deadline = Deadline::new(&clock);
    let cwd = run.path.clone();
    let environment = recipe::Recipe::environment(&run).unwrap();
    let (_sender, signals) = signals();
    assert_eq!(
        environment[std::ffi::OsStr::new("CARGO_ENCODED_RUSTFLAGS")],
        recipe::FLAGS
    );
    assert!(!environment.contains_key(std::ffi::OsStr::new("RUSTFLAGS")));
    let mut runner = Runner {
        native: Native::new(&cwd, &environment, signals),
        calls: vec![],
        replayed: vec![],
        replay_code: None,
    };
    let observation = execute::produce(&request, &run, &mut runner, &deadline)
        .await
        .unwrap();
    assert_eq!(runner.calls.len(), 5);
    assert_eq!(runner.calls[4].0, Program::OpsCargo);
    assert_eq!(
        runner.calls[4]
            .1
            .iter()
            .map(|v| v.to_str().unwrap())
            .collect::<Vec<_>>(),
        vec![
            "build",
            "--frozen",
            "--release",
            "-p",
            "simplestchat-ops",
            "--bin",
            "simplestchat-ops",
            "--target",
            TARGET,
            "--manifest-path",
            run.path.join("source/Cargo.toml").to_str().unwrap(),
            "--target-dir",
            run.path.join("target").to_str().unwrap()
        ]
    );
    assert_eq!(
        fs::read(run.path.join("build.log")).unwrap(),
        b"PRIVATE-STDOUT\nPRIVATE-STDERR\n"
    );
    assert_eq!(
        fs::read(run.path.join("unsigned/simplestchat-ops")).unwrap(),
        binary()
    );
    assert_eq!(
        fs::metadata(run.path.join("unsigned/simplestchat-ops"))
            .unwrap()
            .permissions()
            .mode()
            & 0o111,
        0
    ); // metadata fixture is deliberately not an executable candidate
    execute::complete(Ok(observation), run, &mut runner, &deadline)
        .await
        .unwrap();
    assert_eq!(budget.count(), 0);
    let public = String::from_utf8(runner.replayed).unwrap();
    assert!(!public.contains("PRIVATE"));
    assert!(!public.contains(fixture.root.to_str().unwrap()));
    let manifest = simplestchat_ops::distribution::manifest::ArtifactManifest::read(
        &fixture.output.join("unsigned/ops-release.json"),
    )
    .unwrap();
    simplestchat_ops::distribution::manifest::verify_pair(
        &manifest,
        &fixture.output.join("unsigned/simplestchat-ops"),
        &request.selection,
    )
    .unwrap();
}
#[tokio::test]
async fn external_metadata_manifest_refuses_build_and_does_not_publish_unsigned_inventory() {
    let fixture = Fixture::new();
    let cargo = fixture.kit.join("toolchain/bin/cargo");
    let script = fs::read_to_string(&cargo).unwrap().replace(
        "'manifest_path':manifest",
        "'manifest_path':'/PRIVATE-SENTINEL/Cargo.toml'",
    );
    fs::write(cargo, script).unwrap();
    let budget = Budget::default();
    let request = fixture.request(&budget).await;
    let run = OwnedRun::fixture(&fixture.output, &budget).unwrap();
    let cwd = run.path.clone();
    let environment = recipe::Recipe::environment(&run).unwrap();
    let (_sender, signals) = signals();
    let clock = NativeClock;
    let deadline = Deadline::new(&clock);
    let mut runner = Runner {
        native: Native::new(&cwd, &environment, signals),
        calls: vec![],
        replayed: vec![],
        replay_code: None,
    };
    let result = execute::produce(&request, &run, &mut runner, &deadline).await;
    assert_eq!(runner.calls.len(), 4);
    assert_eq!(result.unwrap_err().code, 1);
    assert!(!fixture.output.join("unsigned").exists());
    run.finish().unwrap();
    assert_eq!(budget.count(), 0);
}
#[tokio::test]
async fn known_build_failure_preserves_status_and_settles_private_log() {
    let fixture = Fixture::new();
    let cargo = fixture.kit.join("toolchain/bin/cargo");
    let script = fs::read_to_string(&cargo).unwrap().replace(
        "os.write(2,b'PRIVATE-STDERR\\n')",
        "os.write(2,b'PRIVATE-STDERR\\n')\n    sys.exit(100)",
    );
    fs::write(cargo, script).unwrap();
    let budget = Budget::default();
    let request = fixture.request(&budget).await;
    let run = OwnedRun::fixture(&fixture.output, &budget).unwrap();
    let cwd = run.path.clone();
    let environment = recipe::Recipe::environment(&run).unwrap();
    let (_sender, signals) = signals();
    let clock = NativeClock;
    let deadline = Deadline::new(&clock);
    let mut runner = Runner {
        native: Native::new(&cwd, &environment, signals),
        calls: vec![],
        replayed: vec![],
        replay_code: None,
    };
    let result = execute::produce(&request, &run, &mut runner, &deadline).await;
    assert_eq!(result.unwrap_err().code, 100);
    assert!(runner.cleanup_passed());
    assert!(!fixture.output.join("unsigned").exists());
    run.finish().unwrap();
    assert_eq!(budget.count(), 0);
}
#[tokio::test]
async fn early_signal_admits_no_cli() {
    let fixture = Fixture::new();
    let budget = Budget::default();
    let request = fixture.request(&budget).await;
    let run = OwnedRun::fixture(&fixture.output, &budget).unwrap();
    let cwd = run.path.clone();
    let environment = recipe::Recipe::environment(&run).unwrap();
    let (sender, signals) = signals();
    sender.send(143).unwrap();
    let clock = NativeClock;
    let deadline = Deadline::new(&clock);
    let mut runner = Runner {
        native: Native::new(&cwd, &environment, signals),
        calls: vec![],
        replayed: vec![],
        replay_code: None,
    };
    let result = execute::produce(&request, &run, &mut runner, &deadline).await;
    assert_eq!(result.unwrap_err().code, 143);
    assert!(runner.calls.is_empty());
    run.finish().unwrap();
    assert_eq!(budget.count(), 0);
}

#[tokio::test]
async fn final_namespace_close_failure_prevents_public_replay() {
    let fixture = Fixture::new();
    let budget = Budget::default();
    let request = fixture.request(&budget).await;
    let run = OwnedRun::fixture(&fixture.output, &budget).unwrap();
    let cwd = run.path.clone();
    let environment = recipe::Recipe::environment(&run).unwrap();
    let (_sender, signals) = signals();
    let clock = NativeClock;
    let deadline = Deadline::new(&clock);
    let mut runner = Runner {
        native: Native::new(&cwd, &environment, signals),
        calls: vec![],
        replayed: vec![],
        replay_code: None,
    };
    let observation = execute::produce(&request, &run, &mut runner, &deadline)
        .await
        .unwrap();
    fs::set_permissions(&run.path, fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(
        execute::complete(Ok(observation), run, &mut runner, &deadline)
            .await
            .unwrap_err()
            .code,
        1
    );
    assert!(runner.replayed.is_empty());
    assert_eq!(budget.count(), 0);
}

#[tokio::test]
async fn extra_destination_entry_before_directory_stamp_starts_no_cli() {
    let fixture = Fixture::new();
    let budget = Budget::default();
    let request = fixture.request(&budget).await;
    let run = OwnedRun::fixture(&fixture.output, &budget).unwrap();
    let cwd = run.path.clone();
    let environment = recipe::Recipe::environment(&run).unwrap();
    let (_sender, signals) = signals();
    let clock = NativeClock;
    let deadline = Deadline::new(&clock);
    let runner = Runner {
        native: Native::new(&cwd, &environment, signals),
        calls: vec![],
        replayed: vec![],
        replay_code: None,
    };
    let directory = run.path.join("kit/host-tools/bin");
    let mut runner = InjectExtra {
        inner: runner,
        directory,
        injected: false,
    };
    let result = execute::produce(&request, &run, &mut runner, &deadline).await;
    assert_eq!(result.unwrap_err().code, 1);
    assert!(runner.injected);
    assert!(runner.directory.join("extra").is_file());
    assert!(runner.inner.calls.is_empty());
    assert!(runner.cleanup_passed());
    assert!(!fixture.output.join("unsigned").exists());
    run.finish().unwrap();
    assert_eq!(budget.count(), 0);
}
#[tokio::test]
async fn closed_result_replay_failure_and_expired_final_clock_cannot_return_zero() {
    for timeout in [false, true] {
        let fixture = Fixture::new();
        let budget = Budget::default();
        let request = fixture.request(&budget).await;
        let run = OwnedRun::fixture(&fixture.output, &budget).unwrap();
        let cwd = run.path.clone();
        let environment = recipe::Recipe::environment(&run).unwrap();
        let (_sender, signals) = signals();
        let clock = FakeClock::new();
        let deadline = Deadline::new(&clock);
        let mut runner = Runner {
            native: Native::new(&cwd, &environment, signals),
            calls: vec![],
            replayed: vec![],
            replay_code: Some(130),
        };
        let observation = execute::produce(&request, &run, &mut runner, &deadline)
            .await
            .unwrap();
        if timeout {
            clock.0.set(clock.now() + TOTAL);
        }
        let result = execute::complete(Ok(observation), run, &mut runner, &deadline).await;
        assert_eq!(result.unwrap_err().code, if timeout { 124 } else { 130 });
        assert_eq!(runner.replayed.is_empty(), timeout);
        assert_eq!(budget.count(), 0);
    }
}
