//! Complete early ownership/failure paths; no hosted component is acquired.

use super::*;
use crate::ci::command::Program;
use std::{fs, os::unix::fs::PermissionsExt};

fn context(root: &Path) -> Context {
    Context {
        repository: "swiftraccoon/simplestChat",
        revision: "a".repeat(40),
        package_version: env!("CARGO_PKG_VERSION"),
        protocol_version: 1,
        target: super::super::context::TARGET,
        build_run_id: 123,
        run_attempt: 2,
        workspace: root.into(),
    }
}
struct Pending {
    clock: Arc<super::super::fixture::Clock>,
    code: u8,
    late: bool,
}
impl Executor for Pending {
    async fn run(&mut self, _: Command) -> Result<Vec<u8>> {
        panic!("no CLI after refused creation")
    }
    async fn ready(&mut self) -> Result<()> {
        if self.late {
            self.clock.advance(plan::ENTRY + Duration::from_secs(1));
        }
        if self.code == 0 {
            Ok(())
        } else {
            Err(failure(self.code))
        }
    }
}
#[tokio::test]
async fn namespace_preparation_cannot_admit_output_after_signal_or_entry_deadline() {
    for code in [130, 143, 124] {
        let fixture = super::super::fixture::Fixture::new();
        let workspace = fixture.root.join("source");
        fs::create_dir(&workspace).unwrap();
        let context = context(&workspace);
        let budget = Budget::default();
        let clock = Arc::new(super::super::fixture::Clock::new());
        let plan = Plan::new(clock.clone());
        let mut executor = Pending {
            clock,
            code: if code == 124 { 0 } else { code },
            late: code == 124,
        };
        let output = fixture.root.join("result");
        super::super::fixture::expect_error(
            create(&context, &output, &budget, &plan, &mut executor).await,
            code,
        );
        assert!(!output.exists());
        assert_eq!(budget.count(), 0);
    }
}
struct FirstFailure {
    calls: usize,
}
impl Executor for FirstFailure {
    async fn run(&mut self, request: Command) -> Result<Vec<u8>> {
        self.calls += 1;
        assert_eq!(request.program, Program::OpsGit);
        assert!(request.source_git.as_ref().unwrap().context()?.is_some());
        Err(failure(42))
    }
    async fn ready(&mut self) -> Result<()> {
        Ok(())
    }
}
#[tokio::test]
async fn whole_coordinator_source_failure_preserves_primary_closes_peers_and_never_fetches() {
    let fixture = super::super::fixture::Fixture::new();
    let workspace = fixture.root.join("source");
    fs::create_dir_all(workspace.join(".git/objects/info")).unwrap();
    let context = context(&workspace);
    let toolroot = fixture.root.join("tools");
    fs::create_dir(&toolroot).unwrap();
    for name in ["git", "curl", "dpkg-deb"] {
        let path = toolroot.join(name);
        fs::write(&path, b"#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let env = Environment::from([("PATH".into(), toolroot.into_os_string())]);
    let budget = Budget::default();
    let plan = Plan::new(Arc::new(super::super::fixture::Clock::new()));
    let run = OwnedRun::local(&fixture.root.join("result"), &budget).unwrap();
    let mut executor = FirstFailure { calls: 0 };
    let mut diagnostics = Vec::new();
    let mut progress = Progress::new(&plan, &mut diagnostics);
    super::super::fixture::expect_error(
        produce(&context, &run, &env, &mut progress, &mut executor).await,
        42,
    );
    assert!(!progress.finished());
    assert_eq!(executor.calls, 1);
    for never in ["downloads", "registry", "kit", "build", "unsigned"] {
        assert!(!run.path.join(never).exists());
    }
    run.finish().unwrap();
    assert_eq!(budget.count(), 0);
    let (attempted, fallback) = budget.close_counts();
    assert!(attempted > 0);
    assert_eq!(fallback, 0);
}
#[tokio::test]
async fn lexical_and_physical_source_ancestors_are_refused_without_new_output() {
    let fixture = super::super::fixture::Fixture::new();
    let workspace = fixture.root.join("source");
    fs::create_dir(&workspace).unwrap();
    let context = context(&workspace);
    let budget = Budget::default();
    let plan = Plan::new(Arc::new(super::super::fixture::Clock::new()));
    let mut executor = super::super::fixture::Noop;
    super::super::fixture::expect_error(
        create(
            &context,
            &workspace.join("new"),
            &budget,
            &plan,
            &mut executor,
        )
        .await,
        1,
    );
    assert!(!workspace.join("new").exists());
    assert_eq!(budget.count(), 0);
}
