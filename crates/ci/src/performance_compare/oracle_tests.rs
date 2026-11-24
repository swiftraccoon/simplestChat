//! Gitless runtime oracles retain the three decoded `run: |` scalar bodies
//! from `.github/workflows/performance.yml` at the exact source commit below.
//! Their original bytes are checked before Bash runs only owned inert recorders;
//! no Git, compiler, candidate, server or workload is executed by these tests.

use super::plan::Plan;
use sha2::{Digest, Sha256};
use simplestchat_toolkit::tooling::Environment;
use std::{
    fs,
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

const SOURCE_COMMIT: &str = "166af83deaa7c073e134e19fc30ae365ce0c4af3";
const SOURCE_PATH: &str = ".github/workflows/performance.yml";
const WORKFLOW_SHA256: &str = "9889c58a9748d5ed29405110bd6a71725ae29e3a7dbadf21dd3835b026c037d4";
const BASELINE: &[u8] = include_bytes!("reference-baseline.sh");
const BUILD: &[u8] = include_bytes!("reference-build.sh");
const COMPARISON: &[u8] = include_bytes!("reference-comparison.sh");
const SELECTED_REVISION: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn assert_original_bytes() {
    assert_eq!(SOURCE_COMMIT.len(), 40);
    assert_eq!(SOURCE_PATH, ".github/workflows/performance.yml");
    assert_eq!(WORKFLOW_SHA256.len(), 64);
    for (bytes, size, hash) in [
        (
            BASELINE,
            571,
            "e1eb6f4000e328e1528a759c8ee3bb500184d6026c5395f97cff324257d97f7a",
        ),
        (
            BUILD,
            123,
            "c76eae20bde3e84414610b205c50cfe3de4218b19e70482547441c358b0f7e93",
        ),
        (
            COMPARISON,
            918,
            "0ec14a7dea947c2bf8f8fc02db3080be99a9e2586373f6ea175d48190498cc80",
        ),
    ] {
        assert_eq!(bytes.len(), size);
        let digest: String = Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        assert_eq!(digest, hash);
    }
}

/// Every executable is this same inert recorder under a literal role name.
/// The build recorder writes distinguishable fixture bytes but executes none.
const RECORDER: &str = r#"
role=${0##*/}
workspace=${GITHUB_WORKSPACE?}
temporary=${RUNNER_TEMP?}
cwd=$(pwd -P)
event="{\"program\": $(json_string "$role"), \"args\": $(json_strings "$@")"
event="$event, \"cwd\": $(json_string "$cwd"), \"environment\": {"
separator=
for key in CARGO_TARGET_DIR OPENSSL_DIR OPENSSL_STATIC AWS_LC_SYS_SYSTEM_DIR AWS_LC_SYS_STATIC \
  AWS_LC_SYS_SYSTEM_SKIP_VERSION_CHECK PKG_CONFIG_PATH PIP_CONSTRAINT CAPTURED_MARKER; do
  event="$event$separator\"$key\": $(json_value "$key")"
  separator=', '
done
printf '%s}}\n' "$event" >>"${ORACLE_RECORD?}"
fresh_directory() {
  if [ -e "$1" ] || [ -L "$1" ]; then return 1; fi
  /bin/mkdir -p -- "$1"
}
selected=$(repeat b 40)
case $role in
  git)
    [ "$#" -ge 1 ] || exit 1
    if [ "$1" = rev-parse ]; then
      [ "$#" -eq 3 ] && [ "$2" = --verify ] && [ "$3" = "${BASELINE_REF?}^{commit}" ] || exit 1
      printf '%s\n' "$selected"
    else
      [ "$#" -eq 5 ] && [ "$1" = worktree ] && [ "$2" = add ] && [ "$3" = --detach ] &&
        [ "$4" = "$temporary/baseline" ] && [ "$5" = "$selected" ] || exit 1
      fresh_directory "$temporary/baseline/vendor" || exit 1
    fi
    ;;
  find)
    [ "$#" -eq 8 ] && [ "$1" = "$temporary/baseline/vendor" ] && [ "$2" = -exec ] &&
      [ "$3" = touch ] && [ "$4" = -h ] && [ "$5" = -t ] && [ "$6" = 200001010000 ] &&
      [ "$7" = '{}' ] && [ "$8" = + ] || exit 1
    ;;
  cargo)
    {
      [ "$#" -eq 5 ] && [ "$1" = build ] && [ "$2" = --locked ] && [ "$3" = --release ] &&
        [ "$4" = --bin ] && [ "$5" = simplestChat ]
    } || {
      [ "$#" -eq 7 ] && [ "$1" = build ] && [ "$2" = --locked ] && [ "$3" = --release ] &&
        [ "$4" = -p ] && [ "$5" = simplestchat-load-test ] && [ "$6" = --bin ] &&
        [ "$7" = load_test ]
    } || exit 1
    target=${CARGO_TARGET_DIR-$cwd/target}/release
    /bin/mkdir -p -- "$target" || exit 1
    if [ "$cwd" = "$temporary/baseline" ]; then variant=baseline; else variant=candidate; fi
    eval "binary=\${$#}"
    printf 'inert %s %s' "$variant" "$binary" >"$target/$binary"
    ;;
  install)
    [ "$#" -eq 3 ] && [ "$1" = -D ] && [ "$2" = "$workspace/target/release/simplestChat" ] &&
      [ "$3" = "$temporary/baseline/target/release/simplestChat" ] || exit 1
    fresh_directory "${3%/*}" || exit 1
    /bin/cat -- "$2" >"$3" && /bin/chmod 755 "$3" || exit 1
    ;;
  rm)
    [ "$#" -eq 2 ] && [ "$1" = -f ] && [ "$2" = "$workspace/target/release/simplestChat" ] ||
      exit 1
    /bin/rm -- "$2" || exit 1
    ;;
  *)
    [ "$role" = load_test ] || exit 1
    ;;
