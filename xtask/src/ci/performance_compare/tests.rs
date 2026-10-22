//! Fixed profiles and owned fake command/filesystem integration; no builds run.

use super::*;
use crate::{
    ci::{
        command::{Command, Program},
        process::Executor,
    },
    tooling::Environment,
};
use std::{
    fs,
    io::Write,
    os::unix::{
        ffi::OsStringExt,
        fs::{DirBuilderExt, PermissionsExt, symlink},
    },
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

struct Fixture {
    path: PathBuf,
    root: PathBuf,
    temporary: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("comparison owned {}", files::nonce().unwrap()));
        fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        let root = path.join("candidate λ");
        let temporary = path.join("temporary λ");
        for path in [&root, &temporary, &path.join("tools")] {
            fs::create_dir(path).unwrap();
        }
        fs::create_dir_all(root.join("target/release")).unwrap();
        for name in ["git", "cargo"] {
            executable(&path.join("tools").join(name), b"inert tool");
        }
        Self {
            path,
            root,
            temporary,
        }
    }
    fn environment(&self) -> Environment {
        [
            ("GITHUB_ACTIONS", "true".into()),
            ("RUNNER_OS", "Linux".into()),
            ("RUNNER_TEMP", self.temporary.clone().into_os_string()),
            ("PATH", self.path.join("tools").into_os_string()),
            ("BASELINE_REF", "literal ref ; $(not-a-command)".into()),
            ("OPENSSL_DIR", "/literal OpenSSL".into()),
            ("OPENSSL_STATIC", "1".into()),
            ("AWS_LC_SYS_SYSTEM_DIR", "/literal AWS-LC".into()),
            ("PIP_CONSTRAINT", "/literal constraints".into()),
        ]
        .into_iter()
        .map(|(k, v)| (k.into(), v))
        .collect()
    }
    fn plan(&self) -> plan::Plan {
        plan::Plan::parse(&self.root, self.environment(), "linux").unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}
