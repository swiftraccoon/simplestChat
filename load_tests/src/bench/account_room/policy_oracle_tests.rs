//! Actual retained JavaScript functions are the pure-policy oracle.

use super::{tests, *};
use crate::bench::process::{self, Command, NativeHost, Sink};
use std::{path::PathBuf, time::Duration};

struct Case {
    name: &'static str,
    arguments: Vec<Value>,
    result: Result<Value>,
}
fn same(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Number(_), Value::Number(_)) => left.as_f64() == right.as_f64(),
        (Value::Array(left), Value::Array(right)) => {
            left.len() == right.len()
                && left
                    .iter()
                    .zip(right)
                    .all(|(left, right)| same(left, right))
        }
        (Value::Object(left), Value::Object(right)) => {
            left.len() == right.len()
                && left
                    .iter()
                    .all(|(key, left)| right.get(key).is_some_and(|right| same(left, right)))
        }
        _ => left == right,
    }
}
async fn compare(cases: Vec<Case>) {
    let root = std::env::var_os("ACCOUNT_ROOM_POLICY_ROOT").map_or_else(
        || {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap()
                .to_owned()
        },
        PathBuf::from,
    );
    let reference = root.join("build/account-room-check.mjs");
    let program = format!(
        r#"
import fs from 'node:fs';import {{pathToFileURL}} from 'node:url';
const m=await import(pathToFileURL({}).href);
const cases=JSON.parse(fs.readFileSync(0,'utf8'));
const results=cases.map(c=>{{try{{return {{ok:true,value:m[c.name](...c.arguments)}};}}
catch(error){{return {{ok:false,code:/^[a-z0-9_]+$/.test(error.message??'')?error.message:'evidence_validation_failed'}};}}}});
process.stdout.write(JSON.stringify(results));
"#,
        serde_json::to_string(&reference).unwrap()
    );
    let node = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|path| path.join("node"))
        .find(|path| path.is_file())
        .unwrap();
    let input: Vec<_> = cases
        .iter()
        .map(|case| json!({"name":case.name,"arguments":case.arguments}))
        .collect();
    let bytes = serde_json::to_vec(&input).unwrap();
    assert!(bytes.len() <= 32 * 1024 * 1024, "Bounded pure oracle input");
    let output = process::run(
        &NativeHost::default(),
        Command {
            executable: node,
            arguments: vec!["--input-type=module".into(), "-e".into(), program],
            directory: root,
            environment: [("PATH".into(), std::env::var_os("PATH").unwrap_or_default())].into(),
            timeout: Duration::from_secs(15),
            stdout_limit: 4 * 1024 * 1024,
            stderr_limit: 65536,
            input: bytes,
            stdout: Sink::Capture,
            stderr: Sink::Capture,
            cleanup: false,
        },
    )
    .await
    .unwrap();
    assert!(
        output.exit.success(),
        "Actual pure JavaScript oracle must finish"
    );
    assert!(output.stderr.is_empty());
    let expected: Vec<Value> = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(expected.len(), cases.len());
    for (index, (case, expected)) in cases.into_iter().zip(expected).enumerate() {
        match case.result {
            Ok(actual) => assert!(
                expected["ok"] == true && same(&actual, &expected["value"]),
                "case{index} {} result differs: actual={actual},expected={expected}",
                case.name
            ),
            Err(error) => assert!(
                expected["ok"] == false,
                "case{index} {} must also refuse in reference: {}",
                case.name,
                failure_code(&error)
            ),
        }
    }
}

