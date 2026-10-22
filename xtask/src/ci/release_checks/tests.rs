//! Fixed-plan, read-only filesystem and inert owned-process coverage.

use super::{
    execute, files,
    plan::{self, Context},
};
use crate::{
    ci::{
        command::{Command, Program},
        process::Executor,
    },
    tooling::{Environment, Failure, Result},
};
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
        let context = Context::new(&root, environment, "Linux").unwrap();
        Self {
            root,
            context,
            node,
            chromium,
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
        let node = request.program == Program::Node;
        if node {
            let proof = request.node.take().unwrap();
            assert_eq!(
                proof.validate().unwrap(),
                self.fixture.node.canonicalize().unwrap()
            );
            drop(proof);
            assert_eq!(request.capture, Some(4096));
        } else {
            assert_eq!(request.program, Program::Python);
            assert!(request.capture.is_none());
        }
        self.commands.push(request);
        if (node && self.mode == "node_failure") || (!node && self.mode == "coordinator_failure") {
            return Err(Failure::new(
                if node { 42 } else { 17 },
                "Fixed fixture failure.",
            ));
        }
        if node {
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
            if self.mode == "browser_changed" {
                fs::write(&self.fixture.chromium, b"replaced Chromium").unwrap();
            }
            Ok(vec![])
        }
    }
}

#[tokio::test]
async fn exact_discovery_and_coordinator_arguments_keep_one_budget_and_inherited_output() {
    let f = Fixture::new();
    let mut fake = Fake::new(&f, "");
    execute::execute(&f.context, &mut fake).await.unwrap();
    assert_eq!(fake.commands.len(), 2);
    assert_eq!(fake.commands[0].arguments, ["-e", plan::EXPRESSION]);
    assert_eq!(
        plan::EXPRESSION,
        concat!(
            "process.stdout.write(require(\"./web/e2e/node_modules/",
            "playwright\").chromium.executablePath())",
        )
    );
    assert_eq!(
        fake.commands[1].arguments,
        vec![
            "-B".into(),
            "build/ci_release_checks.py".into(),
            "--controller".into(),
            f.context
                .temporary
                .join("release-container-controller/bin/python")
                .into_os_string(),
            "--node".into(),
            f.node.canonicalize().unwrap().into_os_string(),
            "--chromium".into(),
            f.chromium.canonicalize().unwrap().into_os_string(),
            "--image".into(),
            f.context.image.clone().into(),
            "--fixture-output".into(),
            f.context
                .temporary
                .join("release-container")
                .into_os_string(),
            "--output".into(),
            f.context.temporary.join("release-checks").into_os_string()
        ]
    );
    assert_eq!(plan::BUDGET, Duration::from_secs(660));
    assert!(fake.commands[0].timeout <= Duration::from_secs(30));
    assert!(fake.commands[1].timeout <= Duration::from_secs(640));
    assert!(fake.commands[1].timeout > Duration::from_secs(639));
    assert_eq!(fake.commands[1].grace, Duration::from_secs(15));
    assert!(
        !f.context.temporary.join("release-checks").exists(),
        "only coordinator creates its output"
    );
}

#[tokio::test]
async fn discovery_or_coordinator_failures_keep_status_and_never_start_a_later_stage() {
    for (mode, code, count) in [
        ("node_failure", 42, 1),
        ("coordinator_failure", 17, 2),
        ("cancelled", 143, 0),
        ("unclean", 1, 2),
    ] {
        let f = Fixture::new();
        let mut fake = Fake::new(&f, mode);
        assert_eq!(
            execute::execute(&f.context, &mut fake)
                .await
                .unwrap_err()
                .code,
            code,
            "{mode}"
        );
        assert_eq!(fake.commands.len(), count, "{mode}");
    }
}

#[tokio::test]
async fn invalid_returned_paths_and_tool_mutation_refuse_success_without_echoing_input() {
    for mode in [
        "relative",
        "newline",
        "utf8",
        "directory",
        "oversize",
        "empty",
        "node_changed",
        "browser_changed",
    ] {
        let f = Fixture::new();
        let mut fake = Fake::new(&f, mode);
        let error = execute::execute(&f.context, &mut fake).await.unwrap_err();
        assert!(!error.message.contains(f.root.to_str().unwrap()));
        assert_eq!(
            fake.commands.len(),
            if mode == "browser_changed" { 2 } else { 1 },
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
        let proof = files::Executable::open(&selected).unwrap();
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
    assert!(files::Executable::open(&f.chromium).is_err());
    executable(&f.chromium, b"empty");
    fs::write(&f.chromium, []).unwrap();
    assert!(files::Executable::open(&f.chromium).is_err());
}

#[test]
fn contexts_arguments_and_exhausted_budget_fail_before_any_workload() {
    let f = Fixture::new();
    for system in ["Darwin", "Windows"] {
        assert!(Context::new(&f.root, f.context.environment.clone(), system).is_err());
    }
    let mut env = f.context.environment.clone();
    env.insert(
        "PRODUCTION_IMAGE".into(),
        "simplestchat-ci:production".into(),
    );
    assert!(Context::new(&f.root, env, "Linux").is_err());
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
    assert!(Context::new(&f.root, env.clone(), "Linux").is_err());
    env.insert("LOCAL_CI_DISPOSABLE".into(), "1".into());
    Context::new(&f.root, env, "Linux").unwrap();
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
