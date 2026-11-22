//! Fixed-plan, private-output and inert owned-process coverage of the
//! overlapped release fixture and scanner download.

use super::{
    execute, files,
    plan::{self, Context},
};
use crate::{
    command::{Command, Program},
    process::Executor,
};
use simplestchat_toolkit::tooling::{Environment, Failure, Result};
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

#[path = "native_tests.rs"]
mod native;

struct Fixture {
    root: PathBuf,
    context: Context,
    node: PathBuf,
    chromium: PathBuf,
    sudo: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "release checks {} {} $(literal)",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let temporary = root.join("runner temp");
        fs::create_dir(&temporary).unwrap();
        let bin = root.join("commands");
        fs::create_dir(&bin).unwrap();
        let node = bin.join("node");
        executable(&node, b"fixture Node");
        let sudo = bin.join("sudo");
        executable(&sudo, b"inert sudo");
        executable(&bin.join("cargo"), b"inert cargo");
        let chromium = root.join("browser with spaces");
        executable(&chromium, b"fixture Chromium");
        let environment = [
            ("PATH".into(), bin.into_os_string()),
            ("RUNNER_TEMP".into(), temporary.into_os_string()),
            ("GITHUB_ACTIONS".into(), "true".into()),
            ("RUNNER_ENVIRONMENT".into(), "github-hosted".into()),
            (
                "PRODUCTION_IMAGE".into(),
                format!("sha256:{}", "a".repeat(64)).into(),
            ),
        ]
        .into_iter()
        .collect::<Environment>();
        let http_policy = root.join("feature xtask");
        executable(&http_policy, b"feature checker fixture");
        let context = Context::new(&root, environment, "Linux", &http_policy).unwrap();
        Self {
            root,
            context,
            node,
            chromium,
            sudo,
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}
fn executable(path: &Path, bytes: &[u8]) {
    fs::write(path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}
struct Fake<'a> {
    fixture: &'a Fixture,
    mode: &'static str,
    commands: Vec<Command>,
}
impl<'a> Fake<'a> {
    fn new(fixture: &'a Fixture, mode: &'static str) -> Self {
        Self {
            fixture,
            mode,
            commands: vec![],
        }
    }
}
impl Executor for Fake<'_> {
    async fn ready(&mut self) -> Result<()> {
        if self.mode == "cancelled" {
            Err(Failure::new(143, "Fixed fixture cancellation."))
        } else {
            Ok(())
        }
    }
    fn cleanup_passed(&self) -> bool {
        self.mode != "unclean"
    }
    async fn run(&mut self, mut request: Command) -> Result<Vec<u8>> {
        let program = request.program;
        match program {
            Program::Node => {
                let proof = request.node.take().unwrap();
                assert_eq!(
                    proof.validate().unwrap(),
                    self.fixture.node.canonicalize().unwrap()
                );
                drop(proof);
                assert_eq!(request.capture, Some(4096));
            }
            Program::Sudo => {
                let proof = request.release_checks.take().unwrap();
                proof.begin(&request).unwrap();
                assert_eq!(
                    proof.validate(request.program, &request.arguments).unwrap(),
                    self.fixture.sudo
                );
                drop(proof);
                assert!(request.capture.is_none() && request.separate_logs.is_some());
            }
            _ => {
                assert_eq!(program, Program::Cargo);
                assert!(request.release_checks.is_none() && request.separate_logs.is_some());
            }
        }
        request.separate_logs = None;
        self.commands.push(request);
        let failed = match program {
            Program::Node => (self.mode == "node_failure").then_some(42),
            Program::Sudo => matches!(self.mode, "fixture_failure" | "both_failure").then_some(17),
            _ => matches!(self.mode, "scanner_failure" | "both_failure").then_some(23),
        };
        if let Some(code) = failed {
            return Err(Failure::child_exit(code));
        }
        if program == Program::Node {
            let bytes = match self.mode {
                "relative" => b"browser".to_vec(),
                "newline" => format!("{}\n", self.fixture.chromium.display()).into_bytes(),
                "utf8" => vec![255],
                "directory" => self.fixture.root.as_os_str().as_encoded_bytes().to_vec(),
                "oversize" => vec![b'a'; 4097],
                "empty" => vec![],
                _ => self
                    .fixture
                    .chromium
                    .as_os_str()
                    .as_encoded_bytes()
                    .to_vec(),
            };
            if self.mode == "node_changed" {
                fs::write(&self.fixture.node, b"replaced Node").unwrap();
            }
            Ok(bytes)
        } else {
            if self.mode == "checker_changed" && program == Program::Sudo {
                fs::write(
                    &self.fixture.context.http_policy,
                    b"replacement feature binary",
                )
                .unwrap();
            }
            if self.mode == "browser_changed" && program == Program::Sudo {
                fs::write(&self.fixture.chromium, b"replaced Chromium").unwrap();
            }
            Ok(vec![])
        }
    }
}

fn run(f: &Fixture, mode: &'static str) -> (Result<()>, Vec<Command>) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut fake = Fake::new(f, mode);
    let mut scanner = Fake::new(f, mode);
    let result = runtime.block_on(execute::execute(&f.context, &mut fake, &mut scanner));
    let mut commands = fake.commands;
    commands.extend(scanner.commands);
    (result, commands)
}

