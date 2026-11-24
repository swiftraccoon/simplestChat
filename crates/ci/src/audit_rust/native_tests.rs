//! Execute owned installer and scanner fixtures through the shared native
//! supervisor.

use super::*;
use crate::{browser_suite::process::Signals, process::Native};

fn executable(fixture: &mut Fixture, mode: &str) {
    let bin = fixture.root.join("fake commands");
    fs::create_dir(&bin).unwrap();
    let source = r#"
PATH=/usr/bin:/bin
root=${FIXTURE_ROOT?} mode=${FIXTURE_MODE?}
[ "$#" -ge 7 ] && [ "$1" = run ] && [ "$2" = --locked ] && [ "$3" = -q ] && [ "$4" = -p ] &&
  [ "$5" = simplestchat-security ] && [ "$6" = -- ] && [ "$7" = tools ] || exit 1
shift 6
[ "$#" -ge 2 ] || exit 1
if [ "$2" = install ]; then stage=install; else stage=resolve; fi
{ printf '['; json_string "$stage"; printf ', '; json_strings "$@"; printf ']\n'; } >>"$root/events"
if [ "$mode" = "$stage-fail" ]; then
  printf 'PRIVATE-SENTINEL\n' >&2
  exit 42
fi
if [ "$mode" = "$stage-hang" ]; then
  trap '' INT TERM
  printf '%s' "$$" >"$root/ready"
  while :; do sleep 0.01; done
fi
if [ "$stage" = install ]; then
  [ "$#" -eq 6 ] && [ "$2" = install ] && [ "$3" = --tools ] && [ "$4" = cargo-audit ] &&
    [ "$5" = --directory ] || exit 1
  mkdir -p -- "$6" && mkdir -- "$6/bin" || exit 1
  cat >"$6/bin/cargo-audit" <<'SCANNER'
#!/bin/sh
PATH=/usr/bin:/bin
root=${FIXTURE_ROOT?} mode=${FIXTURE_MODE?}
[ "$#" -eq 5 ] && [ "$1" = audit ] && [ "$2" = --deny ] && [ "$3" = unsound ] &&
  [ "$4" = --ignore ] && [ "$5" = RUSTSEC-2026-0459 ] || exit 1
printf 'owned fixture only' >"$root/audit-started"
if [ "$mode" = audit-hang ]; then
  trap '' INT TERM
  printf '%s' "$$" >"$root/ready"
  while :; do sleep 0.01; done
fi
if [ "$mode" = audit-fail ]; then exit 17; fi
printf 'Owned fixture advisory output\n'
SCANNER
  chmod 700 "$6/bin/cargo-audit"
else
  [ "$#" -eq 5 ] && [ "$2" = path ] && [ "$3" = cargo-audit ] && [ "$4" = --directory ] || exit 1
  if [ "$mode" = resolve-overflow ]; then
    repeat x 4097
    printf '\n'
  else
    printf '%s/bin/cargo-audit\n' "$5"
  fi
fi
"#;
    let path = bin.join("cargo");
    fs::write(
        &path,
        format!("#!/bin/sh\n{}{source}", crate::fake_shell::PRELUDE),
    )
    .unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    for (key, value) in [
        ("PATH", bin.into_os_string()),
        ("FIXTURE_ROOT", fixture.root.clone().into_os_string()),
        ("FIXTURE_MODE", mode.into()),
    ] {
        fixture.context.environment.insert(key.into(), value);
    }
}
fn signals() -> Signals {
    let (_, receiver) = tokio::sync::mpsc::unbounded_channel();
    Signals::injected(receiver)
}

#[tokio::test]
async fn actual_installer_and_selected_scanner_fixtures_preserve_literal_arguments_and_output_mode()
{
    let mut fixture = Fixture::new();
    executable(&mut fixture, "");
    let mut native = Native::new(&fixture.root, &fixture.context.environment, signals());
    execute::execute(&fixture.context, &mut native)
        .await
        .unwrap();
    assert!(fixture.root.join("audit-started").is_file());
    assert_eq!(
        fs::read_to_string(fixture.root.join("events"))
            .unwrap()
            .lines()
            .count(),
        2
    );
    assert!(fixture.staging().is_empty());
    assert_eq!(fixture.report()["passed"], true);
    assert!(!fixture.root.join("literal").exists());
}

#[tokio::test]
async fn actual_reference_and_scanner_failure_statuses_and_output_overflow_are_not_hidden() {
    for (mode, code, stage) in [
        ("install-fail", 42, "install"),
        ("resolve-fail", 42, "resolve"),
        ("audit-fail", 17, "audit"),
        ("resolve-overflow", 1, "resolve"),
    ] {
        let mut fixture = Fixture::new();
        executable(&mut fixture, mode);
        let mut native = Native::new(&fixture.root, &fixture.context.environment, signals());
        let error = execute::execute(&fixture.context, &mut native)
            .await
            .unwrap_err();
        assert_eq!(error.code, code, "{mode}");
        assert!(!error.message.contains("PRIVATE-SENTINEL"));
        assert!(fixture.staging().is_empty());
        let report = fixture.report();
        assert_eq!(report["passed"], false);
        assert_eq!(report["failure"], stage);
        assert_eq!(report["cleanupPassed"], true);
    }
}