#[tokio::test]
async fn generator_authentication_retention_and_parser_policies_match_actual_javascript() {
    let mut cases = Vec::new();
    for patch in [
        json!({}),
        json!({"failedConsumers":1}),
        json!({"validatedConsumers":1139}),
        json!({"failedConnections":1}),
        json!({"successfulConnections":29}),
        json!({"totalClients":29}),
    ] {
        for (code, signal) in [(0, None), (1, None), (42, None), (0, Some("SIGTERM"))] {
            let mut summary = tests::summary();
            summary
                .as_object_mut()
                .unwrap()
                .extend(patch.as_object().unwrap().clone());
            let exit = Exit {
                code: Some(code),
                signal: signal.map(str::to_owned),
            };
            cases.push(Case {
                name: "generatorVerdict",
                arguments: vec![summary.clone(), json!({"code":code,"signal":signal})],
                result: generator_verdict(&summary, &exit),
            });
        }
    }
    for (path, value) in [("passed", json!(false)), ("completed", json!(false))] {
        let mut source = tests::summary();
        source["run"][path] = value;
        cases.push(Case {
            name: "generatorVerdict",
            arguments: vec![source.clone(), json!({"code":0,"signal":null})],
            result: generator_verdict(
                &source,
                &Exit {
                    code: Some(0),
                    signal: None,
                },
            ),
        });
    }
    for (field, value) in [
        ("passed", json!(true)),
        ("passed", json!(false)),
        ("httpRefreshes", json!(0)),
        ("socketRenewals", json!(0)),
        ("originalExpiryCrossed", json!(false)),
        ("latestExpiresAt", json!(1800000000)),
        ("failures", json!(1)),
        ("accountId", json!(tests::accounts()[1])),
    ] {
        let mut source = tests::authentication();
        source[0][field] = value;
        cases.push(Case {
            name: "authenticationVerdict",
            arguments: vec![source.clone(), json!(tests::accounts())],
            result: authentication_verdict(&source, &tests::accounts()),
        });
    }
    for patch in [
        json!({}),
        json!({"content":"changed"}),
        json!({"senderAccountId":tests::accounts()[1]}),
        json!({"clientMessageId":"foreign"}),
        json!({"messageId":"foreign"}),
    ] {
        let (mut rows, inventory) = tests::retained();
        rows[0]
            .as_object_mut()
            .unwrap()
            .extend(patch.as_object().unwrap().clone());
        cases.push(Case {
            name: "retentionVerdict",
            arguments: vec![rows.clone(), inventory.clone(), json!(tests::accounts())],
            result: retention_verdict(&rows, &inventory, &tests::accounts()),
        });
    }
    for raw in [
        "#empty\n",
        "simplestchat_connections_active 30\n",
        "simplestchat_connections_active 30\r\n",
        "simplestchat_connections_active 30\u{2028}",
        "simplestchat_connections_active 30\u{2029}",
        "simplestchat_connections_active -0\nsimplestchat_x{kind=\"a\"} 1.25e-2\n",
        "simplestchat_connections_active 29\nsimplestchat_connections_active 30\nsimplestchat_bad 1e999\n",
    ] {
        cases.push(Case {
            name: "parseMetrics",
            arguments: vec![json!(raw)],
            result: parse_metrics(raw).map(Value::Object),
        });
    }
    for raw in [
        "{\"connections\":5,\"active\":1,\"waiting\":0}\nTime: 0.321 ms\n",
        "{\"connections\":5}\n",
        "{\"connections\":5,\"active\":1,\"waiting\":0}\nTime: 1.2 ms (1 row)\n",
    ] {
        cases.push(Case {
            name: "parseDatabaseProbe",
            arguments: vec![json!(raw)],
            result: parse_database_probe(raw),
        });
    }
    for (stat, maximum) in [
        (
            "usage_usec 500\nnr_periods 10\nnr_throttled 1\n",
            "200000 100000\n",
        ),
        (
            "usage_usec 500\nnr_periods 0\nnr_throttled 0\n",
            "max 100000",
        ),
        ("usage_usec 500", "max 100000"),
    ] {
        cases.push(Case {
            name: "parseCgroup",
            arguments: vec![json!(stat), json!(maximum)],
            result: parse_cgroup(stat, maximum),
        });
    }
    for (stat, memory) in [
        ("cpu 10 0 20 60 5 2 3 0 0 0\n", "MemAvailable: 500000 kB\n"),
        ("cpu 1 2 3\n", "MemAvailable: 500000 kB\n"),
        ("cpu 10 0 20 60 5 2 3 0\n", "MemAvailable: 0 kB\n"),
    ] {
        cases.push(Case {
            name: "parseHost",
            arguments: vec![json!(stat), json!(memory)],
            result: parse_host(stat, memory),
        });
    }
    compare(cases).await;
}

