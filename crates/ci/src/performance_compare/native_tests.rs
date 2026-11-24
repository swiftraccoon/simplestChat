//! Closed comparison roles run only an owned inert shell recorder. File
//! handshakes establish readiness/finalization; no benchmark or compiler runs.

use super::{files, plan};
use crate::{
    bounded_log::Log,
    command::{Command, Program},
    process::{Executor, Native},
};
use simplestchat_toolkit::{
    owned_process::{Group, Signals},
    tooling::Environment,
};
use std::{
    fs::{self, OpenOptions},
    os::unix::{
        fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

struct Fixture {
    path: PathBuf,
    root: PathBuf,
    environment: Environment,
}
impl Fixture {
    fn new(body: &str) -> Self {
        let path = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("comparison-native-{}", files::nonce().unwrap()));
        fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        let root = path.join("selected candidate λ");
        fs::create_dir(&root).unwrap();
        fs::create_dir(path.join("temporary")).unwrap();
        fs::create_dir_all(root.join("target/release")).unwrap();
        let script = format!(
            concat!(
                "#!/bin/sh\nroot=${{FIXTURE_ROOT?}}\n",
                "[ \"$(pwd -P)\" = \"${{EXPECTED_CWD?}}\" ] || exit 1\n",
                "[ \"${{CAPTURED_MARKER?}}\" = 'selected literal context' ] || exit 1\n",
                "[ \"$#\" -eq 1 ] && [ \"$1\" = --owned-inert ] || exit 1\n",
                "record() {{\n",
                "  printf '%s' \"$2\" >\"$root/$1.pending\"\n",
                "  /bin/mv -f \"$root/$1.pending\" \"$root/$1\"\n",
                "}}\n",
                "{body}\n"
            ),
            body = body
        );
        let binary = root.join("target/release/load_test");
        fs::write(&binary, script).unwrap();
        fs::set_permissions(binary, fs::Permissions::from_mode(0o700)).unwrap();
        let environment = [
            ("GITHUB_ACTIONS", "true".into()),
            ("RUNNER_OS", "Linux".into()),
            ("RUNNER_TEMP", path.join("temporary").into_os_string()),
            ("FIXTURE_ROOT", path.clone().into_os_string()),
            ("EXPECTED_CWD", root.clone().into_os_string()),
            ("CAPTURED_MARKER", "selected literal context".into()),
            ("PATH", "/nonexistent/no-fallback".into()),
        ]
        .into_iter()
        .map(|(key, value)| (key.into(), value))
        .collect();
        Self {
            path,
            root,
            environment,
        }
    }
    fn request(
        &self,
        tool: &Arc<files::Tool>,
        scope: &Arc<files::Scope>,
        program: Program,
    ) -> Command {
        let mut command = Command::capture(program, &["--owned-inert"]);
        command.comparison = Some(files::Invocation::new(
            tool.clone(),
            scope.clone(),
            self.environment.clone(),
        ));
        command.timeout = Duration::from_secs(5);
        command.grace = Duration::from_secs(5);
        command.capture = None;
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(self.path.join("combined.log"))
            .unwrap();
        command.combined_log = Some(Arc::new(Log::start(file, 4096).unwrap()));
        command
    }
    fn selected(&self) -> (Arc<files::Tool>, Arc<files::Scope>) {
        let plan = plan::Plan::parse(&self.root, self.environment.clone(), "linux").unwrap();
        (
            files::Tool::benchmark(&plan).unwrap(),
            files::Scope::existing(&self.root).unwrap(),
        )
    }
    fn output(&self) -> Vec<u8> {
        fs::read(self.path.join("combined.log")).unwrap()
    }
    fn output_lines(&self) -> Vec<String> {
        let mut lines: Vec<_> = String::from_utf8(self.output())
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect();
        lines.sort();
        lines
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}
async fn ready(path: &Path) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !path.is_file() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}
fn no_signal() -> Signals {
    let (_, receiver) = tokio::sync::mpsc::unbounded_channel();
    Signals::injected(receiver)
}

#[tokio::test]
async fn benchmark_term_reaches_only_leader_and_waits_for_cooperative_finalizers() {
    let fixture = Fixture::new(
        r#"
(
  trap 'record descendant-term observed' TERM
  record descendant-ready "$(exec /bin/sh -c 'printf %s "$PPID"')"
  while [ ! -e "$root/descendant-release" ]; do
    if [ -e "$root/probe-request" ]; then record descendant-probe alive; fi
    /bin/sleep 0.005
  done
  exit 0
) &
child=$!
finish() {
  record leader-term observed
  while [ ! -e "$root/release" ]; do /bin/sleep 0.005; done
  [ ! -e "$root/descendant-term" ] || exit 1
  record descendant-release released
  wait "$child" || exit 1
  printf 'finalized-output\n'
  printf 'finalized-error\n' >&2
  record finalized after-descendant-and-output
  exit 0
}
trap finish TERM
while [ ! -e "$root/descendant-ready" ]; do /bin/sleep 0.005; done
record ready "$$"
while :; do /bin/sleep 0.01; done
"#,
    );
    let (tool, scope) = fixture.selected();
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    let unrelated_environment = Environment::new();
    let mut native = Native::new(
        &fixture.path,
        &unrelated_environment,
        Signals::injected(receiver),
    );
    let mut unrelated = std::process::Command::new("/bin/sleep");
    unrelated.arg("30").process_group(0);
    let unrelated = Group::new(unrelated.spawn().unwrap());
    let interrupt = async {
        ready(&fixture.path.join("ready")).await;
        sender.send(143).unwrap();
        ready(&fixture.path.join("leader-term")).await;
        fs::write(fixture.path.join("probe-request"), b"probe").unwrap();
        ready(&fixture.path.join("descendant-probe")).await;
        assert!(!fixture.path.join("descendant-term").exists());
        fs::write(fixture.path.join("release"), b"finalize").unwrap();
    };
    let (result, ()) = tokio::join!(
        native.run(fixture.request(&tool, &scope, Program::ComparisonBenchmark)),
        interrupt
    );
    assert_eq!(result.unwrap_err().code, 143);
    assert!(native.cleanup_passed());
    assert_eq!(
        fixture.output_lines(),
        ["finalized-error", "finalized-output"]
    );
    assert_eq!(
        fs::read(fixture.path.join("finalized")).unwrap(),
        b"after-descendant-and-output"
    );
    assert!(!simplestchat_toolkit::owned_process::exited_now(unrelated.pid()).unwrap());
    drop(unrelated);
    files::close_tool(tool).unwrap();
    files::finish(scope, false).unwrap();
}

#[tokio::test]
async fn wrong_comparison_role_refuses_before_dispatch_and_finishes_the_admitted_log() {
    let fixture = Fixture::new("record dispatched unexpected");
    let (tool, scope) = fixture.selected();
    let mut native = Native::new(&fixture.path, &fixture.environment, no_signal());
    let error = native
        .run(fixture.request(&tool, &scope, Program::ComparisonCargo))
        .await
        .unwrap_err();
    assert_eq!(error.code, 1);
    assert!(native.cleanup_passed());
    assert!(!fixture.path.join("dispatched").exists());
    assert!(fixture.output().is_empty());
    files::close_tool(tool).unwrap();
    files::finish(scope, false).unwrap();
}

#[tokio::test]
async fn closed_benchmark_nonzero_status_keeps_both_output_channels_and_retirement() {
    let fixture = Fixture::new("printf 'closed-output\\n'\nprintf 'closed-error\\n' >&2\nexit 42");
    let (tool, scope) = fixture.selected();
    let mut native = Native::new(&fixture.path, &fixture.environment, no_signal());
    let error = native
        .run(fixture.request(&tool, &scope, Program::ComparisonBenchmark))
        .await
        .unwrap_err();
    assert_eq!(error.code, 42);
    assert!(native.cleanup_passed());
    assert_eq!(fixture.output_lines(), ["closed-error", "closed-output"]);
    files::close_tool(tool).unwrap();
    files::finish(scope, false).unwrap();
}
