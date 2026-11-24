//! Exact decoded source73 Bash scalar with owned fake tools; no Git runtime.
use super::{
    super::plan,
    fixture::{Fixture, REV},
};
use sha2::{Digest, Sha256};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{Command, Stdio},
    time::Duration,
};
const ORIGINAL: &[u8] = include_bytes!("reference-build.sh");
const SOURCE: &str = "d835ec261c15d926a32328bd7d7fcb0ec49257e0";
const WORKFLOW: &str = "0ffc53e450724e8e3d3ea7240530a9bcf32dc5d5a9a4624d159dc2ece2b7b921";
/// Every role's recorder: one event line per call, written with one append
/// so that the two ends of the `docker save | zstd` pipeline cannot
/// interleave their lines.
const RECORDER: &str = r#"
role=${0##*/}
temp=${RUNNER_TEMP?}
phase=$role
if [ "$role" = docker ]; then
  if [ "$#" -ge 3 ] && [ "$1" = buildx ] && [ "$2" = rm ] && [ "$3" = --force ]; then
    if [ -e "$temp/builder-created" ]; then phase=final-rm; else phase=initial-rm; fi
  elif [ "$#" -ge 2 ] && [ "$1" = buildx ] && [ "$2" = create ]; then
    phase=create
  elif [ "$#" -ge 2 ] && [ "$1" = buildx ] && [ "$2" = build ]; then
    phase=build
  elif [ "$#" -ge 1 ] && [ "$1" = save ]; then
    phase=save
  elif [ "$#" -ge 2 ] && [ "$1" = image ] && [ "$2" = inspect ]; then
    phase=inspect
  else
    exit 1
  fi
fi
cwd=$(pwd -P)
event="{\"role\": $(json_string "$role"), \"phase\": $(json_string "$phase")"
event="$event, \"args\": $(json_strings "$@"), \"cwd\": $(json_string "$cwd")}"
(umask 077 && printf '%s\n' "$event" >>"${ORACLE_RECORD?}") || exit 1
if [ "$phase" = initial-rm ]; then exit 9; fi
if [ "${ORACLE_FAILURE?}" = "$phase" ]; then exit "${ORACLE_STATUS?}"; fi
if [ "$role" = git ]; then
  if [ "$ORACLE_FAILURE" != head ]; then
    printf '%s\n' "${GITHUB_SHA?}"
  else
    printf '%s\n' "$(repeat b 40)"
  fi
elif [ "$phase" = create ]; then
  printf inert >"$temp/builder-created"
elif [ "$role" = timeout ]; then
  [ "$#" -ge 4 ] && [ "$1" = --signal=TERM ] && [ "$2" = --kill-after=2m ] && [ "$3" = 40m ] ||
    exit 1
  shift 4
  exec "$PATH/docker" "$@"
elif [ "$role" = mkdir ]; then
  [ "$#" -eq 2 ] && [ "$1" = -p ] && [ "$2" = "$temp/loadtest-image" ] || exit 1
  /bin/mkdir -- "$temp/loadtest-image" || exit 1
elif [ "$phase" = save ]; then
  printf 'inert saved tar bytes'
elif [ "$role" = zstd ]; then
  [ "$#" -ge 4 ] && [ "$1" = -T0 ] && [ "$2" = -3 ] && [ "$3" = -o ] || exit 1
  { printf 'inert compressed '; /bin/cat; } >"$4" || exit 1
elif [ "$phase" = inspect ]; then
  printf 'sha256:%s\n' "$(repeat c 64)"
elif [ "$role" = ls ]; then
  [ "$#" -eq 2 ] && [ "$1" = -la ] && [ "$2" = "$temp/loadtest-image" ] || exit 1
fi
"#;
fn bash() -> PathBuf {
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .map(|path| path.join("bash"))
        .find(|path| {
            path.metadata()
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
        .expect("installed Bash required by retained reference oracle")
        .canonicalize()
        .unwrap()
}
async fn reference(
    cache: bool,
    failure: &str,
    status: u8,
) -> (Fixture, Vec<serde_json::Value>, u8) {
    assert_eq!(SOURCE.len(), 40);
    assert_eq!(WORKFLOW.len(), 64);
    assert_eq!(ORIGINAL.len(), 1087);
    let sha: String = Sha256::digest(ORIGINAL)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(
        sha,
        "0e156c8cc97e2368f64c4c37da67e0f1b8a67382a19193d92db1a3b02889a48b"
    );
    let selected_bash = bash();
    let fixture = Fixture::new();
    let recorder = format!("#!/bin/sh\n{}{RECORDER}", crate::fake_shell::PRELUDE);
    for role in ["git", "docker", "timeout", "mkdir", "zstd", "ls"] {
        let path = fixture.root.join("bin").join(role);
        fs::write(&path, &recorder).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    if cache {
        let path = fixture.context.temporary.join("buildx-cache");
        fs::create_dir(&path).unwrap();
        fs::write(path.join("index.json"), b"{}").unwrap();
    }
    let script = fixture.root.join("reference.sh");
    fs::write(&script, ORIGINAL).unwrap();
    let record = fixture.root.join("oracle.ndjson");
    let mut command = Command::new(selected_bash);
    command
        .args(["--noprofile", "--norc"])
        .arg(&script)
        .current_dir(&fixture.root)
        .env_clear()
        .envs(&fixture.context.environment)
        .env("ORACLE_RECORD", &record)
        .env("ORACLE_FAILURE", failure)
        .env("ORACLE_STATUS", status.to_string())
        .stderr(Stdio::null());
    let (status, _) = simplestchat_toolkit::test_env::helper_with_signals(
        command,
        Duration::from_secs(5),
        Some(64 * 1024),
        None,
    )
    .await
    .unwrap();
    let bytes = fs::read(record).unwrap();
    assert!(bytes.len() <= 64 * 1024);
    let events = std::str::from_utf8(&bytes)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    (fixture, events, u8::try_from(status).unwrap())
}
#[tokio::test]
async fn actual_retained_shell_fixed_recipe_cache_and_exact_artifacts_match_owner() {
    for cache in [false, true] {
        let (fixture, events, status) = reference(cache, "", 0).await;
        assert_eq!(status, 0);
        let build = events
            .iter()
            .find(|event| event["phase"] == "build")
            .unwrap();
        let context = &fixture.context;
        let command = context.build("unix:///owned/docker.sock", "nonce-owned", cache);
        let mut args = command
            .arguments
            .iter()
            .map(|v| v.to_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(&args[..2], ["--host", "unix:///owned/docker.sock"]);
        args.drain(..2);
        let builder = args.iter().position(|v| v == "--builder").unwrap();
        args.drain(builder..builder + 2);
        assert_eq!(build["args"], serde_json::json!(args));
        assert_eq!(build["cwd"], fixture.root.to_str().unwrap());
        let directory = context.temporary.join("loadtest-image");
        assert_eq!(
            fs::read(directory.join("image-id.txt")).unwrap(),
            format!("sha256:{}\n", "c".repeat(64)).as_bytes()
        );
        assert_eq!(
            fs::read(directory.join(context.archive_name())).unwrap(),
            b"inert compressed inert saved tar bytes"
        );
        assert_eq!(fs::read_dir(directory).unwrap().count(), 2);
        assert!(events.iter().any(|event| event["phase"] == "timeout"));
        assert_eq!(context.revision, REV);
        assert_eq!(plan::TAG, "simplestchat-loadtest:ci");
    }
}
#[tokio::test]
async fn actual_retained_shell_keeps_known_failures_and_never_reaches_later_inspection() {
    for (phase, status) in [
        ("head", 1),
        ("create", 41),
        ("build", 42),
        ("timeout", 124),
        ("final-rm", 43),
        ("save", 44),
        ("zstd", 45),
        ("inspect", 46),
    ] {
        let (_fixture, events, observed) = reference(false, phase, status).await;
        assert_eq!(observed, status, "{phase}");
        assert!(!events.iter().any(|event| event["phase"] == "ls"));
        if !matches!(phase, "inspect") {
            assert!(!events.iter().any(|event| event["phase"] == "inspect"));
        }
    }
}