fn executable(path: &Path, bytes: &[u8]) {
    fs::write(path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}
#[derive(Default)]
struct FakeClock(AtomicU64);
impl Clock for FakeClock {
    fn elapsed(&self) -> Duration {
        Duration::from_secs(self.0.load(Ordering::SeqCst))
    }
}
#[derive(Debug)]
struct Call {
    stage: String,
    args: Vec<String>,
    cwd: PathBuf,
    environment: Environment,
    timeout: Duration,
    grace: Duration,
    cleanup: bool,
}
struct Fake<'a> {
    plan: &'a plan::Plan,
    clock: &'a FakeClock,
    calls: Vec<Call>,
    registered: bool,
    fail: Option<&'static str>,
    failed: bool,
    code: u8,
    unknown: bool,
    bad_receipt: bool,
    after_build: Option<u64>,
    foreign: bool,
    removed: Arc<AtomicBool>,
    removal_unknown: bool,
    jump_ready_after_calls: Option<(usize, u64)>,
}
impl<'a> Fake<'a> {
    fn new(plan: &'a plan::Plan, clock: &'a FakeClock) -> Self {
        Self {
            plan,
            clock,
            calls: Vec::new(),
            registered: false,
            fail: None,
            failed: false,
            code: 42,
            unknown: false,
            bad_receipt: false,
            after_build: None,
            foreign: false,
            removed: Arc::new(AtomicBool::new(false)),
            removal_unknown: false,
            jump_ready_after_calls: None,
        }
    }
}
impl Executor for Fake<'_> {
    async fn ready(&mut self) -> Result<()> {
        if let Some((calls, seconds)) = self.jump_ready_after_calls
            && self.calls.len() == calls
        {
            self.jump_ready_after_calls = None;
            self.clock.0.store(seconds, Ordering::SeqCst);
        }
        Ok(())
    }
    fn cleanup_passed(&self) -> bool {
        !self.unknown
    }
    async fn run(&mut self, mut command: Command) -> Result<Vec<u8>> {
        assert!(command.capture.is_none() && command.stdout.is_none());
        let role = match command.program {
            Program::ComparisonGit => files::Role::Git,
            Program::ComparisonCargo => files::Role::Cargo,
            Program::ComparisonBenchmark => files::Role::Benchmark,
            _ => panic!("unexpected role"),
        };
        let invocation = command.comparison.as_ref().unwrap();
        let (cwd, environment) = invocation.context(role).unwrap();
        let cwd = cwd.to_owned();
        let environment = environment.clone();
        let args = command
            .arguments
            .iter()
            .map(|v| v.to_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        let mut output = Vec::new();
        let stage = match role {
            files::Role::Git => match args[0].as_str() {
                "rev-parse" => {
                    output = format!("{}\n", "b".repeat(40)).into();
                    "resolve"
                }
                "worktree" => match args[1].as_str() {
                    "add" => {
                        let entries = fs::read_dir(&self.plan.temporary)
                            .unwrap()
                            .filter_map(|v| v.ok())
                            .filter(|v| {
                                v.file_name()
                                    .to_string_lossy()
                                    .starts_with("performance-compare.")
                            })
                            .collect::<Vec<_>>();
                        assert_eq!(entries.len(), 1);
                        assert!(entries[0].path().join("ownership.json").is_file());
                        fs::create_dir(self.plan.baseline.join("vendor")).unwrap();
                        fs::write(self.plan.baseline.join("vendor/input"), b"native").unwrap();
                        self.registered = true;
                        "add"
                    }
                    "list" => {
                        output = format!(
                            "worktree {}\0HEAD {}\0branch refs/heads/main\0\0",
                            self.plan.root.display(),
                            "c".repeat(40)
                        )
                        .into();
                        if self.registered {
                            output.extend(
                                format!(
                                    "worktree {}\0HEAD {}\0detached\0\0",
                                    self.plan.baseline.display(),
                                    if self.foreign {
                                        "e".repeat(40)
                                    } else {
                                        "b".repeat(40)
                                    }
                                )
                                .as_bytes(),
                            );
                        }
                        "list"
                    }
                    "remove" => {
                        fs::remove_dir_all(&self.plan.baseline).unwrap();
                        self.registered = false;
                        if !self.removal_unknown {
                            self.removed.store(true, Ordering::SeqCst);
                        }
                        "remove"
                    }
                    _ => panic!("unknown worktree operation"),
                },
                _ => panic!("unknown Git operation"),
            },
            files::Role::Cargo => {
                if cwd == self.plan.baseline {
                    assert_eq!(
                        environment.get(std::ffi::OsStr::new("CARGO_TARGET_DIR")),
                        Some(&self.plan.target.clone().into_os_string())
                    );
                    executable(&self.plan.candidate_binary(), b"baseline binary");
                    "baseline"
                } else if args.last().unwrap() == "load_test" {
                    executable(&self.plan.generator(), b"inert generator");
                    if let Some(seconds) = self.after_build {
                        self.clock.0.store(seconds, Ordering::SeqCst);
                    }
                    "generator"
                } else {
                    assert!(!self.plan.candidate_binary().exists());
                    assert_eq!(
                        fs::read(self.plan.baseline_binary()).unwrap(),
                        b"baseline binary"
                    );
                    executable(&self.plan.candidate_binary(), b"candidate binary");
                    "candidate"
                }
            }
            files::Role::Benchmark => {
                assert_eq!(
                    fs::read(self.plan.baseline_binary()).unwrap(),
                    b"baseline binary"
                );
                fs::DirBuilder::new()
                    .mode(0o700)
                    .create(&self.plan.comparison)
                    .unwrap();
                let run_id = environment[std::ffi::OsStr::new("BENCH_CI_RUN_ID")]
                    .to_str()
                    .unwrap();
                let receipt = serde_json::json!({"schemaVersion":1,"runId":run_id,
                    "completed":true,"passed":true,"runs":6,
                    "groupCleanupPassed":!self.bad_receipt,"streamsClosed":true});
                let file = self.plan.comparison.join("comparison-owner.json");
                fs::write(&file, serde_json::to_vec(&receipt).unwrap()).unwrap();
                fs::set_permissions(file, fs::Permissions::from_mode(0o600)).unwrap();
                "compare"
            }
        }
        .to_owned();
        self.calls.push(Call {
            stage: stage.clone(),
            args,
            cwd,
            environment,
            timeout: command.timeout,
            grace: command.grace,
            cleanup: command.cleanup,
        });
        let log = Arc::try_unwrap(command.combined_log.take().unwrap())
            .ok()
            .unwrap();
        let mut writer = log.writer()?;
        writer.write_all(&output).unwrap();
        files::close(writer)?;
        assert!(log.finish().await?.complete);
        if self.fail == Some(stage.as_str()) && !self.failed {
            self.failed = true;
            if self.foreign {
                fs::rename(
                    &self.plan.baseline,
                    self.plan.temporary.join("renamed-owned"),
                )
                .unwrap();
                fs::create_dir(&self.plan.baseline).unwrap();
                fs::write(self.plan.baseline.join("foreign"), b"keep foreign").unwrap();
            }
            return Err(plan::failure(self.code));
        }
        Ok(Vec::new())
    }
}

