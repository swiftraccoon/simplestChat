//! The shared owner runs fixture executables in a PATH containing no real tools.

use super::*;
use crate::{browser_suite::process::Signals, process::Native};
use std::os::unix::fs::PermissionsExt;

fn executable(fixture: &mut Fixture, expected: &Expected, mode: &str) {
    let directory = fixture.root.join("fake commands");
    fs::create_dir(&directory).unwrap();
    // `FIXTURE_VERSION` holds the expected version command, one word per line.
    let source = r#"
PATH=/usr/bin:/bin
root=${FIXTURE_ROOT?}
program=${0##*/}
newline='
'
member() {
  member_word=$1
  shift
  for member_item in "$@"; do
    if [ "$member_item" = "$member_word" ]; then return 0; fi
  done
  return 1
}
same() {
  same_rest=$1
  shift
  while [ -n "$same_rest" ]; do
    same_line=${same_rest%%"$newline"*}
    same_rest=${same_rest#*"$newline"}
    [ "$#" -gt 0 ] && [ "$1" = "$same_line" ] || return 1
    shift
  done
  [ "$#" -eq 0 ]
}
if [ "$program" = curl ]; then
  stage=download
elif [ "$program" = docker ]; then
  stage=caller_version
else
  [ "$#" -ge 1 ] || exit 1
  if [ "$1" = env ]; then
    stage=root_version
  else
    [ "$#" -ge 2 ] || exit 1
    if [ "$2" = -d ]; then stage=install_directory; else stage=install_file; fi
  fi
fi
mode=${FIXTURE_MODE?}
{
  printf '['; json_string "$program"; printf ', '; json_strings "$@"
  printf ', '; json_string "$stage"; printf ']\n'
} >>"$root/events"
if [ "$mode" = "$stage-fail" ]; then
  printf 'PRIVATE-SENTINEL\n' >&2
  exit 42
fi
if [ "$stage" = install_file ]; then
  [ "$#" -ge 5 ] && [ "$1" = install ] && [ "$2" = -m ] && [ "$3" = 0755 ] || exit 1
  case $4 in */verified) ;; *) exit 1 ;; esac
  printf 'owned fixture plugin\n' | cmp -s - "$4" || exit 1
  [ "$5" = "${FIXTURE_PLUGIN?}" ] || exit 1
  printf 'owned fixture only' >"$root/installed-fixture"
fi
if [ "$mode" = "$stage-hang" ]; then
  trap '' INT TERM
  printf '%s' "$$" >"$root/ready"
  while :; do sleep 0.01; done
fi
if [ "$stage" = download ]; then
  if member --output "$@"; then exit 1; fi
  if [ "$mode" = swap-download ]; then
    prefix=
    for candidate in "$root/runner temp"/*; do
      if [ -d "$candidate" ]; then prefix=$candidate; break; fi
    done
    [ -n "$prefix" ] || exit 1
    printf 'foreign sentinel' >"$root/private-target"
    rm -- "$prefix/download" && ln -s -- "$root/private-target" "$prefix/download" || exit 1
  fi
  printf 'owned fixture plugin\n'
elif [ "$stage" = install_directory ]; then
  [ "$#" -eq 5 ] && [ "$1" = install ] && [ "$2" = -d ] && [ "$3" = -m ] && [ "$4" = 0755 ] &&
    [ "$5" = /usr/local/lib/docker/cli-plugins ] || exit 1
elif [ "$stage" = caller_version ]; then
  same "${FIXTURE_VERSION?}" "$@" || exit 1
  printf '%s\n' "${FIXTURE_ANSWER?}"
elif [ "$stage" = root_version ]; then
  [ "$#" -ge 11 ] && [ "$1" = env ] && [ "$2" = -i ] &&
    [ "$3" = PATH=/usr/sbin:/usr/bin:/sbin:/bin ] && [ "$4" = LC_ALL=C ] && [ "$5" = timeout ] &&
    [ "$6" = --signal=TERM ] && [ "$7" = --kill-after=2s ] && [ "$8" = 10s ] &&
    [ "$9" = /usr/bin/docker ] && [ "${10}" = --host ] &&
    [ "${11}" = unix:///var/run/docker.sock ] || exit 1
  shift 11
  same "${FIXTURE_VERSION?}" "$@" || exit 1
  printf '%s\n' "${FIXTURE_ANSWER?}"
fi
"#;
    for name in ["curl", "sudo", "docker"] {
        let path = directory.join(name);
        fs::write(
            &path,
            format!("#!/bin/sh\n{}{source}", crate::fake_shell::PRELUDE),
        )
        .unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let version = expected
        .command
        .iter()
        .map(|word| format!("{word}\n"))
        .collect::<String>();
    for (key, value) in [
        ("PATH", directory.into_os_string()),
        ("FIXTURE_ROOT", fixture.root.clone().into_os_string()),
        ("FIXTURE_MODE", mode.into()),
        ("FIXTURE_PLUGIN", expected.path.into()),
        ("FIXTURE_VERSION", version.into()),
        ("FIXTURE_ANSWER", expected.answer.into()),
    ] {
        fixture.context.environment.insert(key.into(), value);
    }
}
fn signals() -> Signals {
    let (_, receiver) = tokio::sync::mpsc::unbounded_channel();
    Signals::injected(receiver)
}

#[tokio::test]
async fn actual_fixture_processes_authenticate_owned_stdout_before_both_literal_version_checks() {
    for expected in &EXPECTED {
        let mut fixture = Fixture::new(expected);
        executable(&mut fixture, expected, "");
        let mut native = Native::new(
            &fixture.context.root,
            &fixture.context.environment,
            signals(),
        );
        execute::install(&fixture.context, &mut native)
            .await
            .unwrap();
        assert_eq!(
            fs::read_to_string(fixture.root.join("events"))
                .unwrap()
                .lines()
                .count(),
            5
        );
        assert!(fixture.root.join("installed-fixture").is_file());
        assert!(fixture.staging().is_empty());
        assert_eq!(fixture.report()["passed"], true);
        assert!(!fixture.root.join("literal").exists());
    }
}

#[tokio::test]
async fn download_path_swap_cannot_redirect_a_single_byte_of_curl_output() {
    for expected in &EXPECTED {
        let mut fixture = Fixture::new(expected);
        executable(&mut fixture, expected, "swap-download");
        let mut native = Native::new(
            &fixture.context.root,
            &fixture.context.environment,
            signals(),
        );
        assert!(
            execute::install(&fixture.context, &mut native)
                .await
                .is_err()
        );
        assert_eq!(
            fs::read(fixture.root.join("private-target")).unwrap(),
            b"foreign sentinel"
        );
        assert!(!fixture.root.join("installed-fixture").exists());
        assert!(fixture.staging().is_empty());
        assert_eq!(fixture.report()["failure"], "authenticate");
    }
}

#[tokio::test]
async fn process_failures_never_publish_success_or_expose_captured_errors() {
    for expected in &EXPECTED {
        for stage in [
            "download",
            "install_directory",
            "install_file",
            "caller_version",
            "root_version",
        ] {
            let mut fixture = Fixture::new(expected);
            executable(&mut fixture, expected, &format!("{stage}-fail"));
            let mut native = Native::new(
                &fixture.context.root,
                &fixture.context.environment,
                signals(),
            );
            let error = execute::install(&fixture.context, &mut native)
                .await
                .unwrap_err();
            assert_eq!(error.code, 42);
            assert!(!error.message.contains("PRIVATE-SENTINEL"));
            assert!(fixture.staging().is_empty());
            let report = fixture.report();
            assert_eq!(report["passed"], false);
            assert_eq!(report["failure"], stage);
            assert_eq!(report["cleanupPassed"], true);
        }
    }
}

struct Short<'a>(Native<'a>);
impl Executor for Short<'_> {
    fn cleanup_passed(&self) -> bool {
        self.0.cleanup_passed()
    }
    async fn run(&mut self, mut command: Command) -> Result<Vec<u8>> {
        command.timeout = Duration::from_secs(2);
        command.grace = Duration::from_millis(30);
        self.0.run(command).await
    }
    async fn ready(&mut self) -> Result<()> {
        self.0.ready().await
    }
}
async fn ready(path: &std::path::Path) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !path.is_file() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn interrupt_and_timeout_cleanup_preserve_truth_after_uncertain_privileged_install() {
    for expected in &EXPECTED {
        for (code, stage) in [(124, "download"), (130, "download"), (143, "install_file")] {
            let mut fixture = Fixture::new(expected);
            executable(&mut fixture, expected, &format!("{stage}-hang"));
            let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
            let mut native = Short(Native::new(
                &fixture.context.root,
                &fixture.context.environment,
                Signals::injected(receiver),
            ));
            let cancel = async {
                ready(&fixture.root.join("ready")).await;
                if code != 124 {
                    sender.send(code).unwrap();
                }
            };
            let (result, ()) =
                tokio::join!(execute::install(&fixture.context, &mut native), cancel);
            assert_eq!(result.unwrap_err().code, code);
            assert!(fixture.staging().is_empty());
            let report = fixture.report();
            assert_eq!(report["exitCode"], code);
            assert_eq!(report["cleanupPassed"], true);
            assert_eq!(report["passed"], false);
            if stage == "install_file" {
                assert!(fixture.root.join("installed-fixture").exists());
                assert_eq!(report["installationAttempted"], true);
                assert_eq!(report["installationCompleted"], false);
            }
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
}

#[tokio::test]
async fn already_cancelled_installer_spawns_nothing_and_creates_no_owned_resources() {
    for expected in &EXPECTED {
        let mut fixture = Fixture::new(expected);
        executable(&mut fixture, expected, "");
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        sender.send(143).unwrap();
        let mut native = Native::new(
            &fixture.context.root,
            &fixture.context.environment,
            Signals::injected(receiver),
        );
        assert_eq!(
            execute::install(&fixture.context, &mut native)
                .await
                .unwrap_err()
                .code,
            143
        );
        assert!(!fixture.root.join("events").exists());
        assert!(fixture.staging().is_empty());
    }
}