struct Short<'a>(Native<'a>);
impl Executor for Short<'_> {
    fn cleanup_passed(&self) -> bool {
        self.0.cleanup_passed()
    }
    async fn ready(&mut self) -> Result<()> {
        self.0.ready().await
    }
    async fn run(&mut self, mut command: Command) -> Result<Vec<u8>> {
        command.timeout = Duration::from_secs(2);
        command.grace = Duration::from_millis(30);
        self.0.run(command).await
    }
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

#[tokio::test]
async fn interrupt_and_timeout_reap_owned_reference_or_audit_groups_before_namespace_retirement() {
    for (code, mode) in [
        (124, "install-hang"),
        (130, "audit-hang"),
        (143, "audit-hang"),
    ] {
        let mut fixture = Fixture::new();
        executable(&mut fixture, mode);
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut native = Short(Native::new(
            &fixture.root,
            &fixture.context.environment,
            Signals::injected(receiver),
        ));
        let cancel = async {
            ready(&fixture.root.join("ready")).await;
            if code != 124 {
                sender.send(code).unwrap();
            }
        };
        let (result, ()) = tokio::join!(execute::execute(&fixture.context, &mut native), cancel);
        assert_eq!(result.unwrap_err().code, code);
        let report = fixture.report();
        assert_eq!(report["passed"], false);
        assert_eq!(report["exitCode"], code);
        assert_eq!(report["cleanupPassed"], true);
        assert!(fixture.staging().is_empty());
        let pid: i32 = fs::read_to_string(fixture.root.join("ready"))
            .unwrap()
            .parse()
            .unwrap();
        assert!(
            rustix::process::test_kill_process(rustix::process::Pid::from_raw(pid).unwrap())
                .is_err()
        );
    }
}

struct Changed<'a>(Native<'a>);
impl Executor for Changed<'_> {
    fn cleanup_passed(&self) -> bool {
        self.0.cleanup_passed()
    }
    async fn ready(&mut self) -> Result<()> {
        self.0.ready().await
    }
    async fn run(&mut self, command: Command) -> Result<Vec<u8>> {
        if command.program == Program::CargoAudit {
            let path = command
                .executable
                .as_ref()
                .unwrap()
                .validate()
                .unwrap()
                .to_owned();
            fs::write(path, b"PRIVATE-SENTINEL").unwrap();
        }
        self.0.run(command).await
    }
}
#[tokio::test]
async fn native_rechecks_selected_inode_immediately_before_spawn_without_path_fallback() {
    let mut fixture = Fixture::new();
    executable(&mut fixture, "");
    let mut native = Changed(Native::new(
        &fixture.root,
        &fixture.context.environment,
        signals(),
    ));
    assert!(
        execute::execute(&fixture.context, &mut native)
            .await
            .is_err()
    );
    assert!(!fixture.root.join("audit-started").exists());
    assert!(fixture.staging().is_empty());
    assert_eq!(fixture.report()["failure"], "audit");
}

#[tokio::test]
async fn missing_or_wrong_role_proof_and_prior_cancellation_create_no_child() {
    let mut fixture = Fixture::new();
    executable(&mut fixture, "");
    let mut native = Native::new(&fixture.root, &fixture.context.environment, signals());
    let error = native
        .run(Command::capture(Program::CargoAudit, &["audit"]))
        .await
        .unwrap_err();
    assert_eq!(error.code, 1);
    let owned = files::Owned::new(&fixture.context).unwrap();
    let path = owned.path.join("tools/bin/cargo-audit");
    scanner(&path);
    let selected = Arc::new(
        Executable::open(
            &owned.path,
            owned.descriptor().unwrap(),
            path.as_os_str().as_encoded_bytes(),
            &mut true,
        )
        .unwrap(),
    );
    // The scanner's selected executable is no proof for another program.
    let mut command = Command::capture(Program::Cargo, &["--version"]);
    command.executable = Some(Arc::clone(&selected));
    let refused = native.run(command).await.unwrap_err();
    assert_eq!(
        refused.message,
        "CI execution requires its validated executable role."
    );
    Arc::try_unwrap(selected).unwrap().close().unwrap();
    let (reporter, cleanup) = owned.cleanup();
    cleanup.unwrap();
    reporter
        .write(&serde_json::json!({"passed":false}))
        .unwrap();
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    sender.send(143).unwrap();
    let mut native = Native::new(
        &fixture.root,
        &fixture.context.environment,
        Signals::injected(receiver),
    );
    assert_eq!(
        execute::execute(&fixture.context, &mut native)
            .await
            .unwrap_err()
            .code,
        143
    );
    assert!(!fixture.root.join("events").exists());
    assert!(fixture.staging().is_empty());
}