#[test]
fn exact_discovery_fixture_and_scanner_arguments_keep_one_budget_and_private_logs() {
    let f = Fixture::new();
    let (result, commands) = run(&f, "");
    result.unwrap();
    assert_eq!(commands.len(), 3);
    assert_eq!(commands[0].arguments, ["-e", plan::EXPRESSION]);
    assert_eq!(
        plan::EXPRESSION,
        concat!(
            "process.stdout.write(require(\"./web/e2e/node_modules/",
            "playwright\").chromium.executablePath())",
        )
    );
    let temporary = &f.context.temporary;
    assert_eq!(
        commands[1].arguments,
        vec![
            f.context.http_policy.clone().into_os_string(),
            "release-fixture".into(),
            "--disposable-host".into(),
            "--image".into(),
            f.context.image.clone().into(),
            "--output".into(),
            temporary.join("release-container").into_os_string(),
            "--node".into(),
            f.node.canonicalize().unwrap().into_os_string(),
            "--chromium".into(),
            f.chromium.canonicalize().unwrap().into_os_string(),
            "--http-policy-binary".into(),
            f.context.http_policy.clone().into_os_string(),
            "--ansible".into(),
            temporary
                .join("release-container-controller/bin/ansible-playbook")
                .into_os_string(),
        ]
    );
    let output = temporary.join("release-checks");
    let mut scanner: Vec<std::ffi::OsString> = [
        "run",
        "--locked",
        "-q",
        "-p",
        "simplestchat-security",
        "--",
        "tools",
        "install",
        "--tools",
        "syft",
        "grype",
        "gitleaks",
        "--platform",
        "linux-x86_64",
        "--directory",
    ]
    .map(Into::into)
    .into();
    scanner.push(output.join("tools").into_os_string());
    assert_eq!(commands[2].arguments, scanner);
    assert_eq!(plan::BUDGET, Duration::from_secs(1140));
    assert!(commands[0].timeout <= Duration::from_secs(30));
    assert_eq!(commands[1].timeout, Duration::from_secs(850));
    assert_eq!(commands[1].grace, Duration::from_secs(200));
    assert_eq!(commands[2].timeout, Duration::from_secs(300));
    assert_eq!(commands[2].grace, Duration::from_secs(5));
    let mode = |path: &Path| {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    };
    assert_eq!(mode(&output), 0o700);
    for name in [
        "summary.json",
        "release-fixture.stdout",
        "release-fixture.stderr",
        "scanner-tools.stdout",
        "scanner-tools.stderr",
    ] {
        assert_eq!(mode(&output.join(name)), 0o600, "{name}");
    }
    let summary: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("summary.json")).unwrap()).unwrap();
    assert_eq!(summary["passed"], true);
    assert_eq!(summary["checks"][0]["name"], "release-fixture");
    assert_eq!(summary["checks"][1]["name"], "scanner-tools");
    assert_eq!(summary["checks"][1]["status"], 0);
}

#[test]
fn each_overlapped_failure_keeps_its_own_status_and_never_hides_the_other() {
    for (mode, code, count, passed) in [
        ("node_failure", 42, 1, None),
        ("fixture_failure", 17, 3, Some([Some(17), Some(0)])),
        ("scanner_failure", 23, 3, Some([Some(0), Some(23)])),
        ("both_failure", 17, 3, Some([Some(17), Some(23)])),
        ("cancelled", 143, 0, None),
        ("unclean", 1, 3, Some([Some(0), Some(0)])),
    ] {
        let f = Fixture::new();
        let (result, commands) = run(&f, mode);
        assert_eq!(result.unwrap_err().code, code, "{mode}");
        assert_eq!(commands.len(), count, "{mode}");
        let summary = f.context.temporary.join("release-checks/summary.json");
        match passed {
            Some(statuses) => {
                let summary: serde_json::Value =
                    serde_json::from_slice(&fs::read(summary).unwrap()).unwrap();
                assert_eq!(summary["passed"], statuses == [Some(0), Some(0)], "{mode}");
                for (index, status) in statuses.iter().enumerate() {
                    assert_eq!(
                        summary["checks"][index]["status"],
                        serde_json::json!(status),
                        "{mode}"
                    );
                }
            }
            None => assert!(!summary.exists(), "{mode}"),
        }
    }
}

#[test]
fn an_existing_output_directory_refuses_before_either_command_starts() {
    let f = Fixture::new();
    fs::create_dir(f.context.temporary.join("release-checks")).unwrap();
    let (result, commands) = run(&f, "");
    assert!(result.is_err());
    assert_eq!(commands.len(), 1);
}

#[test]
fn invalid_returned_paths_and_tool_mutation_refuse_success_without_echoing_input() {
    for mode in [
        "relative",
        "newline",
        "utf8",
        "directory",
        "oversize",
        "empty",
        "node_changed",
        "browser_changed",
        "checker_changed",
    ] {
        let f = Fixture::new();
        let (result, commands) = run(&f, mode);
        let error = result.unwrap_err();
        assert!(!error.message.contains(f.root.to_str().unwrap()));
        assert_eq!(
            commands.len(),
            if matches!(mode, "browser_changed" | "checker_changed") {
                3
            } else {
                1
            },
            "{mode}"
        );
    }
}