esac
"#;

struct Fixture {
    directory: PathBuf,
    workspace: PathBuf,
    temporary: PathBuf,
    bash: PathBuf,
    environment: Environment,
}
impl Fixture {
    fn new(clients: &str, workers: &str) -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let directory = std::env::temp_dir().join(format!(
            "comparison Bash λ {} {}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .unwrap();
        let directory = directory.canonicalize().unwrap();
        let workspace = directory.join("candidate work λ");
        let temporary = directory.join("runner temp λ");
        let tools = directory.join("inert tools λ");
        for path in [&workspace, &temporary, &tools] {
            fs::create_dir(path).unwrap();
        }
        fs::create_dir_all(workspace.join("target/release")).unwrap();
        for name in ["git", "cargo", "find", "install", "rm"] {
            executable(&tools.join(name));
        }
        executable(&workspace.join("target/release/load_test"));
        let environment = [
            ("GITHUB_ACTIONS", "true".into()),
            ("RUNNER_OS", "Linux".into()),
            ("GITHUB_WORKSPACE", workspace.clone().into_os_string()),
            ("RUNNER_TEMP", temporary.clone().into_os_string()),
            ("PATH", tools.into_os_string()),
            ("BASELINE_REF", "release with spaces".into()),
            ("CLIENTS", clients.into()),
            ("WORKERS", workers.into()),
            (
                "ORACLE_RECORD",
                directory.join("record.ndjson").into_os_string(),
            ),
            ("OPENSSL_DIR", "/literal selected OpenSSL".into()),
            ("OPENSSL_STATIC", "1".into()),
            ("AWS_LC_SYS_SYSTEM_DIR", "/literal selected AWS-LC".into()),
            ("AWS_LC_SYS_STATIC", "1".into()),
            ("AWS_LC_SYS_SYSTEM_SKIP_VERSION_CHECK", "0".into()),
            ("PKG_CONFIG_PATH", "/literal selected pkgconfig".into()),
            ("PIP_CONSTRAINT", "/literal selected constraints".into()),
            ("CAPTURED_MARKER", "literal marker with spaces λ".into()),
        ]
        .into_iter()
        .map(|(name, value)| (name.into(), value))
        .collect();
        Self {
            directory,
            workspace,
            temporary,
            bash: reference_bash(),
            environment,
        }
    }
    async fn bash(&self, name: &str, original: &[u8]) {
        let script = self.directory.join(name);
        fs::write(&script, original).unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o600)).unwrap();
        let mut version = Command::new(&self.bash);
        version.arg("--version").env_clear().stderr(Stdio::null());
        let (status, bytes) = simplestchat_toolkit::test_env::helper_with_signals(
            version,
            Duration::from_secs(5),
            Some(4096),
            None,
        )
        .await
        .unwrap();
        assert_eq!(status, 0);
        let identity = [self.bash.as_os_str().as_encoded_bytes(), b"\n", &bytes].concat();
        fs::write(self.directory.join("bash-runtime.txt"), identity).unwrap();
        let mut command = Command::new(&self.bash);
        command
            .args(["--noprofile", "--norc"])
            .arg(script)
            .current_dir(&self.workspace)
            .env_clear()
            .envs(&self.environment)
            .stderr(Stdio::null());
        let (status, _) = simplestchat_toolkit::test_env::helper_with_signals(
            command,
            Duration::from_secs(5),
            Some(64 * 1024),
            None,
        )
        .await
        .unwrap();
        assert_eq!(status, 0);
    }
    fn events(&self) -> Vec<serde_json::Value> {
        let bytes = fs::read(self.directory.join("record.ndjson")).unwrap();
        assert!(bytes.len() < 64 * 1024);
        std::str::from_utf8(&bytes)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}
