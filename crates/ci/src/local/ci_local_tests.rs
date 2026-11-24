//! Actual orchestration with fake allowlisted tools; no engine or VM is touched.

use super::*;
use serde_json::{Value, json};
use std::os::unix::fs::{PermissionsExt, symlink};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

static NEXT: AtomicUsize = AtomicUsize::new(0);
const REVISION: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const BASE: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
/// Every fake tool, run as `sh DISPATCH TOOL ARGUMENT…`. Each call appends one
/// event to the log; `CI_FIXTURE_*` variables select the answers. The fake
/// `podman machine ssh` runs the guest script's capability probe, the lines
/// before its first `test -x /mnt/rosetta`, with the fake `od` of
/// `CI_FIXTURE_GUEST_TOOLS`.
const DISPATCH: &str = r#"
tool=$1
shift
newline='
'
log=${CI_FIXTURE_LOG?}
seen=$log.act-seen
cwd=$(pwd -P)
event="{\"tool\": $(json_string "$tool"), \"args\": $(json_strings "$@")"
event="$event, \"dockerHost\": $(json_string "${DOCKER_HOST-}"), \"cwd\": $(json_string "$cwd")}"
printf '%s\n' "$event" >>"$log"
enabled() { eval "[ -n \"\${CI_FIXTURE_$1-}\" ]"; }
member() {
  member_word=$1
  shift
  for member_item in "$@"; do
    if [ "$member_item" = "$member_word" ]; then return 0; fi
  done
  return 1
}
value() {
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
}
last=
if [ "$#" -gt 0 ]; then eval "last=\${$#}"; fi
phase=$tool
case $tool in
  git)
    [ "$#" -ge 1 ] || exit 1
    phase=$tool-$1
    ;;
  podman)
    [ "$#" -ge 2 ] || exit 1
    phase=$tool-$2
    ;;
  curl)
    [ "$#" -ge 1 ] || exit 1
    case $last in
      */_ping) phase=curl-ping ;;
      */networks) phase=curl-networks ;;
      *) phase=curl-containers ;;
    esac
    ;;
esac
if [ "${CI_FIXTURE_FAIL-}" = "$phase" ]; then exit "${CI_FIXTURE_FAIL_CODE-43}"; fi
case $tool in
git)
  case $1 in
    status)
      if enabled DIRTY || { enabled DIRTY_AFTER && [ -e "$seen" ]; }; then
        printf ' M source.js\n'
      fi
      ;;
    merge-base)
      [ "$#" -ge 2 ] || exit 1
      if [ "$2" = --is-ancestor ]; then
        if enabled NONANCESTOR; then exit 1; else exit 0; fi
      fi
      if enabled DEFAULT_HEAD_BASE; then repeat a 40; else repeat b 40; fi
      printf '\n'
      ;;
    rev-parse)
      if [ "$last" = HEAD ]; then
        if enabled CHANGED && [ -e "$seen" ]; then repeat c 40; else repeat a 40; fi
      elif enabled INVALID_BASE; then
        exit 128
      elif enabled SAME_BASE; then
        repeat a 40
      else
        repeat b 40
      fi
      printf '\n'
      ;;
    *) exit 93 ;;
  esac
  ;;
go)
  [ "$#" -eq 4 ] && [ "$1" = env ] && [ "$2" = GOVERSION ] && [ "$3" = GOOS ] &&
    [ "$4" = GOARCH ] || exit 1
  printf 'go1.27.1\ndarwin\narm64\n'
  ;;
uname)
  if [ "$#" -eq 1 ] && [ "$1" = -m ]; then
    printf '%s\n' "${CI_FIXTURE_ARCH-arm64}"
  else
    printf '%s\n' "${CI_FIXTURE_OS-Darwin}"
  fi
  ;;
curl)
  if enabled ENGINE_DOWN; then exit 7; fi
  networks='[{"Name":"bridge","IPAM":{"Config":[{"Gateway":"10.88.0.1"}]}}]'
  if [ "${CI_FIXTURE_NETWORKS+set}" = set ]; then networks=$CI_FIXTURE_NETWORKS; fi
  case $phase in
    curl-ping) printf 'OK\n' ;;
    curl-networks) printf '%s\n' "$networks" ;;
    *)
      if enabled BUSY || { enabled BUSY_AFTER && [ -e "$seen" ]; }; then
        printf '[{"Id":"existing"}]\n'
      else
        printf '%s\n' "${CI_FIXTURE_CONTAINERS-[]}"
      fi
      ;;
  esac
  ;;
podman)
  [ "$1" = machine ] || exit 1
  case $2 in
    inspect)
      if ! member --format "$@"; then
        if enabled NEW_VM; then exit 1; fi
      elif member '{{.State}}' "$@"; then
        if enabled STOPPED; then printf 'stopped\n'; else printf 'running\n'; fi
      else
        printf '/owned-ci/socket\n'
      fi
      ;;
    list)
      if enabled OTHER_VM; then
        printf 'other-project true\n'
      else
        printf 'simplestchat-ci false\n'
      fi
      ;;
    init | start | stop) ;;
    ssh)
      script=$(/bin/cat; printf x) || exit 1
      script=${script%x}
      printf '%s' "$script" >"$log.guest"
      probe=${script%%"$newline"'test -x /mnt/rosetta'*}
      if ! printf '%s\n' "$probe" |
        PATH=${CI_FIXTURE_GUEST_TOOLS?} /bin/sh -se 2>"$log.guest-stderr"; then
        /bin/mv "$log.guest-stderr" "$log.guest-error"
        exit 1
      fi
      /bin/rm -f "$log.guest-stderr"
      if enabled EMULATOR_FAIL; then exit 69; fi
      : >"$log.emulator-selected"
      ;;
    *) exit 94 ;;
  esac
  ;;
