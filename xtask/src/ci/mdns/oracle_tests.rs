//! The driver executes the actual unchanged JS and all nine retained tests.
use super::tests::*;
use super::*;
use crate::{
    ci::{
        command::{Command, Program},
        process::{Executor, Native},
    },
    owned_process::Signals,
    tooling::Result,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{path::Path, sync::Arc, time::Duration};

fn sha(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
#[test]
fn original_source_and_tests_are_byte_retained() {
    assert_eq!(
        sha(include_bytes!("reference/test-mdns.mjs")),
        "cf4ba79746d13d89bc31600a7ca2ed3109779aa06c905ef3fdf486e7cd271624"
    );
    assert_eq!(
        sha(include_bytes!("reference/test-mdns.test.mjs")),
        "500f33614a53ccee5861c63d9c612cf72afcd72e87768ea049f122aaf6b0bafd"
    );
}
async fn reference(value: &Value, self_test: bool) -> Result<Value> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let node = crate::ci::release_checks::files::select(
        root,
        &crate::tooling::inherited_env(),
        std::ffi::OsStr::new("node"),
    )?;
    let node = Arc::new(node);
    let fixture = Fixture::new("snapshot");
    let input = fixture.root.join("input.json");
    std::fs::write(&input, serde_json::to_vec(value).unwrap()).unwrap();
    let file = std::fs::File::open(&input).unwrap();
    let mut temporary = files::Temporary::new(&fixture.config)?;
    let logs = temporary.logs(0).await?;
    let mut command = Command::capture(
        Program::Node,
        &["--experimental-vm-modules", "--no-warnings"],
    );
    command
        .arguments
        .push(root.join("xtask/src/ci/mdns/oracle.mjs").into_os_string());
    if self_test {
        command.arguments.push("--self-test".into());
    }
    command.stdin = Some(Arc::new(file));
    command.capture = None;
    command.timeout = Duration::from_secs(10);
    command.separate_logs = Some(Arc::new(logs));
    command.node = Some(node.clone());
    let mut native = Native::new(root, &fixture.config.environment, Signals::new()?);
    let outcome = native.run(command).await;
    let bytes = temporary.read(0);
    let closed = temporary.finish();
    let selected = Arc::try_unwrap(node).unwrap().close();
    outcome?;
    closed?;
    selected?;
    serde_json::from_slice(&bytes?).map_err(|_| failure(1))
}

#[tokio::test]
async fn all_nine_original_tests_execute_with_inert_system_boundaries() {
    assert_eq!(
        reference(&json!({}), true).await.unwrap(),
        json!({"unchangedReferenceTests":9})
    );
}
#[tokio::test]
async fn whole_native_controller_matches_actual_js_common_domain_reports() {
    let mut cases = vec![
        json!({"mode":"preflight"}),
        json!({"mode":"snapshot"}),
        json!({"mode":"preflight","plistLabel":"PRIVATE unknown"}),
        json!({"mode":"preflight","compileExit":42}),
        json!({"mode":"preflight","probeExit":124}),
        json!({"mode":"preflight","probeStdout":concat!(
            "{\"stage\":\"callback\",\"error\":0.0,\"callbackObserved\":true,",
            "\"timedOut\":false,\"deallocated\":true,\"PRIVATE\":\"SECRET\"}")}),
    ];
    for error in ["-0", "1e0", "1,\"error\":0"] {
        cases.push(json!({"mode":"preflight","probeStdout":format!(
            concat!("{{\"stage\":\"callback\",\"error\":{error},\"callbackObserved\":true,",
                "\"timedOut\":false,\"deallocated\":true}}"),error=error
        )}));
    }
    let events = ["9007199254740991", "9007199254740992", "1e2", "-0", "0.0"]
        .into_iter()
        .map(|n| {
            json!({"eventMessage":format!(
            "PRIVATE registerMDNSNameCallback with error {n}")})
        })
        .chain([json!({"eventMessage":format!(
            "registerMDNSNameCallback with error {}","9".repeat(400))})])
        .map(|v| v.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    cases.push(json!({"mode":"snapshot","logs":events}));
    cases.push(
        json!({"mode":"snapshot","address":"192.0.2.1","interfaceRows":[
        {"name":"stf0","flags":[],"addresses":[]},
        {"name":"lo0","flags":["UP","RUNNING","LOOPBACK"],"addresses":["127.0.0.1"]},
        {"name":"en0","flags":["UP","RUNNING"],"addresses":["192.0.2.1","192.0.2.1"]},
        {"name":"en1","flags":["UP"],"addresses":["198.51.100.2"]}]}),
    );
    cases.push(
        json!({"mode":"snapshot","address":"127.0.0.1","interfaceRows":[
        {"name":"lo0","flags":["UP","RUNNING","LOOPBACK"],"addresses":["192.0.2.1"]},
        {"name":"en0","flags":["UP","RUNNING"],"addresses":["127.0.0.1"]}]}),
    );
    let actual = reference(&json!({"cases":cases}), false).await.unwrap();
    for (index, case) in cases.iter().enumerate() {
        let mut f = Fixture::new(case["mode"].as_str().unwrap());
        let mut e = f.executor();
        if let Some(address) = case.get("address").and_then(Value::as_str) {
            f.config.address = address.into();
            let listing = if address == "127.0.0.1" {
                concat!(
                    "lo0: flags=49<UP,RUNNING,LOOPBACK> metric 3 mtu 16384\n",
                    " inet 192.0.2.1 netmask x\nen0: flags=41<UP,RUNNING>\n",
                    " inet 127.0.0.1 netmask x\n"
                )
            } else {
                concat!(
                    "stf0: flags=0<> mtu 1280\n",
                    "lo0: flags=49<UP,RUNNING,LOOPBACK> mtu 16384\n inet 127.0.0.1 netmask x\n",
                    "en0: flags=41<UP,RUNNING> mtu 1500\n",
                    " inet 192.0.2.1 netmask x\n inet 192.0.2.1 netmask x\n",
                    "en1: flags=1<UP> mtu 1500\n inet 198.51.100.2 netmask x\n"
                )
            };
            e.cases.get_mut(&tools::Role::Interfaces).unwrap().0 = listing.as_bytes().to_vec();
        }
        if let Some(label) = case.get("plistLabel").and_then(Value::as_str) {
            e.cases.get_mut(&tools::Role::Plist).unwrap().0 = label.as_bytes().to_vec();
        }
        for (key, role) in [
            ("compileExit", tools::Role::Compiler),
            ("probeExit", tools::Role::Probe),
        ] {
            if let Some(code) = case.get(key).and_then(Value::as_u64) {
                e.cases.get_mut(&role).unwrap().1 = code as u8;
            }
        }
        if let Some(bytes) = case.get("probeStdout").and_then(Value::as_str) {
            e.cases.get_mut(&tools::Role::Probe).unwrap().0 = bytes.as_bytes().to_vec();
        }
        if let Some(bytes) = case.get("logs").and_then(Value::as_str) {
            e.cases.get_mut(&tools::Role::Logs).unwrap().0 = bytes.as_bytes().to_vec();
        }
        let result = execute::run(&f.config, &mut e, &f.clock, &f.paths).await;
        let status = result.err().map_or(0, |e| e.code);
        let mut report = f.report();
        report.as_object_mut().unwrap().remove("startedAt");
        report.as_object_mut().unwrap().remove("finishedAt");
        assert_eq!(
            status,
            actual["results"][index]["status"].as_u64().unwrap() as u8,
            "case{index}"
        );
        assert_eq!(report, actual["results"][index]["report"], "case{index}");
        let commands = e
            .calls
            .iter()
            .enumerate()
            .filter(|(_, role)| **role != tools::Role::Interfaces)
            .map(|(i, role)| {
                let role_name = match role {
                    tools::Role::Plist => "plutil",
                    tools::Role::Launch => "launchctl",
                    tools::Role::Logs => "log",
                    tools::Role::Compiler => "cc",
                    tools::Role::Probe => "probe",
                    _ => unreachable!(),
                };
                let args = if *role == tools::Role::Probe {
                    vec!["<assigned-ipv4>".into(), "<uuid>".into()]
                } else {
                    e.arguments[i]
                        .iter()
                        .map(|s| {
                            if s.ends_with("/test-mdns.c") {
                                "<probe-source>".into()
                            } else if s.ends_with("/probe") {
                                "<probe-binary>".into()
                            } else {
                                s.clone()
                            }
                        })
                        .collect()
                };
                json!({"role":role_name,"args":args,"timeoutMs":e.timeouts[i]})
            })
            .collect::<Vec<_>>();
        assert_eq!(
            json!(commands),
            actual["results"][index]["commands"],
            "case{index}"
        );
        f.no_temporary();
    }
}

#[test]
fn finite_json_profile_refuses_unknown_numeric_overflow_and_unpaired_surrogates() {
    for private in ["1e309", r#""\ud800""#] {
        let observation = policy::Observation {
            stdout: format!(
                concat!(
                    "{{\"stage\":\"callback\",\"error\":0,\"callbackObserved\":true,",
                    "\"timedOut\":false,\"deallocated\":true,\"PRIVATE\":{private}}}"
                ),
                private = private
            )
            .into_bytes(),
            exit: Some(0),
            timed_out: false,
        };
        assert_eq!(policy::probe(&observation)["stage"], "invalid_report");
        assert_eq!(policy::probe(&observation)["passed"], false);
    }
}
