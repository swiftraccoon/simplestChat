//! The unchanged launcher is exercised only with owned inert fixture tools.

use super::super::tests::{saved_environment, shell};
use super::{
    plan::{Mode, Plan},
    tests::{Fault, Fixture, executable},
};
use crate::bench::process::{self, Command, NativeHost, Sink};
use std::{ffi::OsString, fs, path::Path, time::Duration};

#[tokio::test]
async fn unchanged_launcher_proves_guest_env_profile_inputs_status_and_cleanup_corrections() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    for (mode, node_status, server_status) in [
        (Mode::Mechanics, 0, 0),
        (Mode::Mechanics, 42, 0),
        (Mode::Mechanics, 130, 0),
        (Mode::Mechanics, 0, 42),
        (Mode::Adaptive, 0, 0),
        (Mode::Adaptive, 42, 0),
        (Mode::Adaptive, 143, 0),
        (Mode::Adaptive, 0, 42),
    ] {
        let (fixture, mut plan) = Fixture::new(Fault::None, node_status);
        fs::create_dir(fixture.root.join("build")).unwrap();
        if mode.adaptive() {
            executable(&fixture.root.join("tc"), b"owned inert tc selection");
            fs::write(
                fixture.root.join("web/e2e/browser-adaptive.cjs"),
                b"owned inert entry",
            )
            .unwrap();
        }
        for name in ["impaired-check.sh", "impair.sh"] {
            fs::copy(
                repository.join("build").join(name),
                fixture.root.join("build").join(name),
            )
            .unwrap();
        }
        let root = fixture.root.to_str().unwrap();
        assert!(!root.contains('\''));
        shell(
            &fixture.root.join("server"),
            &format!(
                r#"
root='{root}'
sleeper=
trap 'kill "$sleeper" 2>/dev/null; exit {server_status}' TERM
/usr/bin/env >"$root/server-environment"
printf ready >"$root/ready.part"
/bin/mv -f "$root/ready.part" "$root/server.ready"
while :; do
  /bin/sleep 1 >/dev/null 2>&1 &
  sleeper=$!
  wait "$sleeper"
done
"#
            ),
        );
        shell(
            &fixture.root.join("curl"),
            &format!(
                r#"
attempt=0
while [ "$attempt" -lt 200 ]; do
  if [ -e '{root}/server.ready' ]; then exit 0; fi
  /bin/sleep 0.01
  attempt=$((attempt + 1))
done
exit 1
"#
            ),
        );
        shell(
            &fixture.root.join("node"),
            &format!(
                r#"
root='{root}'
/usr/bin/env >"$root/node-environment"
json_strings "$@" >"$root/node-argv.json"
exit {node_status}
"#
            ),
        );
        shell(
            &fixture.root.join("sudo"),
            &format!(
                r#"
printf '%s\n' "$(json_strings "$@")" >>'{root}/kernel.jsonl'
"#
            ),
        );
        executable(
            &fixture.root.join("uname"),
            b"#!/bin/sh\nprintf 'Linux\\n'\n",
        );
        let mut environment = plan.environment.clone();
        let mut paths = vec![fixture.root.clone()];
        paths.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        environment.insert("PATH".into(), std::env::join_paths(paths).unwrap());
        environment.insert("TMPDIR".into(), fixture.root.clone().into_os_string());
        for (name, value) in [
            (
                "IMPAIR_SCRIPT",
                if mode.adaptive() {
                    "rust-owned"
                } else {
                    "none"
                },
            ),
            (
                "IMPAIRED_PROFILES",
                if mode.adaptive() {
                    "baseline,lossy,constrained,severe,recovery,lossyJoin"
                } else {
                    "baseline,silentAudio"
                },
            ),
            (
                "IMPAIRED_SILENT_AUDIO",
                if mode.adaptive() { "0" } else { "1" },
            ),
            ("E2E_BROWSER", "chromium"),
        ] {
            environment.insert(name.into(), value.into());
        }
        environment.insert("ACT".into(), "true".into());
        environment.insert("LOCAL_CI_DISPOSABLE".into(), "1".into());
        plan = Plan::parse_mode(
            &fixture.root,
            &["--output".into(), plan.output.clone().into_os_string()],
            environment.clone(),
            mode,
            "linux",
        )
        .unwrap();
        let output = process::run(
            &NativeHost::default(),
            Command {
                executable: "/bin/sh".into(),
                arguments: vec![
                    "build/impaired-check.sh".into(),
                    plan.output.to_str().unwrap().into(),
                ],
                directory: fixture.root.clone(),
                environment,
                timeout: Duration::from_secs(8),
                stdout_limit: 65536,
                stderr_limit: 65536,
                input: Vec::new(),
                stdout: Sink::Capture,
                stderr: Sink::Capture,
                cleanup: false,
            },
        )
        .await
        .unwrap();
        assert_eq!(output.exit.code, Some(node_status));
        let read = |name: &str| {
            serde_json::from_slice::<serde_json::Value>(&fs::read(fixture.root.join(name)).unwrap())
                .unwrap()
        };
        let server = saved_environment(&fixture.root.join("server-environment"));
        let expected = plan.server_environment();
        for (name, value) in expected {
            assert_eq!(server[name.to_str().unwrap()], value.to_str().unwrap());
        }
        assert!(!server.contains_key("DATABASE_URL"));
        assert!(!server.contains_key("NODE_PATH"));
        let node = saved_environment(&fixture.root.join("node-environment"));
        let expected = plan.node_environment();
        for name in [
            "BASE_URL",
            "IMPAIR_UDP_PORT",
            "IMPAIR_WORKERS",
            "IMPAIR_SCRIPT",
            "IMPAIRED_PROFILES",
            "IMPAIRED_SILENT_AUDIO",
            "E2E_BROWSER",
            "E2E_ARTIFACTS",
            "PLAYWRIGHT_BROWSERS_PATH",
        ] {
            assert_eq!(
                node[name],
                expected[&OsString::from(name)].to_str().unwrap()
            );
        }
        assert_eq!(
            read("node-argv.json"),
            serde_json::json!(["web/e2e/impaired-network.cjs"])
        );
        // The old trap mutates kernel state even with IMPAIR_SCRIPT=none and
        // ignores server42. Both are explicit corrections in the native mode.
        let kernel = fs::read_to_string(fixture.root.join("kernel.jsonl")).unwrap();
        let calls: Vec<serde_json::Value> = kernel
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(calls.len(), 3);
        assert_eq!(
            calls[0],
            serde_json::json!(["tc", "qdisc", "del", "dev", "lo", "root"])
        );
        assert!(
            calls[1..]
                .iter()
                .all(|call| call[0] == "iptables" && call[1] == "-D")
        );
    }
}
