//! Execute the unchanged shell against only allowlisted inert command stand-ins.

use super::*;
use std::{
    os::unix::fs::PermissionsExt,
    process::{Command as Process, Stdio},
};

fn reference_bash() -> PathBuf {
    // Resolve the unchanged env-shebang before installing the fake command PATH.
    // Record the exact choice/version in the owned test directory, never rely
    // on Darwin's hard-coded Bash3.2 as a substitute for the selected runtime.
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .map(|path| path.join("bash"))
        .find(|path| {
            path.metadata()
                .is_ok_and(|value| value.is_file() && value.permissions().mode() & 0o111 != 0)
        })
        .unwrap()
        .canonicalize()
        .unwrap()
}

/// The stand-in for `docker`, `curl` and `sleep`. Its state lives in the
/// fixture's `oracle-state` directory: `bad` names the fault to answer,
/// `database`, `app`, `restarted` and `registeredRequest` exist once that
/// happened, and `removed` holds one removed container per line.
const STAND_IN: &str = r#"#!/bin/sh
PATH=/usr/bin:/bin
root=${0%/*}
root=${root%/*}
state=$root/oracle-state
bad=$(cat "$state/bad") || exit 1
program=${0##*/}
last=
if [ "$#" -gt 0 ]; then eval "last=\${$#}"; fi
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
if [ "$program" = sleep ]; then exit 0; fi
if [ "$program" = docker ]; then
  [ "$#" -ge 1 ] || exit 1
  case $1 in
    info | image) exit 0 ;;
    run)
      if member --publish "$@"; then
        : >"$state/database"
        printf 'dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd\n'
      else
        : >"$state/app"
        printf 'eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee\n'
      fi
      exit 0
      ;;
    inspect)
      if [ "$last" = eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee ] &&
        [ -e "$state/restarted" ] && [ "$bad" = restart ]; then
        printf 'false\n'
      else
        printf 'true\n'
      fi
      exit 0
      ;;
    exec)
      [ "$#" -ge 3 ] || exit 1
      if [ "$3" = pg_isready ]; then exit 0; fi
      case $last in
        *_sqlx_migrations*)
          case $bad in
            migration-zero) printf '0\n' ;;
            migration-count) printf '1\n' ;;
            *) printf '2\n' ;;
          esac
          ;;
        *) if [ "$bad" = registered ]; then printf '0\n'; else printf '1\n'; fi ;;
      esac
      exit 0
      ;;
    port)
      if [ "$bad" = port ]; then printf '0.0.0.0:38001\n'; else printf '127.0.0.1:38001\n'; fi
      exit 0
      ;;
    restart)
      : >"$state/restarted"
      printf '%s\n' "$last"
      exit 0
      ;;
    logs)
      printf 'PRIVATE inert guest logs\n'
      exit 0
      ;;
    rm)
      printf '%s\n' "$last" >>"$state/removed"
      exit 0
      ;;
  esac
  exit 99
fi
if [ "$program" = curl ]; then
  route=${last##*:38001}
  case $route in
    /api/auth/register)
      output=$(value --output "$@") && [ "$output" = /dev/null ] || exit 1
      data=$(value --data "$@") || exit 1
      case $data in
        *'"email":"container-smoke@example.test"'*) ;;
        *) exit 1 ;;
      esac
      : >"$state/registeredRequest"
      exit 0
      ;;
    /api/rooms)
      if [ "$bad" = rooms ]; then printf '[{}]'; else printf '[]\n\n'; fi
      ;;
    /health)
      if [ "$bad" = health-command ]; then exit 22; fi
      if [ "$bad" = health ]; then printf wrong; else printf '{"status":"ok"}\n\n'; fi
      ;;
    /)
      if [ "$bad" = html-command ]; then exit 22; fi
      if [ "$bad" = html ]; then
        printf 'not html'
      else
        printf '<!DOCTYPE HTML><title>owned</title>'
      fi
      ;;
    *)
      asset=${route#?}
      case $bad:$asset in
        mime:*) mime=text/plain ;;
        *:manifest.webmanifest) mime=application/manifest+json ;;
        *:sw.js) mime=application/javascript ;;
        *) mime=image/png ;;
      esac
      output=$(value --output "$@") || exit 1
      if [ "$bad" = asset-bytes ]; then
        printf wrong >"$output"
      else
        cat -- "$root/web/public/$asset" >"$output" || exit 1
      fi
      printf '%s; charset=utf-8\n' "$mime"
      ;;
  esac
  exit 0
fi
exit 98
"#;

#[tokio::test]
async fn all_original_assertion_outcomes_match_the_inert_legacy_script() {
    let bash = reference_bash();
    let version = Process::new(&bash).arg("--version").output().unwrap();
    assert!(version.status.success() && version.stdout.len() < 4096);
    for bad in [
        "",
        "health",
        "health-command",
        "html-command",
        "html",
        "mime",
        "asset-bytes",
        "rooms",
        "migration-zero",
        "migration-count",
        "registered",
        "restart",
        "port",
    ] {
        let legacy = Fixture::new();
        fs::create_dir(legacy.root.join("build")).unwrap();
        fs::create_dir(legacy.root.join("fake-bin")).unwrap();
        let reference = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(std::path::Path::parent)
            .unwrap()
            .join("build/test-container.sh");
        let script = legacy.root.join("build/test-container.sh");
        fs::copy(reference, &script).unwrap();
        for name in ["docker", "curl", "sleep"] {
            let path = legacy.root.join("fake-bin").join(name);
            fs::write(&path, STAND_IN).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let state = legacy.root.join("oracle-state");
        fs::create_dir(&state).unwrap();
        fs::write(state.join("bad"), bad).unwrap();
        fs::write(
            legacy.root.join("reference-bash.txt"),
            [bash.as_os_str().as_encoded_bytes(), b"\n", &version.stdout].concat(),
        )
        .unwrap();
        let output = Process::new(&bash)
            .arg(&script)
            .env_clear()
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", legacy.root.join("fake-bin").display()),
            )
            .env("PRODUCTION_IMAGE", "fixture:production")
            .env("CONTAINER_TEST_ARTIFACTS", legacy.root.join("artifacts"))
            .stdin(Stdio::null())
            .output()
            .unwrap();
        let expected = output.status.code().unwrap();
        assert!(
            matches!(expected, 0 | 1 | 22),
            "owned legacy fixture {bad} failed unexpectedly"
        );
        let native = Fixture::new();
        let mut fake = native.fake();
        fake.bad = bad;
        let outcome = execute::run(
            &native.context,
            ENDPOINT,
            &mut fake,
            &mut FakeClock::default(),
        )
        .await
        .unwrap();
        let actual = outcome
            .result
            .as_ref()
            .err()
            .map_or(0, |error| i32::from(error.code));
        if bad == "port" && expected == 0 {
            // Bash3.2's errexit ignores this failed [[ ]] command. The typed
            // implementation deliberately enforces the stated loopback check.
            assert_eq!(actual, 1);
        } else {
            assert_eq!(actual, expected, "{bad}");
        }
        let removed = fs::read_to_string(state.join("removed")).unwrap();
        assert!(removed.lines().count() > 0);
        if bad.is_empty() {
            assert!(state.join("registeredRequest").is_file());
            assert!(state.join("restarted").is_file());
            assert_eq!(fake.restarted, [APPID]);
        }
    }
}
