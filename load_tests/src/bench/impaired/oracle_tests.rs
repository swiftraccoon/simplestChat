//! Run the unchanged shell with only owned fake tools: no sockets or kernel calls.

use super::tests::{Fault, Fixture, executable};
use crate::bench::process::{self, Command, NativeHost, Sink};
use serde_json::Value;
use std::{ffi::OsString, fs, path::Path, time::Duration};

fn python(path: &Path, source: &str) {
    executable(path, format!("#!/usr/bin/python3\n{source}\n").as_bytes());
}

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
        let quoted = serde_json::to_string(fixture.root.to_str().unwrap()).unwrap();
        python(
            &fixture.root.join("server"),
            &format!(
                r#"
import json,os,pathlib,signal,sys
root=pathlib.Path({quoted})
signal.signal(signal.SIGTERM,lambda *_:sys.exit(0))
(root/'server-env.json').write_text(json.dumps(dict(os.environ)))
(root/'ready.part').write_text('ready')
os.replace(root/'ready.part',root/'server.ready')
while True: signal.pause()
"#
            ),
        );
        python(
            &fixture.root.join("curl"),
            &format!(
                r#"
import pathlib,time,sys
ready=pathlib.Path({quoted})/'server.ready'
for _ in range(200):
 if ready.exists():sys.exit(0)
 time.sleep(.01)
sys.exit(1)
"#
            ),
        );
        python(
            &fixture.root.join("generator"),
            &format!(
                r#"
import json,os,pathlib,sys
root=pathlib.Path({quoted})
(root/'generator-argv.json').write_text(json.dumps(sys.argv[1:]))
(root/'generator-env.json').write_text(json.dumps(dict(os.environ)))
output=pathlib.Path(sys.argv[sys.argv.index('--output-dir')+1])
keys=['successfulConnections','failedConnections','validatedConsumers','failedConsumers',
 'keyframesRequested','bandwidthEstimates','totalPacketsReceived','totalErrors']
(output/'load_test_summary.json').write_text(json.dumps(dict(zip(keys,range(8)))))
sys.exit({status})
"#
            ),
        );
        for name in ["sudo", "tc"] {
            python(
                &fixture.root.join(name),
                &format!(
                    r#"
import json,pathlib,sys
with (pathlib.Path({quoted})/'kernel-argv.jsonl').open('a') as output:
 output.write(json.dumps([pathlib.Path(sys.argv[0]).name,*sys.argv[1:]])+'\n')
print('owned fixture state')
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
        let exported: std::collections::BTreeMap<String, String> =
            serde_json::from_slice(&fs::read(fixture.root.join("generator-env.json")).unwrap())
                .unwrap();
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
        let server: std::collections::BTreeMap<String, String> =
            serde_json::from_slice(&fs::read(fixture.root.join("server-env.json")).unwrap())
                .unwrap();
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