async fn run_fake(plan: &plan::Plan, fake: &mut Fake<'_>, clock: &FakeClock) -> Result<()> {
    let witness = fake.removed.clone();
    execute::execute_with(plan, fake, clock, |scope| {
        scope.removal_witness(witness.clone())
    })
    .await
}

#[test]
fn defaults_are_the_existing_ten_client_conference() {
    let profile = plan::Profile::parse(&Default::default()).unwrap();
    assert_eq!(profile.clients, 10);
    assert_eq!(profile.workers, 1);
    assert_eq!(profile.scenario(), "conference");
    assert_eq!(profile.ramp(), "5");
}

#[tokio::test]
async fn complete_fake_operation_preserves_order_cwd_environment_copy_and_retirement() {
    let fixture = Fixture::new();
    let plan = fixture.plan();
    let clock = FakeClock::default();
    let mut fake = Fake::new(&plan, &clock);
    run_fake(&plan, &mut fake, &clock).await.unwrap();
    assert_eq!(
        fake.calls
            .iter()
            .map(|v| v.stage.as_str())
            .collect::<Vec<_>>(),
        [
            "resolve",
            "add",
            "list",
            "baseline",
            "candidate",
            "generator",
            "compare",
            "list",
            "remove",
            "list"
        ]
    );
    assert!(!plan.baseline.exists());
    assert_eq!(
        fs::read(plan.candidate_binary()).unwrap(),
        b"candidate binary"
    );
    let baseline = &fake.calls[3];
    assert_eq!(baseline.cwd, plan.baseline);
    assert_eq!(
        baseline.args,
        ["build", "--locked", "--release", "--bin", "simplestChat"]
    );
    for call in &fake.calls {
        assert_eq!(
            call.environment.get(std::ffi::OsStr::new("OPENSSL_DIR")),
            plan.environment.get(std::ffi::OsStr::new("OPENSSL_DIR"))
        );
    }
    let compared = &fake.calls[6];
    assert_eq!(compared.args, plan.comparison_arguments());
    assert_eq!(compared.grace, Duration::from_secs(90));
    assert!(compared.timeout > plan.profile.minimum());
    assert!(fake.calls[7..].iter().all(|v| v.cleanup));
}

#[tokio::test]
async fn every_known_phase_failure_keeps_status_and_attempts_owned_worktree_cleanup() {
    for phase in [
        "resolve",
        "add",
        "baseline",
        "candidate",
        "generator",
        "compare",
        "remove",
    ] {
        let fixture = Fixture::new();
        let plan = fixture.plan();
        let clock = FakeClock::default();
        let mut fake = Fake::new(&plan, &clock);
        fake.fail = Some(phase);
        let error = run_fake(&plan, &mut fake, &clock).await.unwrap_err();
        assert_eq!(error.code, 42, "{phase}");
        assert!(!plan.baseline.exists(), "{phase}");
        if phase != "resolve" {
            assert!(fake.calls.iter().any(|v| v.stage == "remove"), "{phase}");
        }
    }
}