#[test]
fn path_selection_keeps_first_executable_symlink_relative_and_empty_entries() {
    let f = Fixture::new();
    let first = f.root.join("first");
    let second = f.root.join("second");
    fs::create_dir(&first).unwrap();
    fs::create_dir(&second).unwrap();
    fs::write(first.join("node"), b"nonexecutable").unwrap();
    executable(&second.join("node"), b"second executable");
    let mut env = f.context.environment.clone();
    env.insert(
        "PATH".into(),
        std::env::join_paths([&first, &second, f.node.parent().unwrap()]).unwrap(),
    );
    let proof = files::node(&f.root, &env).unwrap();
    assert_eq!(proof.validate().unwrap(), second.join("node"));
    proof.close().unwrap();
    fs::remove_file(first.join("node")).unwrap();
    symlink(&f.node, first.join("node")).unwrap();
    let proof = files::node(&f.root, &env).unwrap();
    assert_eq!(proof.validate().unwrap(), f.node);
    proof.close().unwrap();
    env.insert("PATH".into(), "missing:commands".into());
    let proof = files::node(&f.root, &env).unwrap();
    assert_eq!(proof.validate().unwrap(), f.node);
    proof.close().unwrap();
    executable(&f.root.join("node"), b"empty entry executable");
    env.insert("PATH".into(), ":commands".into());
    let proof = files::node(&f.root, &env).unwrap();
    assert_eq!(proof.validate().unwrap(), f.root.join("node"));
    proof.close().unwrap();
    env.insert("PATH".into(), first.join("absent").into_os_string());
    assert_eq!(files::node(&f.root, &env).unwrap_err().code, 127);
}

#[test]
fn retained_tool_identity_rejects_mutation_replacement_and_retargeted_symlinks() {
    for mode in ["mutation", "replacement", "link"] {
        let f = Fixture::new();
        let selected = f.root.join("selected");
        symlink(&f.node, &selected).unwrap();
        let proof = simplestchat_toolkit::executable::Executable::open(&selected).unwrap();
        match mode {
            "mutation" => fs::write(&f.node, b"changed").unwrap(),
            "replacement" => {
                fs::remove_file(&f.node).unwrap();
                executable(&f.node, b"replaced");
            }
            _ => {
                fs::remove_file(&selected).unwrap();
                symlink(&f.chromium, &selected).unwrap();
            }
        }
        assert!(proof.validate().is_err(), "{mode}");
        proof.close().unwrap();
    }
    let f = Fixture::new();
    fs::set_permissions(&f.chromium, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(simplestchat_toolkit::executable::Executable::open(&f.chromium).is_err());
    executable(&f.chromium, b"empty");
    fs::write(&f.chromium, []).unwrap();
    assert!(simplestchat_toolkit::executable::Executable::open(&f.chromium).is_err());
}

#[test]
fn contexts_arguments_and_exhausted_budget_fail_before_any_workload() {
    let f = Fixture::new();
    for system in ["Darwin", "Windows"] {
        assert!(
            Context::new(
                &f.root,
                f.context.environment.clone(),
                system,
                &f.context.http_policy
            )
            .is_err()
        );
    }
    let mut env = f.context.environment.clone();
    env.insert(
        "PRODUCTION_IMAGE".into(),
        "simplestchat-ci:production".into(),
    );
    assert!(Context::new(&f.root, env, "Linux", &f.context.http_policy).is_err());
    let proof = Arc::new(files::node(&f.root, &f.context.environment).unwrap());
    assert_eq!(
        plan::discover(Arc::clone(&proof), Instant::now() + Duration::from_secs(19))
            .unwrap_err()
            .code,
        124
    );
    Arc::try_unwrap(proof).unwrap().close().unwrap();
    assert_eq!(
        super::run(&f.root, &["--help".into()]).unwrap(),
        std::process::ExitCode::SUCCESS
    );
    assert_eq!(
        super::run(&f.root, &["--bad".into()]).unwrap(),
        std::process::ExitCode::from(2)
    );
    let mut env = f.context.environment.clone();
    env.insert("ACT".into(), "true".into());
    assert!(Context::new(&f.root, env.clone(), "Linux", &f.context.http_policy).is_err());
    env.insert("LOCAL_CI_DISPOSABLE".into(), "1".into());
    Context::new(&f.root, env, "Linux", &f.context.http_policy).unwrap();
}

#[test]
fn metadata_only_proof_accepts_large_regular_browser_without_reading_or_allocating_its_contents() {
    let f = Fixture::new();
    // Sparse owned fixture: admission has no scanner-derived content-size cap.
    fs::OpenOptions::new()
        .write(true)
        .open(&f.chromium)
        .unwrap()
        .set_len(257 * 1024 * 1024)
        .unwrap();
    let proof = files::chromium(f.chromium.as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(proof.validate().unwrap(), f.chromium);
    proof.close().unwrap();
}
