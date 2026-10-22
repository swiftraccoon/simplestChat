//! Actual policy, filesystem admission and fake-owned process fixtures.

use super::{
    execute, files,
    plan::{self, Context},
};
use crate::{
    ci::{
        command::{Command, Program},
        process::Executor,
        selected_executable::Executable,
    },
    tooling::{Environment, Failure, Result},
};
use serde_json::Value;
use std::{
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt, symlink},
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
}
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "audit fixture {} {} $(literal)",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        let temporary = root.join("runner temp");
        fs::create_dir(&temporary).unwrap();
        let environment = [
            ("GITHUB_ACTIONS".into(), "true".into()),
            ("RUNNER_ENVIRONMENT".into(), "github-hosted".into()),
            ("RUNNER_TEMP".into(), temporary.into_os_string()),
        ]
        .into_iter()
        .collect::<Environment>();
        let context = Context::new(&root, environment, "Linux").unwrap();
        Self { root, context }
    }
    fn staging(&self) -> Vec<PathBuf> {
        fs::read_dir(&self.context.temporary)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|path| path.is_dir())
            .collect()
    }
    fn report(&self) -> Value {
        let reports = fs::read_dir(&self.context.temporary)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.is_file())
            .collect::<Vec<_>>();
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].metadata().unwrap().mode() & 0o777, 0o600);
        let bytes = fs::read(&reports[0]).unwrap();
        let text = String::from_utf8(bytes.clone()).unwrap();
        assert!(!text.contains("PRIVATE-SENTINEL"));
        assert!(!text.contains(self.root.to_str().unwrap()));
        serde_json::from_slice(&bytes).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}
fn scanner(path: &Path) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, b"#!/usr/bin/python3\nprint('fixture only')\n").unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

struct Fake {
    commands: Vec<Command>,
    mode: &'static str,
    tools: Option<PathBuf>,
}
impl Fake {
    fn new(mode: &'static str) -> Self {
        Self {
            commands: vec![],
            mode,
            tools: None,
        }
    }
    fn audits(&self) -> usize {
        self.commands
            .iter()
            .filter(|command| command.program == Program::CargoAudit)
            .count()
    }
}
impl Executor for Fake {
    async fn ready(&mut self) -> Result<()> {
        Ok(())
    }
    fn cleanup_passed(&self) -> bool {
        self.mode != "unproven"
    }
    async fn run(&mut self, mut command: Command) -> Result<Vec<u8>> {
        let args = command
            .arguments
            .iter()
            .map(|s| s.to_str().unwrap())
            .collect::<Vec<_>>();
        let stage = if command.program == Program::CargoAudit {
            "audit"
        } else if args[1] == "install" {
            "install"
        } else {
            "resolve"
        };
        let mut output = Vec::new();
        if stage == "install" {
            let tools = PathBuf::from(args.last().unwrap());
            let prefix = tools.parent().unwrap();
            assert_eq!(prefix.metadata().unwrap().mode() & 0o777, 0o700);
            assert!(prefix.join("intent.json").is_file());
            scanner(&tools.join("bin/cargo-audit"));
            self.tools = Some(tools);
        } else if stage == "resolve" {
            let tools = self.tools.as_ref().unwrap();
            let path = tools.join("bin/cargo-audit");
            output = format!("{}\n\n", path.display()).into_bytes();
            match self.mode {
                "outside" => output = b"/PRIVATE-SENTINEL/cargo-audit\n".to_vec(),
                "newline" => output.extend_from_slice(b"PRIVATE-SENTINEL\n"),
                "utf8" => output = vec![255],
                "empty" => fs::write(&path, []).unwrap(),
                "oversize" => fs::OpenOptions::new()
                    .write(true)
                    .open(&path)
                    .unwrap()
                    .set_len(256 * 1024 * 1024 + 1)
                    .unwrap(),
                "nonexec" => fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap(),
                "writable" => {
                    fs::set_permissions(&path, fs::Permissions::from_mode(0o722)).unwrap()
                }
                "hardlink" => fs::hard_link(&path, tools.join("other")).unwrap(),
                "link" => {
                    fs::remove_file(&path).unwrap();
                    symlink("/PRIVATE-SENTINEL", &path).unwrap();
                }
                "directory_link" => {
                    fs::rename(tools.join("bin"), tools.join("original")).unwrap();
                    symlink("original", tools.join("bin")).unwrap();
                }
                "loose_prefix" => {
                    fs::set_permissions(tools.parent().unwrap(), fs::Permissions::from_mode(0o755))
                        .unwrap()
                }
                "deep" => {
                    let mut path = tools.clone();
                    for _ in 0..18 {
                        path.push("nested");
                        fs::create_dir(&path).unwrap();
                    }
                }
                _ => {}
            }
        } else {
            let executable = command.executable.take().unwrap();
            executable.validate()?;
            drop(executable);
            assert!(command.capture.is_none());
            if self.mode == "changed_after_audit" {
                fs::write(
                    self.tools.as_ref().unwrap().join("bin/cargo-audit"),
                    b"changed",
                )
                .unwrap();
            }
        }
        self.commands.push(command);
        if self.mode == stage {
            Err(Failure::new(42, "Fixed fixture failure."))
        } else {
            Ok(output)
        }
    }
}