#[tokio::test]
async fn insufficient_known_six_run_budget_refuses_before_benchmark_dispatch() {
    for (clients, seconds) in [(10, 450), (30, 810), (100, 1632)] {
        let fixture = Fixture::new();
        let mut environment = fixture.environment();
        environment.insert("CLIENTS".into(), clients.to_string().into());
        let plan = plan::Plan::parse(&fixture.root, environment, "linux").unwrap();
        let clock = FakeClock::default();
        let mut fake = Fake::new(&plan, &clock);
        assert_eq!(plan.profile.minimum().as_secs(), seconds);
        fake.after_build = Some(
            (plan::WATCHDOG
                - plan::RESERVE
                - plan::BENCH_GRACE
                - plan::SETTLEMENT
                - plan.profile.minimum())
            .as_secs()
                + 1,
        );
        assert_eq!(
            run_fake(&plan, &mut fake, &clock).await.unwrap_err().code,
            124
        );
        assert!(!fake.calls.iter().any(|v| v.stage == "compare"));
        assert!(!plan.baseline.exists());
    }
}

#[tokio::test]
async fn failed_benchmark_still_observes_inner_receipt_without_erasing_known_status() {
    for code in [42, 124, 130, 143] {
        let fixture = Fixture::new();
        let plan = fixture.plan();
        let clock = FakeClock::default();
        let mut fake = Fake::new(&plan, &clock);
        fake.fail = Some("compare");
        fake.code = code;
        fake.bad_receipt = true;
        assert_eq!(
            run_fake(&plan, &mut fake, &clock).await.unwrap_err().code,
            code
        );
        let private = fs::read_dir(&plan.temporary)
            .unwrap()
            .map(|v| v.unwrap().path())
            .find(|v| {
                v.file_name()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .starts_with("performance-compare.")
            })
            .unwrap();
        let result: serde_json::Value =
            serde_json::from_slice(&fs::read(private.join("result.json")).unwrap()).unwrap();
        assert_eq!(result["innerCompletion"], false);
        assert_eq!(result["prepared"], false);
        assert!(!plan.baseline.exists());
    }
}

