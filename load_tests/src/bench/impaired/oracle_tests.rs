//! Run the unchanged shell with only owned fake tools: no sockets or kernel calls.

use super::tests::{Fault, Fixture, executable, saved_environment, shell};
use crate::bench::process::{self, Command, NativeHost, Sink};
use serde_json::Value;
use std::{ffi::OsString, fs, path::Path, time::Duration};

#[tokio::test]
async fn actual_reference_keeps_exact_workload_clean_server_environment_and_delivery_status_evidence()
 {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    for status in [0, 1, 42] {
        let (fixture, mut plan) = Fixture::new(Fault::None, status);
        fs::create_dir(fixture.root.join("build")).unwrap();
        for name in ["impaired-generator.sh", "impair.sh"] {
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
trap 'kill "$sleeper" 2>/dev/null; exit 0' TERM
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
            &fixture.root.join("generator"),
            &format!(
                r#"
root='{root}'
json_strings "$@" >"$root/generator-argv.json"
/usr/bin/env >"$root/generator-environment"
output=$(value --output-dir "$@") || exit 1
printf '{{"successfulConnections": 0, "failedConnections": 1, "validatedConsumers": 2, ' \
  >"$output/load_test_summary.json"
printf '"failedConsumers": 3, "keyframesRequested": 4, "bandwidthEstimates": 5, ' \
  >>"$output/load_test_summary.json"
printf '"totalPacketsReceived": 6, "totalErrors": 7}}' >>"$output/load_test_summary.json"
exit {status}
"#
            ),
        );
        for name in ["sudo", "tc"] {
            shell(
                &fixture.root.join(name),
                &format!(
                    r#"
printf '%s\n' "$(json_strings "${{0##*/}}" "$@")" >>'{root}/kernel-argv.jsonl'
printf 'owned fixture state\n'
"#
                ),
            );
        }
        executable(
            &fixture.root.join("date"),
            b"#!/bin/sh\nprintf '120000\\n'\n",
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
        environment.insert("HOME".into(), fixture.root.clone().into_os_string());
        environment.insert("TMPDIR".into(), fixture.root.clone().into_os_string());
        plan.environment = environment.clone();
        let output = process::run(
            &NativeHost::default(),
            Command {
                executable: "/bin/sh".into(),
                arguments: vec![
                    "build/impaired-generator.sh".into(),
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
        // The old wrapper admits even an abnormal42 when a summary exists.
        // The native controller deliberately corrects that case, while keeping0/1.
        assert!(output.exit.success());
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(stdout.contains(&format!("generator exit {status}; server exit 0")));
        let actual: Vec<String> =
            serde_json::from_slice(&fs::read(fixture.root.join("generator-argv.json")).unwrap())
                .unwrap();
        let mut expected = plan.generator_arguments();
        let room = expected.iter().position(|value| value == "--room").unwrap() + 1;
        expected[room] = "impaired-120000".into();
        assert_eq!(actual, expected);
        let legacy: Value =
            serde_json::from_str(stdout.lines().find(|line| line.starts_with('{')).unwrap())
                .unwrap();
        let mut selected = super::tests::summary(&plan, status);
        for (key, value) in legacy.as_object().unwrap() {
            selected[key] = value.clone();
        }
        let parsed = crate::bench::summary::parse(&selected).unwrap();
        assert_eq!(parsed.impaired_projection(&selected).unwrap(), legacy);
        let exported = saved_environment(&fixture.root.join("generator-environment"));
        for key in [
            "IMPAIR_UDP_PORT",
            "IMPAIR_WORKERS",
            "IMPAIR_DIRECTION",
            "IMPAIR_LOSS",
            "IMPAIR_DELAY_MS",
            "IMPAIR_JITTER_MS",
        ] {
            assert_eq!(
                exported[key],
                plan.environment[&OsString::from(key)].to_str().unwrap()
            );
        }
        let server = saved_environment(&fixture.root.join("server-environment"));
        let expected = plan.server_environment();
        for (key, value) in expected {
            assert_eq!(server[key.to_str().unwrap()], value.to_str().unwrap());
        }
        assert!(!server.contains_key("DATABASE_URL"));
        let calls: Vec<Value> = fs::read_to_string(fixture.root.join("kernel-argv.jsonl"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let additions: Vec<Vec<String>> = calls
            .into_iter()
            .filter_map(|value| {
                let arguments: Vec<String> = serde_json::from_value(value).unwrap();
                (arguments.first().is_some_and(|v| v == "sudo")
                    && arguments.iter().any(|v| v == "add"))
                .then(|| arguments[2..].to_vec())
            })
            .collect();
        assert_eq!(additions, plan.profile.additions());
        assert!(!plan.output.join("server.ready").exists());
    }
}
