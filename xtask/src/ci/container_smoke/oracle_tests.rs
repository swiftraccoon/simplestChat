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

const STAND_IN: &str = r#"#!/usr/bin/python3
import json,sys
from pathlib import Path
root=Path(__file__).parent.parent
state_path=root/'oracle-state.json'
state=json.loads(state_path.read_text())
bad=state['bad'];args=sys.argv[1:];program=Path(sys.argv[0]).name
def value(flag):return args[args.index(flag)+1]
def finish(code=0):
 state_path.write_text(json.dumps(state));raise SystemExit(code)
if program=='sleep':finish()
if program=='docker':
 if args[0] in ['info','image']:finish()
 if args[0]=='run':
  database='--publish' in args;name='database' if database else 'app'
  state[name]=True;print('d'*64 if database else 'e'*64);finish()
 if args[0]=='inspect':
  app=args[-1]=='e'*64
  print('false' if app and state.get('restarted') and bad=='restart' else 'true');finish()
 if args[0]=='exec':
  if args[2]=='pg_isready':finish()
  if '_sqlx_migrations' in args[-1]:print('0' if bad=='migration-zero' else '1' if bad=='migration-count' else '2')
  else:print('0' if bad=='registered' else '1')
  finish()
 if args[0]=='port':print('0.0.0.0:38001' if bad=='port' else '127.0.0.1:38001');finish()
 if args[0]=='restart':state['restarted']=True;print(args[-1]);finish()
 if args[0]=='logs':print('PRIVATE inert guest logs');finish()
 if args[0]=='rm':state['removed'].append(args[-1]);finish()
 finish(99)
if program=='curl':
 route=args[-1].split(':38001')[-1]
 if route=='/api/auth/register':
  assert value('--output')=='/dev/null';assert json.loads(value('--data'))['email']=='container-smoke@example.test'
  state['registeredRequest']=True;finish()
 if route=='/api/rooms':data=b'[{}]' if bad=='rooms' else b'[]\n\n'
 elif route=='/health':
  if bad=='health-command':finish(22)
  data=b'wrong' if bad=='health' else b'{"status":"ok"}\n\n'
 elif route=='/':
  if bad=='html-command':finish(22)
  data=b'not html' if bad=='html' else b'<!DOCTYPE HTML><title>owned</title>'
 else:
  asset=route[1:];data=b'wrong' if bad=='asset-bytes' else (root/'web/public'/asset).read_bytes()
  mime='text/plain' if bad=='mime' else 'application/manifest+json' if asset=='manifest.webmanifest' else 'application/javascript' if asset=='sw.js' else 'image/png'
  Path(value('--output')).write_bytes(data);print(mime+'; charset=utf-8');finish()
 sys.stdout.buffer.write(data);finish()
finish(98)
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
            .unwrap()
            .join("build/test-container.sh");
        let script = legacy.root.join("build/test-container.sh");
        fs::copy(reference, &script).unwrap();
        for name in ["docker", "curl", "sleep"] {
            let path = legacy.root.join("fake-bin").join(name);
            fs::write(&path, STAND_IN).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        fs::write(
            legacy.root.join("oracle-state.json"),
            serde_json::to_vec(&json!({"bad":bad,"removed":[]})).unwrap(),
        )
        .unwrap();
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
        let state: Value =
            serde_json::from_slice(&fs::read(legacy.root.join("oracle-state.json")).unwrap())
                .unwrap();
        assert!(!state["removed"].as_array().unwrap().is_empty());
        if bad.is_empty() {
            assert_eq!(state["registeredRequest"], true);
            assert_eq!(state["restarted"], true);
            assert_eq!(fake.restarted, [APPID]);
        }
    }
}