#[test]
fn metadata_readback_refuses_a_replacement_even_with_identical_private_shape() {
    let fixture = Fixture::new();
    let temporary = files::Scope::existing(&fixture.temporary).unwrap();
    let leaf = files::Scope::fresh(&temporary, "evidence").unwrap();
    let mut original = leaf.create("metadata.log").unwrap();
    original.write_all(b"original").unwrap();
    files::close(original).unwrap();
    fs::rename(
        leaf.path.join("metadata.log"),
        leaf.path.join("retained-original"),
    )
    .unwrap();
    fs::write(leaf.path.join("metadata.log"), b"decoy").unwrap();
    fs::set_permissions(
        leaf.path.join("metadata.log"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    assert!(leaf.read("metadata.log", 4096).is_err());
    files::finish(leaf, false).unwrap();
    files::finish(temporary, false).unwrap();
}

#[tokio::test]
async fn absent_name_without_positive_physical_removal_proof_never_passes() {
    let fixture = Fixture::new();
    let plan = fixture.plan();
    let clock = FakeClock::default();
    let mut fake = Fake::new(&plan, &clock);
    fake.removal_unknown = true;
    assert_eq!(
        run_fake(&plan, &mut fake, &clock).await.unwrap_err().code,
        1
    );
    assert!(!plan.baseline.exists());
    assert!(fake.calls.iter().any(|v| v.stage == "remove"));
}

#[tokio::test]
async fn readiness_yield_cannot_accept_success_after_active_or_final_deadline() {
    for (calls, elapsed) in [(7, plan::WATCHDOG - plan::RESERVE), (10, plan::WATCHDOG)] {
        let fixture = Fixture::new();
        let plan = fixture.plan();
        let clock = FakeClock::default();
        let mut fake = Fake::new(&plan, &clock);
        fake.jump_ready_after_calls = Some((calls, elapsed.as_secs()));
        assert_eq!(
            run_fake(&plan, &mut fake, &clock).await.unwrap_err().code,
            124
        );
        assert!(!plan.baseline.exists());
    }
}

#[tokio::test]
async fn invalid_inner_receipt_and_foreign_replacement_never_pass_or_delete_foreign_data() {
    let fixture = Fixture::new();
    let plan = fixture.plan();
    let clock = FakeClock::default();
    let mut fake = Fake::new(&plan, &clock);
    fake.bad_receipt = true;
    assert_eq!(
        run_fake(&plan, &mut fake, &clock).await.unwrap_err().code,
        1
    );
    assert!(!plan.baseline.exists());
    let fixture = Fixture::new();
    let plan = fixture.plan();
    let clock = FakeClock::default();
    let mut fake = Fake::new(&plan, &clock);
    fake.fail = Some("add");
    fake.foreign = true;
    assert_eq!(
        run_fake(&plan, &mut fake, &clock).await.unwrap_err().code,
        42
    );
    assert_eq!(
        fs::read(plan.baseline.join("foreign")).unwrap(),
        b"keep foreign"
    );
    assert!(plan.temporary.join("renamed-owned").is_dir());
    assert!(!fake.calls.iter().any(|v| v.stage == "remove"));
}

#[test]
fn closed_profiles_and_paths_refuse_lossy_or_unsupported_admission() {
    let fixture = Fixture::new();
    let mut env = fixture.environment();
    env.insert("CLIENTS".into(), "11".into());
    assert!(plan::Plan::parse(&fixture.root, env, "linux").is_err());
    let mut env = fixture.environment();
    env.insert(
        "RUNNER_TEMP".into(),
        std::ffi::OsString::from_vec(b"/tmp/invalid-\xff".to_vec()),
    );
    assert!(plan::Plan::parse(&fixture.root, env, "linux").is_err());
}

#[test]
fn complete_git_inventory_refuses_truncation_duplicate_and_contradictory_records() {
    let path = Path::new("/owned baseline");
    let revision = "b".repeat(40);
    let owned = format!("worktree {}\0HEAD {revision}\0detached\0\0", path.display());
    assert_eq!(
        worktree::listing(owned.as_bytes(), path, &revision).unwrap(),
        worktree::Registration::Owned
    );
    for invalid in [
        String::new(),
        format!("HEAD {revision}\0\0"),
        owned.replace("detached", "detached\0detached"),
        owned.replace("detached", "detached\0branch refs/heads/foreign"),
        owned.trim_end_matches('\0').into(),
        format!("{owned}{owned}"),
        format!("worktree /another\0HEAD garbage\0detached\0\0{owned}"),
        format!("worktree /another\0HEAD {revision}\0\0{owned}"),
    ] {
        assert!(worktree::listing(invalid.as_bytes(), path, &revision).is_err());
    }
}

#[test]
fn renamed_leaf_is_not_removal_and_vendor_touch_never_follows_links() {
    let fixture = Fixture::new();
    let temporary = files::Scope::existing(&fixture.temporary).unwrap();
    let leaf = files::Scope::fresh(&temporary, "baseline").unwrap();
    fs::create_dir(leaf.path.join("vendor")).unwrap();
    let outside = fixture.path.join("outside");
    fs::write(&outside, b"outside").unwrap();
    symlink(&outside, leaf.path.join("vendor/link")).unwrap();
    let before = fs::metadata(&outside).unwrap().modified().unwrap();
    worktree::vendor_times(&leaf).unwrap();
    assert_eq!(fs::metadata(outside).unwrap().modified().unwrap(), before);
    fs::rename(&leaf.path, fixture.temporary.join("renamed")).unwrap();
    assert!(leaf.remove_leaf().is_err());
    assert!(files::finish(leaf, true).is_err());
    files::finish(temporary, false).unwrap();
}