fn executable(path: &Path) {
    fs::write(
        path,
        format!("#!/bin/sh\n{}{RECORDER}", crate::fake_shell::PRELUDE),
    )
    .unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

fn reference_bash() -> PathBuf {
    // Resolve the caller-selected Bash before replacing PATH with inert tools.
    // Darwin's fixed /bin/bash3.2 is not the hosted Linux array/nounset runtime.
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .map(|path| path.join("bash"))
        .find(|path| {
            path.metadata().is_ok_and(|metadata| {
                metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
            })
        })
        .expect("the reference shell oracle requires an installed Bash")
        .canonicalize()
        .unwrap()
}

#[tokio::test]
async fn original_bash_all_six_profiles_equal_the_closed_rust_generator_arguments() {
    assert_original_bytes();
    for clients in ["10", "30", "100"] {
        for workers in ["1", "2"] {
            let fixture = Fixture::new(clients, workers);
            let plan =
                Plan::parse(&fixture.workspace, fixture.environment.clone(), "linux").unwrap();
            fixture.bash("comparison.sh", COMPARISON).await;
            let events = fixture.events();
            assert_eq!(events.len(), 1);
            assert_eq!(events[0]["program"], "load_test");
            assert_eq!(
                events[0]["args"],
                serde_json::json!(plan.comparison_arguments())
            );
            assert_eq!(events[0]["cwd"], fixture.workspace.to_str().unwrap());
            assert_eq!(
                events[0]["environment"]["CAPTURED_MARKER"],
                "literal marker with spaces λ"
            );
        }
    }
}

#[tokio::test]
async fn original_build_phases_keep_baseline_cwd_shared_target_and_copy_before_unlink() {
    assert_original_bytes();
    let fixture = Fixture::new("30", "2");
    fixture.bash("baseline.sh", BASELINE).await;
    let baseline = fixture
        .temporary
        .join("baseline/target/release/simplestChat");
    assert_eq!(fs::read(&baseline).unwrap(), b"inert baseline simplestChat");
    assert!(
        !fixture
            .workspace
            .join("target/release/simplestChat")
            .exists()
    );
    fixture.bash("build.sh", BUILD).await;
    assert_eq!(fs::read(&baseline).unwrap(), b"inert baseline simplestChat");
    assert_eq!(
        fs::read(fixture.workspace.join("target/release/simplestChat")).unwrap(),
        b"inert candidate simplestChat"
    );
    let events = fixture.events();
    let roles: Vec<_> = events
        .iter()
        .map(|v| v["program"].as_str().unwrap())
        .collect();
    assert_eq!(
        roles,
        [
            "git", "git", "find", "cargo", "install", "rm", "cargo", "cargo"
        ]
    );
    assert_eq!(events[0]["args"][2], "release with spaces^{commit}");
    assert_eq!(events[1]["args"][4], SELECTED_REVISION);
    assert_eq!(
        events[3]["cwd"],
        fixture.temporary.join("baseline").to_str().unwrap()
    );
    assert_eq!(
        events[3]["environment"]["CARGO_TARGET_DIR"],
        fixture.workspace.join("target").to_str().unwrap()
    );
    for index in [3, 6, 7] {
        for key in [
            "OPENSSL_DIR",
            "OPENSSL_STATIC",
            "AWS_LC_SYS_SYSTEM_DIR",
            "AWS_LC_SYS_STATIC",
            "AWS_LC_SYS_SYSTEM_SKIP_VERSION_CHECK",
            "PKG_CONFIG_PATH",
            "PIP_CONSTRAINT",
            "CAPTURED_MARKER",
        ] {
            assert_eq!(
                events[index]["environment"][key],
                fixture.environment[std::ffi::OsStr::new(key)]
                    .to_str()
                    .unwrap()
            );
        }
    }
    for index in [6, 7] {
        assert_eq!(events[index]["cwd"], fixture.workspace.to_str().unwrap());
        assert!(events[index]["environment"]["CARGO_TARGET_DIR"].is_null());
    }
}