#[test]
fn exact_reference_commands_advisory_exclusions_and_one_deadline_are_retained() {
    let fixture = Fixture::new();
    let tools = fixture.root.join("tools with spaces $(literal)");
    let deadline = Instant::now() + plan::BUDGET;
    for (install, expected) in [
        (
            true,
            vec![
                "build/security_tools.py",
                "install",
                "--tools",
                "cargo-audit",
                "--directory",
            ],
        ),
        (
            false,
            vec![
                "build/security_tools.py",
                "path",
                "cargo-audit",
                "--directory",
            ],
        ),
    ] {
        let command = plan::reference(&tools, install, deadline).unwrap();
        assert_eq!(command.program, Program::Python);
        assert_eq!(command.arguments[..command.arguments.len() - 1], expected);
        assert_eq!(command.arguments.last().unwrap(), tools.as_os_str());
        assert_eq!(command.capture, Some(4096));
        assert!(command.timeout <= Duration::from_secs(1800));
        assert!(command.timeout > Duration::from_secs(1799));
    }
    let expired = Instant::now() - Duration::from_secs(1);
    assert_eq!(
        plan::reference(&tools, true, expired).unwrap_err().code,
        124
    );
    assert_eq!(plan::BUDGET, Duration::from_secs(1800));
}

#[tokio::test]
async fn successful_audit_uses_validated_path_with_inherited_output_and_retires_everything() {
    let fixture = Fixture::new();
    let mut fake = Fake::new("");
    execute::execute(&fixture.context, &mut fake).await.unwrap();
    assert_eq!(fake.commands.len(), 3);
    assert_eq!(
        fake.commands[2].arguments,
        [
            "audit",
            "--deny",
            "unsound",
            "--ignore",
            "RUSTSEC-2021-0145",
            "--ignore",
            "RUSTSEC-2026-0097"
        ]
    );
    assert!(fixture.staging().is_empty());
    let report = fixture.report();
    for field in [
        "passed",
        "installerCompleted",
        "executableVerified",
        "auditCompleted",
        "executableClosed",
        "processCleanupPassed",
        "namespaceCleanupPassed",
        "cleanupPassed",
    ] {
        assert_eq!(report[field], true, "{field}");
    }
    assert_eq!(report["exitCode"], 0);
    assert!(report["failure"].is_null());
}

#[tokio::test]
async fn each_reference_failure_preserves_status_and_still_closes_and_cleans() {
    for stage in ["install", "resolve", "audit"] {
        let fixture = Fixture::new();
        let mut fake = Fake::new(stage);
        let error = execute::execute(&fixture.context, &mut fake)
            .await
            .unwrap_err();
        assert_eq!(error.code, 42);
        assert!(fixture.staging().is_empty());
        let report = fixture.report();
        assert_eq!(report["passed"], false);
        assert_eq!(report["failure"], stage);
        assert_eq!(report["exitCode"], 42);
        assert_eq!(report["cleanupPassed"], true);
    }
}

#[tokio::test]
async fn untrusted_paths_and_unstable_or_unsafe_scanners_never_reach_execution() {
    for mode in [
        "outside",
        "newline",
        "utf8",
        "empty",
        "oversize",
        "nonexec",
        "writable",
        "hardlink",
        "link",
        "directory_link",
        "loose_prefix",
    ] {
        let fixture = Fixture::new();
        let mut fake = Fake::new(mode);
        assert!(
            execute::execute(&fixture.context, &mut fake).await.is_err(),
            "{mode}"
        );
        assert_eq!(fake.audits(), 0, "{mode}");
        let report = fixture.report();
        assert_eq!(report["passed"], false, "{mode}");
        assert!(report["failure"] == "validate" || report["failure"] == "resolve");
        assert_eq!(report["executableClosed"], true, "{mode}");
    }
}