#[tokio::test]
async fn complete_resource_verdict_thresholds_and_coverage_match_actual_javascript() {
    let source = tests::samples();
    let mut cases = Vec::new();
    let mut variants = vec![source.clone()];
    for (field, value) in [
        ("simplestchat_media_worker_cpu{worker=\"0\"}", json!(0.8)),
        ("simplestchat_media_worker_cpu{worker=\"0\"}", json!(0.7)),
        ("simplestchat_memory_saturated", json!(1)),
        ("simplestchat_participants_active", json!(29)),
        (
            "simplestchat_database_errors_total{kind=\"pool_timeout\"}",
            json!(1),
        ),
    ] {
        let mut invalid = source.clone();
        invalid[100]["metrics"][field] = value;
        variants.push(invalid);
    }
    let mut invalid = source.clone();
    invalid[100]["server"]["processes"][1]["startTicks"] = json!(2000);
    variants.push(invalid);
    let mut invalid = source.clone();
    invalid[2400]["cgroup"]["throttledPeriods"] = json!(200);
    variants.push(invalid);
    let mut invalid = source.clone();
    for row in &mut invalid {
        row["cgroup"] = Value::Null;
    }
    variants.push(invalid);
    variants.push(source[20..].to_vec());
    // Batch two large fixture windows at a time so the subprocess input stays
    // bounded independently of the number of parity cases.
    for rows in variants {
        let result = measurement_resources(&rows, 0.0, 1200000.0);
        cases.push(Case {
            name: "measurementResources",
            arguments: vec![json!(rows), json!(0), json!(1200000)],
            result,
        });
        if cases.len() == 2 {
            compare(std::mem::take(&mut cases)).await;
        }
    }
    if !cases.is_empty() {
        compare(cases).await;
    }
}

#[tokio::test]
async fn exact_headroom_coverage_percentiles_and_window_edges_match_actual_javascript() {
    let source = tests::samples();
    for (kind, amount) in [
        ("busy", 0.85),
        ("busy", 0.850001),
        ("steal", 0.05),
        ("steal", 0.050001),
        ("throttle", 120.0),
        ("throttle", 121.0),
        ("membership", 24.0),
        ("membership", 25.0),
        ("metrics", 24.0),
        ("metrics", 25.0),
        ("host", 240.0),
        ("host", 241.0),
        ("server", 240.0),
        ("server", 241.0),
        ("edge", 3.0),
        ("edge", 4.0),
        ("gap", 2.0),
        ("gap", 3.0),
        ("distributions", 0.0),
    ] {
        let mut rows = source.clone();
        match kind {
            "busy" | "steal" => {
                for (index, row) in rows.iter_mut().enumerate() {
                    let counter = if kind == "busy" { 1.0 - amount } else { amount };
                    row["host"][if kind == "busy" { "idle" } else { "steal" }] =
                        number(index as f64 * 200.0 * counter);
                }
            }
            "throttle" => rows[2400]["cgroup"]["throttledPeriods"] = number(amount),
            "membership" | "metrics" => {
                for index in 0..amount as usize {
                    if kind == "membership" {
                        rows[index * 10]["metrics"]["simplestchat_participants_snapshot_complete"] =
                            json!(0);
                    } else {
                        rows[index * 10]["database"] = Value::Null;
                    }
                }
            }
            "host" | "server" => {
                for row in rows.iter_mut().skip(800).take(amount as usize) {
                    row[kind] = Value::Null;
                }
            }
            "edge" => {
                rows.drain(..amount as usize);
            }
            "gap" => {
                rows.drain(1001..1001 + amount as usize);
            }
            "distributions" => {
                for (index, row) in rows.iter_mut().enumerate() {
                    row["database"]["queryRoundTripMs"] = number((index % 17) as f64 / 10.0);
                    row["database"]["waiting"] = json!(index % 3);
                    row["server"]["rssKiB"] = json!(100000 + index % 10);
                }
            }
            _ => unreachable!(),
        }
        let result = measurement_resources(&rows, 0.0, 1200000.0);
        compare(vec![Case {
            name: "measurementResources",
            arguments: vec![json!(rows), json!(0), json!(1200000)],
            result,
        }])
        .await;
    }
}
