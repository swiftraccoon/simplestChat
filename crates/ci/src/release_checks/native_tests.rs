//! Actual inert Node, `sudo` and Cargo shell fixtures through the shared
//! reserved-group owner: the fixture and the scanner download overlap, each
//! keeps its status, and a signal retires both.

use super::*;
use crate::{browser_suite::process::Signals, process::Native};

/// Fixture programs on the test PATH. Each writes its own pid, and the two
/// overlapped ones meet at a barrier that fails unless both run at once.
fn commands(f: &mut Fixture, mode: &str) {
    let node = r#"#!/bin/sh
[ "$1" = -e ] || exit 91
echo $$ > "$FIXTURE_ROOT/node-pid"; /bin/mv "$FIXTURE_ROOT/node-pid" "$FIXTURE_ROOT/node-ready"
case "$FIXTURE_MODE" in
  node-fail) exit 42 ;;
  node-hang) trap '' TERM INT; while :; do /bin/sleep 0.01; done ;;
  overflow) /usr/bin/head -c 4097 /dev/zero | /usr/bin/tr '\0' x; exit 0 ;;
esac
printf '%s' "$FIXTURE_ROOT/browser with spaces"
"#;
    executable(&f.node, node.as_bytes());
    let overlapped = |name: &str, other: &str, failure: &str, code: u8| {
        format!(
            r#"#!/bin/sh
if [ "$FIXTURE_MODE" = cancel ]; then
  trap '/bin/sleep 0.05; echo owned > "$FIXTURE_ROOT/{name}-cleaned"; exit 0' TERM INT
fi
printf '%s\n' "$@" > "$FIXTURE_ROOT/{name}-arguments"
echo $$ > "$FIXTURE_ROOT/{name}-pid"; /bin/mv "$FIXTURE_ROOT/{name}-pid" "$FIXTURE_ROOT/{name}-ready"
if [ "$FIXTURE_MODE" = cancel ]; then
  while :; do /bin/sleep 0.01; done
fi
waited=0
while [ ! -e "$FIXTURE_ROOT/{other}-ready" ]; do
  /bin/sleep 0.01; waited=$((waited + 1)); [ $waited -lt 300 ] || exit 93
done
echo "Owned {name} fixture output"
[ "$FIXTURE_MODE" = {failure} ] && exit {code}
exit 0
"#
        )
    };
    let bin = f.node.parent().unwrap().to_path_buf();
    executable(
        &bin.join("sudo"),
        overlapped("fixture", "scanner", "fixture-fail", 17).as_bytes(),
    );
    executable(
        &bin.join("cargo"),
        overlapped("scanner", "fixture", "scanner-fail", 23).as_bytes(),
    );
    for (key, value) in [
        ("FIXTURE_ROOT", f.root.clone().into_os_string()),
        ("FIXTURE_MODE", mode.into()),
    ] {
        f.context.environment.insert(key.into(), value);
    }
}
fn signals() -> Signals {
    let (_, receiver) = tokio::sync::mpsc::unbounded_channel();
    Signals::injected(receiver)
}
async fn ready(path: &Path) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !path.is_file() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}
fn gone(path: &Path) -> bool {
    let pid = fs::read_to_string(path).unwrap().trim().parse().unwrap();
    rustix::process::test_kill_process(rustix::process::Pid::from_raw(pid).unwrap()).is_err()
}

#[tokio::test]
async fn actual_overlapped_commands_keep_literal_arguments_and_their_own_statuses() {
    for (mode, expected) in [
        ("", 0),
        ("node-fail", 42),
        ("fixture-fail", 17),
        ("scanner-fail", 23),
        ("overflow", 1),
    ] {
        let mut f = Fixture::new();
        commands(&mut f, mode);
        let mut native = Native::new(&f.root, &f.context.environment, signals());
        let mut scanner = Native::new(&f.root, &f.context.environment, signals());
        let code = execute::execute(&f.context, &mut native, &mut scanner)
            .await
            .map_or_else(|error| error.code, |()| 0);
        assert_eq!(code, expected, "{mode}");
        assert!(native.cleanup_passed() && scanner.cleanup_passed());
        let started = mode.is_empty() || mode.ends_with("-fail") && mode != "node-fail";
        assert_eq!(f.root.join("fixture-ready").exists(), started, "{mode}");
        assert_eq!(f.root.join("scanner-ready").exists(), started, "{mode}");
        if started {
            let fixture = fs::read_to_string(f.root.join("fixture-arguments")).unwrap();
            assert!(fixture.starts_with(&format!(
                "{}\nrelease-fixture\n--disposable-host\n",
                f.context.http_policy.display()
            )));
            let scanner = fs::read_to_string(f.root.join("scanner-arguments")).unwrap();
            assert!(scanner.starts_with("run\n--locked\n-q\n-p\nsimplestchat-security\n--\n"));
            let output = f.context.temporary.join("release-checks");
            let log = fs::read_to_string(output.join("release-fixture.stdout")).unwrap();
            assert_eq!(log, "Owned fixture fixture output\n");
            assert!(gone(&f.root.join("fixture-ready")) && gone(&f.root.join("scanner-ready")));
        }
    }
}