#[tokio::test]
async fn changed_scanner_or_unproven_cleanup_cannot_publish_success() {
    for mode in ["changed_after_audit", "unproven", "deep"] {
        let fixture = Fixture::new();
        let mut fake = Fake::new(mode);
        assert!(execute::execute(&fixture.context, &mut fake).await.is_err());
        let report = fixture.report();
        assert_eq!(report["passed"], false, "{mode}");
        if mode == "unproven" {
            assert_eq!(report["processCleanupPassed"], false);
        }
        if mode == "deep" {
            assert_eq!(report["namespaceCleanupPassed"], false);
        }
    }
}

#[test]
fn private_namespace_is_unique_and_link_cleanup_never_touches_an_external_target() {
    let fixture = Fixture::new();
    let first = files::Owned::new(&fixture.context).unwrap();
    let second = files::Owned::new(&fixture.context).unwrap();
    assert_ne!(first.path, second.path);
    let target = fixture.root.join("external");
    fs::create_dir(&target).unwrap();
    fs::write(target.join("PRIVATE-SENTINEL"), b"retained").unwrap();
    symlink(&target, first.path.join("foreign-link")).unwrap();
    for owned in [first, second] {
        let (reporter, result) = owned.cleanup();
        result.unwrap();
        reporter.write(&serde_json::json!({"passed":true})).unwrap();
    }
    assert_eq!(
        fs::read(target.join("PRIVATE-SENTINEL")).unwrap(),
        b"retained"
    );
    assert!(fixture.staging().is_empty());
}

#[test]
fn report_is_written_to_retained_parent_after_rename_and_foreign_replacement_is_untouched() {
    let fixture = Fixture::new();
    let owned = files::Owned::new(&fixture.context).unwrap();
    let original = fixture.root.join("original runner");
    fs::rename(&fixture.context.temporary, &original).unwrap();
    fs::create_dir(&fixture.context.temporary).unwrap();
    fs::write(
        fixture.context.temporary.join("PRIVATE-SENTINEL"),
        b"foreign",
    )
    .unwrap();
    let (reporter, result) = owned.cleanup();
    result.unwrap();
    reporter
        .write(&serde_json::json!({"passed":false}))
        .unwrap();
    assert_eq!(fs::read_dir(&original).unwrap().count(), 1);
    assert_eq!(fs::read_dir(&fixture.context.temporary).unwrap().count(), 1);
}

#[test]
fn exact_host_context_and_cli_are_checked_before_any_process_or_namespace() {
    let fixture = Fixture::new();
    for system in ["Darwin", "Windows"] {
        assert_eq!(
            Context::new(&fixture.root, fixture.context.environment.clone(), system)
                .err()
                .unwrap()
                .code,
            2
        );
    }
    let mut env = fixture.context.environment.clone();
    env.remove(std::ffi::OsStr::new("RUNNER_ENVIRONMENT"));
    assert!(Context::new(&fixture.root, env.clone(), "Linux").is_err());
    env.insert("ACT".into(), "true".into());
    assert!(Context::new(&fixture.root, env.clone(), "Linux").is_err());
    env.insert("LOCAL_CI_DISPOSABLE".into(), "1".into());
    Context::new(&fixture.root, env, "Linux").unwrap();
    assert_eq!(
        super::run(&fixture.root, &["--help".into()]).unwrap(),
        std::process::ExitCode::SUCCESS
    );
    assert_eq!(
        super::run(&fixture.root, &["--bad".into()]).unwrap(),
        std::process::ExitCode::from(2)
    );
    assert!(fixture.staging().is_empty());
}

#[test]
fn retained_descriptor_refuses_replacement_and_in_place_mutation_before_native_spawn() {
    for replace in [true, false] {
        let fixture = Fixture::new();
        let owned = files::Owned::new(&fixture.context).unwrap();
        let path = owned.path.join("tools/bin/cargo-audit");
        scanner(&path);
        let selected = Executable::open(
            &owned.path,
            owned.descriptor().unwrap(),
            path.as_os_str().as_encoded_bytes(),
            &mut true,
        )
        .unwrap();
        if replace {
            fs::remove_file(&path).unwrap();
        }
        fs::write(&path, b"PRIVATE-SENTINEL").unwrap();
        assert!(selected.validate().is_err());
        selected.close().unwrap();
        let (reporter, result) = owned.cleanup();
        result.unwrap();
        reporter
            .write(&serde_json::json!({"passed":false}))
            .unwrap();
    }
}