act)
  : >"$seen"
  printf 'fixture workflow stdout\n'
  printf 'fixture workflow stderr\n' >&2
  event_path=$(value --eventpath "$@") || exit 1
  output=${event_path%/*}
  if enabled ACT_WAIT; then
    # An inert descendant proves cancellation reaches the owned group. A
    # background shell cannot catch SIGINT, so this leader hands either
    # signal on as SIGTERM before it ends by the same signal.
    child=
    trap 'kill -TERM "$child" 2>/dev/null; trap - INT; kill -INT "$$"' INT
    trap 'kill -TERM "$child" 2>/dev/null; trap - TERM; kill -TERM "$$"' TERM
    (
      sleeper=
      trap 'kill "$sleeper" 2>/dev/null; : >"$log.child-stopped"; exit 0' TERM
      : >"$log.child-ready"
      while :; do
        /bin/sleep 10 >/dev/null 2>&1 &
        sleeper=$!
        wait "$sleeper"
      done
    ) &
    child=$!
    printf '%s' "$child" >"$log.child-pid"
    while :; do /bin/sleep 0.01; done
  fi
  if ! enabled NO_RECEIPT; then
    event=$newline$(/bin/cat -- "$event_path")$newline || exit 1
    field() {
      field_rest=${event#*"$newline  \"$1\": \""}
      [ "$field_rest" != "$event" ] || return 1
      printf '%s' "${field_rest%%\"*}"
    }
    revision=$(field after) && base=$(field before) || exit 1
    run_id=
    found=false
    for argument in "$@"; do
      case $argument in
        LOCAL_CI_RUN_ID=*)
          run_id=${argument#LOCAL_CI_RUN_ID=}
          found=true
          break
          ;;
      esac
    done
    $found || exit 1
    if enabled STALE_RECEIPT; then run_id=$(repeat f 32); fi
    evidence=$output/checks
    /bin/mkdir -- "$evidence/receipts" || exit 1
    check=browser-fixture
    identity="\"runId\": $(json_string "$run_id"), \"revision\": $(json_string "$revision")"
    identity="$identity, \"base\": $(json_string "$base")"
    receipt="{\"schema\": 1, \"status\": \"passed\", \"check\": \"$check\", $identity"
    if enabled EXTRA_RECEIPT_KEY; then receipt="$receipt, \"unrecognized\": true"; fi
    if ! enabled MISSING_MATRIX; then
      printf '%s}' "$receipt" >"$evidence/receipts/$check.json"
    fi
    digest() {
      if [ -x /usr/bin/sha256sum ]; then
        digest_line=$(/usr/bin/sha256sum <"$1") || return 1
      else
        digest_line=$(/usr/bin/shasum -a 256 <"$1") || return 1
      fi
      printf '%s' "${digest_line%% *}"
    }
    ci=$(digest .github/workflows/ci.yml) &&
      security=$(digest .github/workflows/security.yml) &&
      codeql=$(digest .github/workflows/codeql.yml) || exit 1
    if enabled BAD_HASH; then ci=$(repeat 0 64); fi
    if enabled BAD_ID; then
      checks='["../../outside"]'
    elif enabled DUPLICATE; then
      checks="[\"$check\", \"$check\"]"
    else
      checks="[\"$check\"]"
    fi
    workflows="{\"ci.yml\": \"$ci\", \"security.yml\": \"$security\", \"codeql.yml\": \"$codeql\"}"
    printf '{"schema": 1, "status": "passed", "gates": ["fixture"], "checks": %s, ' "$checks" \
      >"$evidence/required.json"
    printf '"workflows": %s, %s}' "$workflows" "$identity" >>"$evidence/required.json"
  fi
  if enabled EXISTING_SUMMARY; then printf 'keep this evidence' >"$output/summary.json"; fi
  if enabled REPLACE_LOCK; then
    lock=target/act/run.lock
    /bin/mv -- "$lock" target/act/original-lock && /bin/mkdir -- "$lock" || exit 1
    printf untouched >"$lock/other-owner"
  fi
  if enabled REPLACE_OUTPUT; then
    /bin/mv -- "$output" "$output.original" && /bin/mkdir -- "$output.other" || exit 1
    printf untouched >"$output.other/marker"
    /bin/ln -s -- "$output.other" "$output" || exit 1
  fi
  exit "${CI_FIXTURE_ACT_EXIT-0}"
  ;;
*) exit 95 ;;
esac
"#;

/// The guest's `od`: the auxiliary vector of a guest whose `AT_HWCAP` and
/// `AT_HWCAP2` are `CI_FIXTURE_HWCAP` and `CI_FIXTURE_HWCAP2` (zero when
/// unset), between two other entries and the terminating one.
const GUEST_OD: &str = r#"#!/bin/sh
[ "$#" -eq 5 ] && [ "$1" = -An ] && [ "$2" = -v ] && [ "$3" = -t ] && [ "$4" = x8 ] &&
  [ "$5" = /proc/self/auxv ] || exit 1
printf ' %016x %016x\n' 6 4096 16 "$((${CI_FIXTURE_HWCAP-0}))" 33 0 \
  26 "$((${CI_FIXTURE_HWCAP2-0}))" 0 0
"#;

struct Fixture {
    temporary: PathBuf,
    root: PathBuf,
    bin: PathBuf,
    log: PathBuf,
    environment: Environment,
}

struct Outcome {
    code: u8,
    events: Vec<Value>,
    output: Option<PathBuf>,
    summary: Option<Value>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // The only possible process here is an inert child made by our fake act.
        if let Ok(pid) = fs::read_to_string(self.log.with_extension("jsonl.child-pid")) {
            let _ = Command::new("/bin/kill")
                .args(["-KILL", "--", pid.trim()])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        }
        let _ = fs::remove_dir_all(&self.temporary);
    }
}

impl Fixture {
    fn new() -> Self {
        let temporary = std::env::temp_dir().join(format!(
            "simplestchat-ci-local-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&temporary).unwrap();
        let temporary = fs::canonicalize(temporary).unwrap();
        let root = temporary.join("checkout with spaces");
        let bin = temporary.join("fake commands");
        let log = temporary.join("calls.jsonl");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir(&bin).unwrap();
        let dispatcher = temporary.join("fake tools");
        fs::write(
            &dispatcher,
            format!("{}{DISPATCH}", crate::fake_shell::PRELUDE),
        )
        .unwrap();
        for tool in ["git", "curl", "go", "uname", "podman", "act"] {
            let path = bin.join(tool);
            fs::write(
                &path,
                format!(
                    "#!/bin/sh\nexec /bin/sh {} {} \"$@\"\n",
                    quote(dispatcher.to_str().unwrap()),
                    quote(tool)
                ),
            )
            .unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let guest = temporary.join("guest tools");
        fs::create_dir(&guest).unwrap();
        fs::write(guest.join("od"), GUEST_OD).unwrap();
        fs::set_permissions(guest.join("od"), fs::Permissions::from_mode(0o755)).unwrap();
        let environment = [
            ("PATH", bin.as_os_str()),
            ("CI_FIXTURE_LOG", log.as_os_str()),
            ("CI_FIXTURE_BIN", bin.as_os_str()),
            ("CI_FIXTURE_GUEST_TOOLS", guest.as_os_str()),
            ("LC_ALL", OsStr::new("C")),
        ]
        .into_iter()
        .map(|(key, value)| (key.into(), value.to_owned()))
        .collect();
        let fixture = Self {
            temporary,
            root,
            bin,
            log,
            environment,
        };
        for name in ["ci.yml", "security.yml", "codeql.yml"] {
            fixture.write(
                &format!(".github/workflows/{name}"),
                &format!("name: {name}\n"),
            );
        }
        fixture.warm_act_cache();
        fixture
    }

    /// Give the checkout a pinned act build whose cache entry is intact, so
    /// preparation returns the fake runner without a compiler or network.
    fn warm_act_cache(&self) {
        use sha2::{Digest, Sha256};
        let hexadecimal = |bytes: &[u8]| -> String {
            let digest = Sha256::digest(bytes);
            digest.iter().map(|byte| format!("{byte:02x}")).collect()
        };
        let patch = "reviewed fixture patch\n";
        let pin = json!({"patchSha256": hexadecimal(patch.as_bytes())}).to_string();
        self.write("build/ci-local-act.patch", patch);
        self.write("build/ci-local-act.json", &pin);
        let key = hexadecimal(format!("{pin}go1.27.1\ndarwin\narm64\n").as_bytes());
        let target = self.root.join(".cache/ci-act").join(&key);
        fs::create_dir_all(&target).unwrap();
        let act = fs::read(self.bin.join("act")).unwrap();
        fs::write(target.join("act"), &act).unwrap();
        fs::set_permissions(target.join("act"), fs::Permissions::from_mode(0o755)).unwrap();
        let receipt = json!({"inputSha256": key, "binarySha256": hexadecimal(&act)});
        fs::write(target.join("receipt.json"), receipt.to_string()).unwrap();
    }

    fn write(&self, path: &str, contents: &str) {
        let path = self.root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    async fn run_async(
        &self,
        args: &[&str],
        overrides: &[(&str, &str)],
        signals: Signals,
    ) -> Outcome {
        fs::write(&self.log, []).unwrap();
        for suffix in ["act-seen", "guest", "guest-error", "emulator-selected"] {
            let _ = fs::remove_file(format!("{}.{suffix}", self.log.display()));
        }
        let mut environment = self.environment.clone();
        environment.extend(
            overrides
                .iter()
                .map(|(key, value)| ((*key).into(), (*value).into())),
        );
        let arguments = args.iter().map(|arg| (*arg).into()).collect::<Vec<_>>();
        let result = run_with_env(&self.root, &arguments, environment, signals).await;
        let code = result.unwrap_or_else(|error| error.code);
        let events = fs::read_to_string(&self.log)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect::<Vec<Value>>();
        let output = events
            .iter()
            .find(|event| event["tool"] == "act")
            .map(|event| {
                let arguments = event["args"].as_array().unwrap();
                let index = arguments
                    .iter()
                    .position(|value| value == "--eventpath")
                    .unwrap();
                PathBuf::from(arguments[index + 1].as_str().unwrap())
                    .parent()
                    .unwrap()
                    .to_path_buf()
            })
            .or_else(|| {
                let selected = args
                    .iter()
                    .position(|arg| *arg == "--output")
                    .and_then(|index| args.get(index + 1))
                    .map(|path| self.root.join(path));
                selected.filter(|path| path.is_dir()).or_else(|| {
                    fs::read_dir(self.root.join("results"))
                        .ok()?
                        .filter_map(|entry| entry.ok())
                        .map(|entry| entry.path())
                        .filter(|path| path.is_dir())
                        .max()
                })
            });
        let summary = output
            .as_ref()
            .and_then(|path| fs::read(path.join("summary.json")).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok());
        Outcome {
            code,
            events,
            output,
            summary,
        }
    }

    fn run(&self, args: &[&str], overrides: &[(&str, &str)]) -> Outcome {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (_sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        runtime.block_on(self.run_async(args, overrides, Signals::injected(receiver)))
    }
}

fn tools(outcome: &Outcome, tool: &str) -> Vec<Value> {
    outcome
        .events
        .iter()
        .filter(|event| event["tool"] == tool)
        .cloned()
        .collect()
}

fn arguments(event: &Value) -> Vec<String> {
    event["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap().to_owned())
        .collect()
}

fn option(event: &Value, name: &str) -> String {
    let args = arguments(event);
    args[args.iter().position(|arg| arg == name).unwrap() + 1].clone()
}

fn summary(outcome: &Outcome) -> &Value {
    outcome
        .summary
        .as_ref()
        .expect("owned output has a summary")
}

#[test]
fn full_arm_run_uses_exact_workflow_pins_resources_private_inputs_and_receipts() {
    let fixture = Fixture::new();
    let outcome = fixture.run(&[], &[]);
    assert_eq!(outcome.code, 0);
    let acts = tools(&outcome, "act");
    let act = &acts[0];
    assert_eq!(option(act, "--job"), "required");
    assert_eq!(option(act, "--workflows"), ".github/workflows/ci.yml");
    assert_eq!(option(act, "--container-architecture"), "");
    assert_eq!(
        option(act, "--platform"),
        format!("ubuntu-24.04={}", engine::ARM64_IMAGE)
    );
    assert_eq!(
        arguments(act)
            .iter()
            .filter(|arg| *arg == "--platform")
            .count(),
        1
    );
    assert_eq!(option(act, "--container-daemon-socket"), "-");
    assert_eq!(option(act, "--network"), "bridge");
    assert_eq!(option(act, "--concurrent-jobs"), "4");
    for name in ["--env-file", "--secret-file", "--var-file", "--input-file"] {
        assert_eq!(option(act, name), "/dev/null");
    }
    assert_eq!(act["dockerHost"], "unix:///owned-ci/socket");
    let args = arguments(act);
    for flag in [
        "--rm",
        "--use-new-action-cache=true",
        "--pull=false",
        "--watch=false",
        "--reuse=false",
    ] {
        assert!(args.iter().any(|arg| arg == flag));
    }
    assert!(args.contains(&format!("LOCAL_CI_RUNNER_IMAGE={}", engine::ARM64_IMAGE)));
    let options = option(act, "--container-options");
    for required in [
        "--privileged",
        "--cgroupns=private",
        "--cpus=3 --memory=16g",
        "--pids-limit=2048",
        "host.docker.internal:10.88.0.1",
        "target=/local-ci-evidence",
    ] {
        assert!(options.contains(required));
    }
    assert_eq!(tools(&outcome, "go").len(), 1);
    let event: Value =
        serde_json::from_slice(&fs::read(option(act, "--eventpath")).unwrap()).unwrap();
    assert_eq!(event["before"], BASE);
    assert_eq!(event["after"], REVISION);
    assert_eq!(event["local_ci"], true);
    assert!(event.get("local_ci_parallel").is_none());
    let receipt = summary(&outcome);
    assert_eq!(receipt["completeLocalGate"], true);
    assert_eq!(receipt["revision"], REVISION);
    assert_eq!(receipt["base"], BASE);
    assert_eq!(receipt["hostedOnly"].as_array().unwrap().len(), 2);
    assert_eq!(receipt["runnerPlatform"], "linux/arm64");
    assert_eq!(receipt["productionPlatform"], "linux/amd64");
    assert_eq!(receipt["vendorScanning"], "excluded");
    let empty = outcome
        .events
        .iter()
        .position(|event| {
            event["tool"] == "curl"
                && arguments(event).contains(&"http://localhost/containers/json?all=1".into())
        })
        .unwrap();
    let emulator = outcome
        .events
        .iter()
        .position(|event| {
            event["tool"] == "podman" && arguments(event).get(1).is_some_and(|arg| arg == "ssh")
        })
        .unwrap();
    assert!(emulator > empty);
    let script = fs::read_to_string(format!("{}.guest", fixture.log.display())).unwrap();
    for required in [
        "test -x /mnt/rosetta",
        "grep -qx enabled /proc/sys/fs/binfmt_misc/rosetta",
        "grep -qx disabled /proc/sys/fs/binfmt_misc/qemu-x86_64",
    ] {
        assert!(script.contains(required));
    }
    assert!(Path::new(&format!("{}.emulator-selected", fixture.log.display())).exists());
    assert!(!fixture.root.join("target/act/run.lock").exists());
}

#[test]
fn selected_job_accepts_dirty_source_and_matrix_without_claiming_complete_gate() {
    let fixture = Fixture::new();
    let outcome = fixture.run(
        &["browser", "--matrix", "group:accounts"],
        &[("CI_FIXTURE_DIRTY", "1")],
    );
    assert_eq!(outcome.code, 0);
    let act = &tools(&outcome, "act")[0];
    assert_eq!(option(act, "--job"), "browser");
    assert_eq!(option(act, "--matrix"), "group:accounts");
    assert_eq!(summary(&outcome)["completeLocalGate"], false);
}

#[test]
fn tracking_is_a_fresh_run_nonce_despite_ambient_values() {
    let fixture = Fixture::new();
    let mut tracking = std::collections::BTreeSet::new();
    for ambient in ["", "untrusted:caller/tracking", "ambient_valid_tracking"] {
        let outcome = fixture.run(&[], &[("RUNNER_TRACKING_ID", ambient)]);
        assert_eq!(outcome.code, 0);
        let args = arguments(&tools(&outcome, "act")[0]);
        let explicit = args
            .windows(2)
            .filter(|pair| pair[0] == "--env")
            .map(|pair| pair[1].clone())
            .collect::<Vec<_>>();
        let ids = explicit
            .iter()
            .filter_map(|value| value.strip_prefix("RUNNER_TRACKING_ID="))
            .collect::<Vec<_>>();
        assert_eq!(ids.len(), 1);
        assert_ne!(ids[0], ambient);
        let id = ids[0].strip_prefix("local-ci-").unwrap();
        assert_eq!(id.len(), 32);
        assert!(id.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert!(explicit.contains(&format!("LOCAL_CI_RUN_ID={id}")));
        assert!(tracking.insert(ids[0].to_owned()));
    }
}

#[test]
fn help_and_full_gate_guards_refuse_before_any_engine_action() {
    for (args, overrides) in [
        (vec!["all", "--matrix", "group:accounts"], vec![]),
        (vec![], vec![("CI_FIXTURE_DIRTY", "1")]),
        (vec![], vec![("CI_FIXTURE_SAME_BASE", "1")]),
        (vec![], vec![("CI_FIXTURE_NONANCESTOR", "1")]),
        (vec![], vec![("CI_FIXTURE_INVALID_BASE", "1")]),
        (vec!["all", "--jobs", "1"], vec![]),
        (vec!["all", "--codeql", "/single-isa-bundle"], vec![]),
        (vec!["all", "--job", "web"], vec![]),
        (vec!["release-security"], vec![]),
        (vec!["release-prepare"], vec![]),
        (vec!["required"], vec![]),
        (vec!["Bad_job"], vec![]),
        (vec!["--base"], vec![]),
    ] {
        let fixture = Fixture::new();
        let outcome = fixture.run(&args, &overrides);
        assert_eq!(outcome.code, 2, "{args:?} {overrides:?}");
        assert!(outcome.events.iter().all(|event| {
            !["act", "podman", "curl"]
                .iter()
                .any(|tool| event["tool"] == *tool)
        }));
    }
    let fixture = Fixture::new();
    let outcome = fixture.run(&["--help"], &[]);
    assert_eq!(outcome.code, 0);
    assert!(outcome.events.is_empty());
}

#[test]
fn missing_prerequisites_refuse_without_falling_back_to_host_tools() {
    for tool in ["curl", "git", "go"] {
        let fixture = Fixture::new();
        fs::remove_file(fixture.bin.join(tool)).unwrap();
        let outcome = fixture.run(&[], &[]);
        assert_eq!(outcome.code, 2);
        assert!(outcome.events.is_empty());
    }
}

#[test]
fn owned_vm_is_stopped_only_when_started_here_including_failed_workflows() {
    for stopped in [false, true] {
        let fixture = Fixture::new();
        let overrides = if stopped {
            vec![("CI_FIXTURE_STOPPED", "1"), ("CI_FIXTURE_ACT_EXIT", "37")]
        } else {
            vec![("CI_FIXTURE_ACT_EXIT", "37")]
        };
        let outcome = fixture.run(&[], &overrides);
        assert_eq!(outcome.code, 37);
        assert_eq!(summary(&outcome)["status"], "failed");
        assert_eq!(summary(&outcome)["completeLocalGate"], false);
        let commands = tools(&outcome, "podman");
        for action in ["start", "stop"] {
            assert_eq!(
                commands
                    .iter()
                    .filter(|event| arguments(event).get(1).is_some_and(|value| value == action))
                    .count(),
                usize::from(stopped)
            );
        }
        assert_eq!(
            tools(&outcome, "curl")
                .iter()
                .filter(|event| arguments(event)
                    .last()
                    .is_some_and(|value| value.ends_with("/containers/json?all=1")))
                .count(),
            2
        );
    }
}

#[test]
fn new_vm_resources_are_exact_and_existing_vms_are_never_resized() {
    for create in [false, true] {
        let fixture = Fixture::new();
        let overrides = if create {
            vec![("CI_FIXTURE_NEW_VM", "1"), ("CI_FIXTURE_STOPPED", "1")]
        } else {
            vec![]
        };
        let outcome = fixture.run(&[], &overrides);
        assert_eq!(outcome.code, 0);
        let commands = tools(&outcome, "podman");
        let initializers = commands
            .iter()
            .filter(|event| arguments(event).get(1).is_some_and(|value| value == "init"))
            .collect::<Vec<_>>();
        assert_eq!(initializers.len(), usize::from(create));
        if create {
            assert_eq!(
                arguments(initializers[0]),
                [
                    "machine",
                    "init",
                    "--rootful",
                    "--cpus",
                    "12",
                    "--memory",
                    "81920",
                    "--disk-size",
                    "80",
                    "simplestchat-ci"
                ]
            );
        }
        assert!(
            commands
                .iter()
                .all(|event| !arguments(event).contains(&"set".into()))
        );
        assert!(commands.iter().all(|event| {
            arguments(event)
                .iter()
                .all(|value| value != "other-project")
        }));
    }
}

#[test]
fn other_vms_and_existing_containers_are_left_untouched() {
    for overrides in [
        vec![("CI_FIXTURE_STOPPED", "1"), ("CI_FIXTURE_OTHER_VM", "1")],
        vec![("CI_FIXTURE_BUSY", "1")],
    ] {
        let fixture = Fixture::new();
        let outcome = fixture.run(&[], &overrides);
        assert_ne!(outcome.code, 0);
        assert!(tools(&outcome, "act").is_empty());
        assert!(tools(&outcome, "podman").iter().all(|event| {
            !arguments(event)
                .get(1)
                .is_some_and(|action| ["start", "stop", "ssh"].contains(&action.as_str()))
        }));
    }
}

#[test]
fn explicit_engines_need_disposable_intent_and_a_local_unix_socket() {
    for (args, overrides) in [
        (vec![], vec![("DOCKER_HOST", "unix:///shared/socket")]),
        (
            vec!["all", "--disposable-engine"],
            vec![("DOCKER_HOST", "tcp://example.test:2375")],
        ),
        (vec!["all", "--disposable-engine"], vec![]),
        (vec![], vec![("CI_FIXTURE_OS", "Linux")]),
    ] {
        let fixture = Fixture::new();
        let outcome = fixture.run(&args, &overrides);
        assert_eq!(outcome.code, 2);
        assert!(tools(&outcome, "act").is_empty());
        assert!(tools(&outcome, "curl").is_empty());
    }
    let fixture = Fixture::new();
    let outcome = fixture.run(
        &["all", "--disposable-engine"],
        &[
            ("DOCKER_HOST", "unix:///owned/socket"),
            ("CI_FIXTURE_OS", "Linux"),
        ],
    );
    assert_eq!(outcome.code, 0);
    assert!(tools(&outcome, "podman").is_empty());
    assert_eq!(summary(&outcome)["runnerPlatform"], "linux/amd64");
    assert_eq!(summary(&outcome)["productionPlatform"], "linux/amd64");
    assert_eq!(summary(&outcome)["vendorScanning"], "excluded");
    assert_eq!(
        option(&tools(&outcome, "act")[0], "--platform"),
        format!("ubuntu-24.04={}", engine::AMD64_IMAGE)
    );
}

#[test]
fn emulator_failure_never_starts_workflow() {
    let fixture = Fixture::new();
    let outcome = fixture.run(&[], &[("CI_FIXTURE_EMULATOR_FAIL", "1")]);
    assert_eq!(outcome.code, 69);
    assert_eq!(summary(&outcome)["completeLocalGate"], false);
    assert!(tools(&outcome, "act").is_empty());
}

#[test]
fn actual_guest_probe_rejects_inconsistent_cpu_flags_before_emulator_selection() {
    for (hwcap, hwcap2, passed) in [
        ("0", "0", true),
        ("0x400000", "2", true),
        ("0", "2", false),
        ("0xefb3ffff", "0x201300327383", false),
    ] {
        let fixture = Fixture::new();
        let outcome = fixture.run(
            &[],
            &[("CI_FIXTURE_HWCAP", hwcap), ("CI_FIXTURE_HWCAP2", hwcap2)],
        );
        assert_eq!(outcome.code == 0, passed);
        assert_eq!(
            Path::new(&format!("{}.emulator-selected", fixture.log.display())).exists(),
            passed
        );
        assert_eq!(!tools(&outcome, "act").is_empty(), passed);
        if !passed {
            assert_eq!(summary(&outcome)["completeLocalGate"], false);
            let error =
                fs::read_to_string(format!("{}.guest-error", fixture.log.display())).unwrap();
            // The refusal names both words in lowercase hexadecimal with no
            // leading zeros, as the guest's values.
            let values = format!(
                "(HWCAP={:#x}, HWCAP2={:#x})",
                u64::from_str_radix(hwcap.trim_start_matches("0x"), 16).unwrap(),
                u64::from_str_radix(hwcap2.trim_start_matches("0x"), 16).unwrap()
            );
            for text in [
                "simplestchat-ci VM advertises SVE2 without SVE",
                values.as_str(),
                "arm64.nosve with rpm-ostree",
                "docs/testing.md#recover-inconsistent-arm-cpu-features",
            ] {
                assert!(error.contains(text), "{text}");
            }
            assert!(tools(&outcome, "podman").iter().all(|event| {
                !arguments(event)
                    .get(1)
                    .is_some_and(|action| ["set", "stop", "start"].contains(&action.as_str()))
            }));
        }
    }
}

#[test]
fn codeql_mounts_only_exact_runner_isa_authenticated_cache_outside_cargo_target() {
    let fixture = Fixture::new();
    let pins = json!({"bundles": {
        "linux-x86_64": {"sha256": "a".repeat(64), "bytes": 123},
        "linux-aarch64": {"sha256": "b".repeat(64), "bytes": 456},
    }});
    fixture.write("security/codeql-toolchain.json", &pins.to_string());
    let old = format!("target/codeql-tools/{}", "b".repeat(64));
    fixture.write(
        &format!("{old}/receipt.json"),
        &json!({
            "archiveSha256": "b".repeat(64), "archiveBytes": 456
        })
        .to_string(),
    );
    fixture.write(&format!("{old}/codeql/codeql"), "#!/bin/sh\nexit 0\n");
    fs::set_permissions(
        fixture.root.join(&old).join("codeql/codeql"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    for (platform, expected) in [("linux-x86_64", false), ("linux-aarch64", true)] {
        let pin = &pins["bundles"][platform];
        let directory = format!(".cache/codeql-tools/{}", pin["sha256"].as_str().unwrap());
        fixture.write(
            &format!("{directory}/receipt.json"),
            &json!({"archiveSha256": pin["sha256"],
            "archiveBytes": pin["bytes"]})
            .to_string(),
        );
        fixture.write(&format!("{directory}/codeql/codeql"), "#!/bin/sh\nexit 0\n");
        fs::set_permissions(
            fixture.root.join(&directory).join("codeql/codeql"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        let outcome = fixture.run(&[], &[]);
        assert_eq!(outcome.code, 0);
        let options = option(&tools(&outcome, "act")[0], "--container-options");
        assert_eq!(options.contains(".cache/codeql-tools"), expected);
        assert!(!options.contains("target/codeql-tools"));
        if expected {
            assert!(options.contains("readonly"));
        }
        assert!(
            !arguments(&tools(&outcome, "act")[0])
                .iter()
                .any(|value| value.starts_with("LOCAL_CODEQL_BINARY="))
        );
        fs::remove_dir_all(fixture.root.join(directory)).unwrap();
    }
}

#[test]
fn base_selection_checks_explicit_commits_and_falls_back_to_parent_of_current_main() {
    for (args, overrides, expected) in [
        (
            vec!["web", "--base", "candidate-parent"],
            vec![],
            "candidate-parent^{commit}".to_owned(),
        ),
        (
            vec!["web"],
            vec![("CI_FIXTURE_DEFAULT_HEAD_BASE", "1")],
            "HEAD^^{commit}".to_owned(),
        ),
    ] {
        let fixture = Fixture::new();
        let outcome = fixture.run(&args, &overrides);
        assert_eq!(outcome.code, 0);
        let git = tools(&outcome, "git");
        assert!(
            git.iter()
                .any(|event| arguments(event) == ["rev-parse", "--verify", &expected])
        );
        assert!(
            git.iter()
                .any(|event| arguments(event) == ["merge-base", "--is-ancestor", BASE, REVISION])
        );
    }
}

#[test]
fn failed_vm_start_is_not_owned_and_failed_owned_stop_cannot_claim_success() {
    for (phase, expected, stopped) in [("podman-start", 43, false), ("podman-stop", 1, true)] {
        let fixture = Fixture::new();
        let outcome = fixture.run(
            &[],
            &[("CI_FIXTURE_STOPPED", "1"), ("CI_FIXTURE_FAIL", phase)],
        );
        assert_eq!(outcome.code, expected);
        assert_eq!(summary(&outcome)["completeLocalGate"], false);
        assert_eq!(
            tools(&outcome, "podman")
                .iter()
                .any(|event| arguments(event)
                    .get(1)
                    .is_some_and(|action| action == "stop")),
            stopped
        );
        assert!(!fixture.root.join("target/act/run.lock").exists());
    }
}

#[test]
fn unresponsive_owned_engine_refuses_before_any_workflow_or_guest_emulator() {
    let fixture = Fixture::new();
    let outcome = fixture.run(&[], &[("CI_FIXTURE_ENGINE_DOWN", "1")]);
    assert_eq!(outcome.code, 2);
    assert!(tools(&outcome, "act").is_empty());
    assert!(tools(&outcome, "podman").iter().all(|event| {
        !arguments(event)
            .get(1)
            .is_some_and(|action| action == "ssh")
    }));
    assert_eq!(summary(&outcome)["completeLocalGate"], false);
}

#[test]
fn changing_head_or_worktree_during_workflow_invalidates_success() {
    for flag in ["CI_FIXTURE_CHANGED", "CI_FIXTURE_DIRTY_AFTER"] {
        let fixture = Fixture::new();
        let outcome = fixture.run(&[], &[(flag, "1")]);
        assert_eq!(outcome.code, 2);
        assert_eq!(summary(&outcome)["completeLocalGate"], false);
    }
}

#[test]
fn aggregate_and_each_matrix_receipt_are_bound_to_this_run_and_workflow_bytes() {
    for flag in [
        "CI_FIXTURE_NO_RECEIPT",
        "CI_FIXTURE_STALE_RECEIPT",
        "CI_FIXTURE_BAD_HASH",
        "CI_FIXTURE_MISSING_MATRIX",
        "CI_FIXTURE_EXTRA_RECEIPT_KEY",
        "CI_FIXTURE_BAD_ID",
        "CI_FIXTURE_DUPLICATE",
    ] {
        let fixture = Fixture::new();
        let outcome = fixture.run(&[], &[(flag, "1")]);
        assert_eq!(outcome.code, 1, "{flag}");
        assert_eq!(summary(&outcome)["completeLocalGate"], false);
    }
}

#[test]
fn existing_output_and_dangling_links_are_never_adopted_or_overwritten() {
    for kind in ["directory", "file", "link"] {
        let fixture = Fixture::new();
        let path = fixture.root.join("existing");
        match kind {
            "directory" => {
                fs::create_dir(&path).unwrap();
                fs::write(path.join("summary.json"), "keep this evidence").unwrap();
            }
            "file" => fs::write(&path, "keep this evidence").unwrap(),
            _ => symlink(fixture.root.join("absent"), &path).unwrap(),
        }
        let outcome = fixture.run(&["all", "--output", "existing"], &[]);
        assert_eq!(outcome.code, 2);
        assert!(tools(&outcome, "podman").is_empty());
        assert!(!fixture.root.join("target/act/run.lock").exists());
        if kind == "directory" {
            assert_eq!(
                fs::read_to_string(path.join("summary.json")).unwrap(),
                "keep this evidence"
            );
        }
        if kind == "file" {
            assert_eq!(fs::read_to_string(&path).unwrap(), "keep this evidence");
        }
        if kind == "link" {
            assert!(fs::symlink_metadata(path).unwrap().is_symlink());
        }
    }
}

#[test]
fn existing_lock_directory_or_link_prevents_invocation_and_is_not_removed() {
    for link in [false, true] {
        let fixture = Fixture::new();
        fixture.write("target/act/marker", "untouched");
        let lock = fixture.root.join("target/act/run.lock");
        if link {
            symlink(fixture.root.join("absent"), &lock).unwrap();
        } else {
            fs::create_dir(&lock).unwrap();
            fs::write(lock.join("owner"), "untouched").unwrap();
        }
        let outcome = fixture.run(&[], &[]);
        assert_eq!(outcome.code, 2);
        assert!(tools(&outcome, "go").is_empty());
        assert!(fs::symlink_metadata(&lock).is_ok());
        assert_eq!(
            fs::read_to_string(fixture.root.join("target/act/marker")).unwrap(),
            "untouched"
        );
    }
}

#[test]
fn evidence_and_log_are_private_and_stdout_stderr_are_both_retained() {
    let fixture = Fixture::new();
    let outcome = fixture.run(&["all", "--output", "evidence with spaces"], &[]);
    assert_eq!(outcome.code, 0);
    let output = outcome.output.unwrap();
    assert_eq!(
        output.metadata().unwrap().permissions().mode() & 0o777,
        0o700
    );
    for name in [
        "event.json",
        "summary.json",
        "act.log",
        "engine-before.json",
        "engine-after.json",
    ] {
        assert_eq!(
            output.join(name).metadata().unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    let log = fs::read_to_string(output.join("act.log")).unwrap();
    assert!(log.contains("fixture workflow stdout"));
    assert!(log.contains("fixture workflow stderr"));
}

#[test]
fn leftover_containers_fail_without_any_removal_or_pruning() {
    let fixture = Fixture::new();
    let outcome = fixture.run(&[], &[("CI_FIXTURE_BUSY_AFTER", "1")]);
    assert_eq!(outcome.code, 1);
    assert_eq!(summary(&outcome)["completeLocalGate"], false);
    assert!(outcome.events.iter().all(|event| {
        !arguments(event)
            .iter()
            .any(|value| ["rm", "prune", "delete"].contains(&value.as_str()))
    }));
}

#[test]
fn cleanup_cannot_remove_a_replacement_lock_or_write_through_replaced_output() {
    let fixture = Fixture::new();
    let outcome = fixture.run(&[], &[("CI_FIXTURE_REPLACE_LOCK", "1")]);
    assert_eq!(outcome.code, 1);
    assert_eq!(
        fs::read_to_string(fixture.root.join("target/act/run.lock/other-owner")).unwrap(),
        "untouched"
    );
    let fixture = Fixture::new();
    let outcome = fixture.run(
        &["--output", "replace-me"],
        &[("CI_FIXTURE_REPLACE_OUTPUT", "1")],
    );
    assert_eq!(outcome.code, 1);
    assert_eq!(
        fs::read_to_string(fixture.root.join("replace-me.other/marker")).unwrap(),
        "untouched"
    );
    assert!(!fixture.root.join("replace-me.other/summary.json").exists());
}

#[test]
fn cleanup_never_overwrites_an_existing_summary() {
    let fixture = Fixture::new();
    let outcome = fixture.run(&[], &[("CI_FIXTURE_EXISTING_SUMMARY", "1")]);
    assert_eq!(outcome.code, 1);
    assert_eq!(
        fs::read_to_string(outcome.output.unwrap().join("summary.json")).unwrap(),
        "keep this evidence"
    );
}

#[test]
fn invalid_engine_json_and_nonprivate_or_ambiguous_gateways_fail_closed() {
    let fixture = Fixture::new();
    let outcome = fixture.run(&[], &[("CI_FIXTURE_CONTAINERS", "{}")]);
    assert_eq!(outcome.code, 1);
    assert!(tools(&outcome, "act").is_empty());
    for addresses in [
        vec!["8.8.8.8"],
        vec!["100.64.0.1"],
        vec!["10.0.0.1", "192.168.0.1"],
        vec!["not-an-ip"],
    ] {
        let networks = json!([{"Name":"bridge", "IPAM":{"Config":addresses.into_iter().map(|address|
            json!({"Gateway":address})).collect::<Vec<_>>()}}]);
        let fixture = Fixture::new();
        let outcome = fixture.run(&[], &[("CI_FIXTURE_NETWORKS", &networks.to_string())]);
        assert_eq!(outcome.code, 1);
        assert!(tools(&outcome, "act").is_empty());
    }
}

#[test]
fn gateway_preserves_python_non_global_ipv4_policy_not_only_rfc1918() {
    for (address, allowed) in [
        ("10.88.0.1", true),
        ("172.17.0.1", true),
        ("192.168.1.1", true),
        ("127.0.0.1", true),
        ("169.254.1.1", true),
        ("192.0.0.9", false),
        ("192.0.0.10", false),
        ("192.0.2.1", true),
        ("100.64.0.1", false),
        ("8.8.8.8", false),
    ] {
        let networks = json!([{"Name":"bridge", "IPAM":{"Config":[{"Gateway":address}]}}]);
        assert_eq!(engine::gateway(&networks).is_ok(), allowed, "{address}");
    }
}

#[test]
fn command_failures_preserve_status_and_release_owned_lock() {
    for phase in ["go", "podman-ssh", "curl-networks", "act"] {
        let fixture = Fixture::new();
        let outcome = fixture.run(
            &[],
            &[("CI_FIXTURE_FAIL", phase), ("CI_FIXTURE_FAIL_CODE", "43")],
        );
        assert_eq!(outcome.code, 43, "{phase}");
        assert!(!fixture.root.join("target/act/run.lock").exists());
        assert_eq!(summary(&outcome)["completeLocalGate"], false);
    }
}

#[test]
fn interrupt_and_term_cancel_only_owned_children_stop_started_vm_and_keep_failed_receipt() {
    for code in [130, 143] {
        let fixture = Fixture::new();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let outcome = runtime.block_on(async {
            let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
            let ready = PathBuf::from(format!("{}.child-ready", fixture.log.display()));
            let signal = tokio::spawn(async move {
                for _ in 0..1000 {
                    if ready.exists() {
                        sender.send(code).unwrap();
                        return;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                panic!("fake owned act did not become ready");
            });
            let result = tokio::time::timeout(
                Duration::from_secs(20),
                fixture.run_async(
                    &[],
                    &[("CI_FIXTURE_STOPPED", "1"), ("CI_FIXTURE_ACT_WAIT", "1")],
                    Signals::injected(receiver),
                ),
            )
            .await
            .expect("owned cancellation completes");
            signal.await.unwrap();
            result
        });
        assert_eq!(outcome.code, code);
        assert_eq!(summary(&outcome)["exitCode"], code);
        assert_eq!(summary(&outcome)["completeLocalGate"], false);
        assert!(!fixture.root.join("target/act/run.lock").exists());
        assert!(tools(&outcome, "podman").iter().any(|event| {
            arguments(event)
                .get(1)
                .is_some_and(|action| action == "stop")
        }));
        assert!(Path::new(&format!("{}.child-stopped", fixture.log.display())).exists());
    }
}

#[test]
fn queued_signal_after_child_completion_is_drained_before_final_receipt() {
    let fixture = Fixture::new();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut runner = Runner::new(
            &fixture.root,
            fixture.environment.clone(),
            Signals::injected(receiver),
        );
        sender.send(143).unwrap();
        assert_eq!(runner.pending_signal().await, Some(143));
        drop(sender);
        assert_eq!(runner.pending_signal().await, None);
    });
}