struct Short<'a>(Native<'a>);
impl Executor for Short<'_> {
    async fn run(&mut self, mut command: Command) -> Result<Vec<u8>> {
        command.timeout = Duration::from_millis(200);
        command.grace = Duration::from_millis(200);
        self.0.run(command).await
    }
    async fn ready(&mut self) -> Result<()> {
        self.0.ready().await
    }
    fn cleanup_passed(&self) -> bool {
        self.0.cleanup_passed()
    }
}

#[tokio::test]
async fn node_timeout_and_cancellation_never_start_the_fixture_and_reap_discovery() {
    for code in [124, 130, 143] {
        let mut f = Fixture::new();
        commands(&mut f, "node-hang");
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut native = Short(Native::new(
            &f.root,
            &f.context.environment,
            Signals::injected(receiver),
        ));
        let mut scanner = Native::new(&f.root, &f.context.environment, signals());
        let cancel = async {
            ready(&f.root.join("node-ready")).await;
            if code != 124 {
                sender.send(code).unwrap();
            }
        };
        let (result, ()) = tokio::join!(
            execute::execute(&f.context, &mut native, &mut scanner),
            cancel
        );
        assert_eq!(result.unwrap_err().code, code);
        assert!(native.cleanup_passed());
        assert!(!f.root.join("fixture-ready").exists());
        assert!(!f.root.join("scanner-ready").exists());
        assert!(gone(&f.root.join("node-ready")));
    }
}

#[tokio::test]
async fn a_signal_lets_both_overlapped_groups_finish_their_own_cleanup_and_fails_the_step() {
    let mut f = Fixture::new();
    commands(&mut f, "cancel");
    struct Unrelated(std::process::Child);
    impl Drop for Unrelated {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let mut unrelated = Unrelated(
        std::process::Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .unwrap(),
    );
    let (first, receiver) = tokio::sync::mpsc::unbounded_channel();
    let (second, other) = tokio::sync::mpsc::unbounded_channel();
    let mut native = Native::new(&f.root, &f.context.environment, Signals::injected(receiver));
    let mut scanner = Native::new(&f.root, &f.context.environment, Signals::injected(other));
    let cancel = async {
        ready(&f.root.join("fixture-ready")).await;
        ready(&f.root.join("scanner-ready")).await;
        first.send(143).unwrap();
        second.send(143).unwrap();
    };
    let (result, ()) = tokio::join!(
        execute::execute(&f.context, &mut native, &mut scanner),
        cancel
    );
    let alive = unrelated.0.try_wait().unwrap().is_none();
    drop(unrelated);
    assert!(result.is_err());
    assert!(native.cleanup_passed() && scanner.cleanup_passed());
    assert!(alive);
    assert!(f.root.join("fixture-cleaned").exists() && f.root.join("scanner-cleaned").exists());
    let summary: serde_json::Value = serde_json::from_slice(
        &fs::read(f.context.temporary.join("release-checks/summary.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(summary["passed"], false);
    assert_eq!(
        summary["checks"][0]["failure"],
        "Production image command interrupted."
    );
}

#[tokio::test]
async fn node_role_proof_never_falls_back_to_path_or_another_program() {
    let mut f = Fixture::new();
    commands(&mut f, "");
    let mut native = Native::new(&f.root, &f.context.environment, signals());
    assert!(
        native
            .run(Command::capture(Program::Node, &["-e", plan::EXPRESSION]))
            .await
            .is_err()
    );
    let proof = Arc::new(files::node(&f.root, &f.context.environment).unwrap());
    let mut request = Command::capture(Program::Sudo, &[]);
    request.node = Some(Arc::clone(&proof));
    assert!(native.run(request).await.is_err());
    Arc::try_unwrap(proof).unwrap().close().unwrap();
    assert!(!f.root.join("node-ready").exists());
    assert!(!f.root.join("fixture-ready").exists());
}
