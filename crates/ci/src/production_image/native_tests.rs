//! Real owned fixture processes exercise the production supervisor, never Docker.

use super::super::process::Native;
use super::*;
use crate::browser_suite::process::Signals;
use std::os::unix::fs::PermissionsExt;
use std::time::Instant;

fn signals() -> Signals {
    let (_, receiver) = tokio::sync::mpsc::unbounded_channel();
    Signals::injected(receiver)
}

fn executable(fixture: &mut Fixture, body: &str) {
    let directory = fixture.root.join("fake commands");
    fs::create_dir(&directory).unwrap();
    // `holds FILE KEY VALUE` accepts an ownership journal, written as the
    // owner's pretty JSON, that names VALUE as KEY.
    let source = format!(
        r#"#!/bin/sh
{prelude}root=${{FIXTURE_ROOT?}}
name=${{0##*/}}
newline='
'
member() {{
  member_word=$1
  shift
  for member_item in "$@"; do
    if [ "$member_item" = "$member_word" ]; then return 0; fi
  done
  return 1
}}
value() {{
  value_flag=$1
  shift
  while [ "$#" -gt 0 ]; do
    if [ "$1" = "$value_flag" ]; then
      [ "$#" -ge 2 ] || return 1
      printf '%s' "$2"
      return 0
    fi
    shift
  done
  return 1
}}
holds() {{
  holds_text=$newline$(/bin/cat -- "$1")$newline || return 1
  holds_line="  \"$2\": $(json_string "$3")"
  case $holds_text in
    *"$newline$holds_line$newline"* | *"$newline$holds_line,$newline"*) ;;
    *) return 1 ;;
  esac
}}
event="[$(json_string "$name"), $(json_strings "$@")]"
printf '%s\n' "$event" >>"$root/events"
{body}
"#,
        prelude = crate::fake_shell::PRELUDE
    );
    for program in ["git", "docker"] {
        let path = directory.join(program);
        fs::write(&path, &source).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    fixture
        .context
        .environment
        .insert("PATH".into(), directory.into_os_string());
    fixture
        .context
        .environment
        .insert("FIXTURE_ROOT".into(), fixture.root.clone().into_os_string());
}

async fn wait(path: &std::path::Path) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !path.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn actual_fake_git_and_docker_execute_journal_build_cleanup_then_append() {
    let mut fixture = Fixture::new();
    executable(
        &mut fixture,
        r#"
if [ "$name" = git ]; then
  if [ "$#" -eq 2 ] && [ "$1" = rev-parse ] && [ "$2" = HEAD ]; then
    printf '%s\n' "${GITHUB_SHA?}"
  elif ! { [ "$#" -eq 3 ] && [ "$1" = status ] && [ "$2" = --porcelain=v1 ] &&
    [ "$3" = --untracked-files=all ]; }; then
    exit 91
  fi
elif [ "$#" -ge 2 ] && [ "$1" = buildx ] && [ "$2" = create ]; then
  builder=$(value --name "$@") || exit 1
  journal=${RUNNER_TEMP?}/production-builder-${builder#simplestchat-ci-}/ownership.json
  holds "$journal" builder "$builder" && holds "$journal" state creation-planned || exit 1
elif [ "$#" -ge 2 ] && [ "$1" = buildx ] && [ "$2" = build ]; then
  builder=$(value --builder "$@") || exit 1
  case $builder in simplestchat-ci-*) ;; *) exit 1 ;; esac
  iidfile=$(value --iidfile "$@") || exit 1
  printf 'sha256:%s\n' "$(repeat b 64)" >"$iidfile"
  destination=${RUNNER_TEMP?}/buildx-cache-next
  /bin/mkdir -- "$destination" || exit 1
  printf completed >"$destination/index.json"
elif [ "$#" -ge 2 ] && [ "$1" = image ] && [ "$2" = inspect ]; then
  revision=$(json_string "${GITHUB_SHA?}")
  printf '{"Id": "sha256:%s", "Config": {"User": "10001:10001", ' "$(repeat b 64)"
  printf '"Labels": {"org.opencontainers.image.revision": %s}, ' "$revision"
  printf '"Env": [%s]}}\n' "$(json_string "SOURCE_REVISION=$GITHUB_SHA")"
elif [ "$#" -ge 2 ] && [ "$1" = buildx ] && [ "$2" = rm ]; then
  printf 'EXISTING=value\n' | /usr/bin/cmp -s - "$root/environment" || exit 1
else
  exit 92
fi
"#,
    );
    let mut native = Native::new(
        &fixture.context.root,
        &fixture.context.environment,
        signals(),
    );
    execute::build(&fixture.context, &mut native).await.unwrap();
    assert_eq!(
        fixture.output(),
        format!("EXISTING=value\nPRODUCTION_IMAGE={IMAGE}\n")
    );
    let events: Vec<Value> = fs::read_to_string(fixture.root.join("events"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(events.len(), 8);
    assert_eq!(events[5][1][1], "rm");
    assert!(!fixture.root.join("literal").exists());
}

#[tokio::test]
async fn actual_command_exit_spawn_refusals_and_private_capture_fail_closed() {
    let mut fixture = Fixture::new();
    executable(&mut fixture, "printf 'PRIVATE-SENTINEL\\n' >&2\nexit 42");
    let mut native = Native::new(
        &fixture.context.root,
        &fixture.context.environment,
        signals(),
    );
    let error = native
        .run(Command::capture(Program::Docker, &["fixture"]))
        .await
        .unwrap_err();
    assert_eq!(error.code, 42);
    assert!(!error.message.contains("PRIVATE-SENTINEL"));
    fs::set_permissions(
        fixture.root.join("fake commands/docker"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    assert_eq!(
        native
            .run(Command::capture(Program::Docker, &["fixture"]))
            .await
            .unwrap_err()
            .code,
        126
    );
    fs::remove_file(fixture.root.join("fake commands/docker")).unwrap();
    fixture.context.environment.insert(
        "PATH".into(),
        fixture.root.join("fake commands").into_os_string(),
    );
    let mut native = Native::new(
        &fixture.context.root,
        &fixture.context.environment,
        signals(),
    );
    assert_eq!(
        native
            .run(Command::capture(Program::Docker, &["fixture"]))
            .await
            .unwrap_err()
            .code,
        127
    );
}

#[tokio::test]
async fn timeout_and_interrupt_preserve_status_and_retire_owned_group() {
    for code in [124, 130, 143] {
        let mut fixture = Fixture::new();
        executable(
            &mut fixture,
            r#"
if [ "$#" -eq 1 ] && [ "$1" = cleanup ]; then
  printf done >"$root/cleanup-ready"
  exit 0
fi
trap '' TERM INT
printf '%s' "$$" >"$root/ready"
while :; do /bin/sleep 0.01; done
"#,
        );
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut native = Native::new(
            &fixture.context.root,
            &fixture.context.environment,
            Signals::injected(receiver),
        );
        let mut request = Command::capture(Program::Docker, &["fixture"]);
        request.grace = Duration::from_millis(30);
        request.timeout = Duration::from_secs(3);
        if code == 124 {
            request.timeout = Duration::from_millis(200);
        }
        let signal = async {
            wait(&fixture.root.join("ready")).await;
            if code != 124 {
                sender.send(code).unwrap();
            }
        };
        let started = Instant::now();
        let (result, ()) = tokio::join!(native.run(request), signal);
        assert_eq!(result.unwrap_err().code, code);
        assert!(started.elapsed() < Duration::from_secs(4));
        let pid: i32 = fs::read_to_string(fixture.root.join("ready"))
            .unwrap()
            .parse()
            .unwrap();
        assert!(
            rustix::process::test_kill_process(rustix::process::Pid::from_raw(pid).unwrap())
                .is_err()
        );
        if code != 124 {
            assert_eq!(
                native
                    .run(Command::capture(Program::Docker, &["blocked"]))
                    .await
                    .unwrap_err()
                    .code,
                code
            );
            let mut cleanup = Command::capture(Program::Docker, &["cleanup"]);
            cleanup.cleanup = true;
            cleanup.timeout = Duration::from_secs(3);
            cleanup.grace = Duration::from_millis(20);
            native.run(cleanup).await.unwrap();
            assert!(fixture.root.join("cleanup-ready").exists());
            assert_eq!(
                fs::read_to_string(fixture.root.join("events"))
                    .unwrap()
                    .lines()
                    .count(),
                2
            );
        }
    }
}

#[tokio::test]
async fn output_bound_failure_reclaims_process_before_return_and_never_echoes_bytes() {
    let mut fixture = Fixture::new();
    executable(
        &mut fixture,
        r#"
printf '%s' "$$" >"$root/ready"
trap '' PIPE
repeat PRIVATE-SENTINEL 10000 || exit 1
while :; do /bin/sleep 0.01; done
"#,
    );
    let mut native = Native::new(
        &fixture.context.root,
        &fixture.context.environment,
        signals(),
    );
    let mut command = Command::capture(Program::Docker, &["fixture"]);
    command.capture = Some(16);
    command.grace = Duration::from_millis(30);
    let error = native.run(command).await.unwrap_err();
    assert_eq!(error.code, 1);
    assert!(!error.message.contains("PRIVATE-SENTINEL"));
    let pid: i32 = fs::read_to_string(fixture.root.join("ready"))
        .unwrap()
        .parse()
        .unwrap();
    assert!(
        rustix::process::test_kill_process(rustix::process::Pid::from_raw(pid).unwrap()).is_err()
    );
}

#[tokio::test]
async fn exited_leader_descendants_are_retired_without_signaling_unrelated_group() {
    use std::os::unix::process::CommandExt;
    let mut unrelated_command = std::process::Command::new("/bin/sleep");
    unrelated_command.arg("30").process_group(0);
    let mut unrelated = unrelated_command.spawn().unwrap();
    let mut fixture = Fixture::new();
    executable(
        &mut fixture,
        r#"
(
  trap '' TERM
  exec >&- 2>&-
  while :; do /bin/sleep 0.01; done
) &
printf '%s' "$!" >"$root/descendant"
"#,
    );
    let mut native = Native::new(
        &fixture.context.root,
        &fixture.context.environment,
        signals(),
    );
    let result = native
        .run(Command::capture(Program::Docker, &["fixture"]))
        .await;
    let unrelated_survived = unrelated.try_wait().unwrap().is_none();
    unrelated.kill().unwrap();
    unrelated.wait().unwrap();
    result.unwrap();
    assert!(unrelated_survived);
    let pid: i32 = fs::read_to_string(fixture.root.join("descendant"))
        .unwrap()
        .parse()
        .unwrap();
    // Darwin may retain an already-dead orphan zombie briefly; the production
    // cleanup proof separately rejects live descendants and permits only zombies.
    let output = std::process::Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .unwrap();
    let status = String::from_utf8(output.stdout).unwrap();
    assert!(status.trim().is_empty() || status.trim_start().starts_with('Z'));
}

#[tokio::test]
async fn interrupted_real_fake_verification_reclaims_inspected_container_after_cli_exit() {
    let mut fixture = Fixture::new();
    fixture.context.image = IMAGE.into();
    executable(
        &mut fixture,
        r#"
state=$root/container.json
container_id=$(repeat d 64)
last=
if [ "$#" -gt 0 ]; then eval "last=\${$#}"; fi
if [ "$name" = git ]; then
  [ "$#" -ge 1 ] || exit 1
  if [ "$1" = rev-parse ]; then printf '%s\n' "${GITHUB_SHA?}"; fi
elif [ "$#" -ge 2 ] && [ "$1" = image ] && [ "$2" = inspect ]; then
  image=$(json_string "${PRODUCTION_IMAGE?}")
  revision=$(json_string "${GITHUB_SHA?}")
  printf '{"Id": %s, "Config": {"User": "10001:10001", ' "$image"
  printf '"Labels": {"org.opencontainers.image.revision": %s}, ' "$revision"
  printf '"Env": [%s]}}\n' "$(json_string "SOURCE_REVISION=$GITHUB_SHA")"
elif [ "$#" -ge 2 ] && [ "$1" = container ] && [ "$2" = create ]; then
  container=$(value --name "$@") || exit 1
  token=${container#simplestchat-ci-verify-}
  holds "${RUNNER_TEMP?}/production-verification-$token/ownership.json" container \
    "$container" || exit 1
  owner=$(json_string "$container")
  {
    printf '{"Id": "%s", "Name": %s, ' "$container_id" "$(json_string "/$container")"
    printf '"Image": %s, ' "$(json_string "${PRODUCTION_IMAGE?}")"
    printf '"Config": {"Labels": {"com.simplestchat.ci.production-verification": %s}}, ' "$owner"
    printf '"State": {"Running": false, "Status": "created", "ExitCode": 0}}'
  } >"$state"
  printf '%s\n' "$container_id"
elif [ "$#" -ge 2 ] && [ "$1" = container ] && [ "$2" = inspect ]; then
  [ "$last" = "$container_id" ] || exit 1
  /bin/cat -- "$state" || exit 1
  printf '\n'
elif [ "$#" -ge 2 ] && [ "$1" = container ] && [ "$2" = start ]; then
  [ "$#" -eq 4 ] && [ "$3" = --attach ] && [ "$4" = "$container_id" ] || exit 1
  printf started >"$root/ready"
  while :; do /bin/sleep 0.01; done
elif [ "$#" -ge 2 ] && [ "$1" = container ] && [ "$2" = ls ]; then
  if [ -e "$state" ]; then
    member "id=$container_id" "$@" || exit 1
    printf '%s\n' "$container_id"
  fi
elif [ "$#" -ge 2 ] && [ "$1" = container ] && [ "$2" = rm ]; then
  [ "$#" -eq 4 ] && [ "$3" = --force ] && [ "$4" = "$container_id" ] || exit 1
  /bin/rm -- "$state" || exit 1
else
  exit 93
fi
"#,
    );
    fixture
        .context
        .environment
        .insert("PRODUCTION_IMAGE".into(), IMAGE.into());
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut native = Native::new(
        &fixture.context.root,
        &fixture.context.environment,
        Signals::injected(receiver),
    );
    let cancel = async {
        wait(&fixture.root.join("ready")).await;
        sender.send(143).unwrap();
    };
    let (result, ()) = tokio::join!(execute::verify(&fixture.context, &mut native), cancel);
    assert_eq!(result.unwrap_err().code, 143);
    assert!(!fixture.root.join("container.json").exists());
    assert_eq!(fixture.output(), "EXISTING=value\n");
    let events: Vec<Value> = fs::read_to_string(fixture.root.join("events"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(events.iter().any(|event| event[1][1] == "rm"));
}
