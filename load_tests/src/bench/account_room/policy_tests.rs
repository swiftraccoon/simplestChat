//! Owned finite observations only; no service or workload.

use super::*;

pub(super) fn accounts() -> Vec<String> {
    (0..CLIENTS)
        .map(|index| format!("00000000-0000-4000-8000-{index:012}"))
        .collect()
}
pub(super) fn authentication() -> Value {
    Value::Array(
        accounts()
            .into_iter()
            .map(|account| {
                json!({"accountId":account,
        "passed":true,"originalExpiresAt":1800000000,"latestExpiresAt":1800000800,
        "originalExpiryCrossed":true,"httpRefreshes":1,"socketRenewals":1,"failures":0})
            })
            .collect(),
    )
}
pub(super) fn summary() -> Value {
    json!({"run":{"completed":true,"passed":true,"failureReasons":[],
        "configuration":{"durationSecs":1200,"rampUpSecs":60,"warmupSecs":30,
            "profile":{"name":"browser"},"maxVideoConsumers":9,"maxAudioConsumers":29}},
        "measurement":{"durationMs":1200000},"totalClients":30,"successfulConnections":30,
        "failedConnections":0,"failedConsumers":0,"validatedConsumers":1140})
}
pub(super) fn retained() -> (Value, Value) {
    let inventory: Vec<_> = accounts().into_iter().enumerate().map(|(index, sender)| {
        json!({"messageId":format!("message-{index}"),"clientMessageId":format!("client-{index}"),
            "senderAccountId":sender,"contentSha256":hex::encode(Sha256::digest(format!("content-{index}").as_bytes()))})
    }).collect();
    let rows = inventory
        .iter()
        .enumerate()
        .map(|(index, value)| {
            let mut value = value.clone();
            value["content"] = json!(format!("content-{index}"));
            value
        })
        .collect();
    (Value::Array(rows), Value::Array(inventory))
}
pub(super) fn samples() -> Vec<Value> {
    (0..=2400)
        .map(|index| {
            json!({"elapsedMs":index*500,
        "server":{"cpuSeconds":number(index as f64*0.1),"rssKiB":100000,"childCount":1,
            "processes":[{"pid":123,"startTicks":1000},{"pid":456,"startTicks":1001}]},
        "generator":{"cpuSeconds":number(index as f64*0.3),"rssKiB":150000},
        "host":{"total":index*200,"idle":index*120,"steal":0,"availableBytes":1073741824},
        "cgroup":{"periods":index,"throttledPeriods":0,"quotaCores":null},
        "metricsExpected":index%10==0,
        "metrics":{"simplestchat_media_worker_cpu{worker=\"0\"}":0.4,
            "simplestchat_cpu_saturated":0,"simplestchat_memory_saturated":0,
            "simplestchat_connections_active":30,"simplestchat_participants_active":30,
            "simplestchat_participants_snapshot_complete":1,"simplestchat_rooms_active":1,
            "simplestchat_database_errors_total{kind=\"pool_timeout\"}":0,
            "simplestchat_db_pool_connections{state=\"used\"}":1,
            "simplestchat_db_pool_connections{state=\"max\"}":10},
        "database":{"queryRoundTripMs":0.5,"waiting":0}})
        })
        .collect()
}

#[test]
fn full_completed_failure_retains_independent_measurement_and_known_fields_only() {
    let mut source = summary();
    source["run"]["passed"] = json!(false);
    source["run"]["failureReasons"] = json!(["one consumer did not deliver"]);
    source["failedConsumers"] = json!(1);
    source["validatedConsumers"] = json!(1139);
    let value = generator_verdict(
        &source,
        &Exit {
            code: Some(1),
            signal: None,
        },
    )
    .unwrap();
    assert_eq!(value["completed"], true);
    assert_eq!(value["fullWindow"], true);
    assert_eq!(value["passed"], false);
    assert_eq!(value["failureReasons"], source["run"]["failureReasons"]);
    for path in ["durationSecs", "rampUpSecs", "warmupSecs"] {
        let mut invalid = summary();
        invalid["run"]["configuration"][path] = json!(1);
        assert_eq!(
            generator_verdict(
                &invalid,
                &Exit {
                    code: Some(0),
                    signal: None
                }
            )
            .unwrap()["fullWindow"],
            false
        );
    }
    let mut source = summary();
    source["chat"] = json!({"passed":true,"acknowledgements":30,"PRIVATE_SENTINEL":"token"});
    source["signalingLatencies"] = json!({"PRIVATE_SENTINEL":"token","operations":{
        "chat_ack":{"count":1,"minMs":1,"maxMs":1,"avgMs":1,"p50Ms":1,"p95Ms":1,
            "p99Ms":1,"histogramMs":{"1":1},"PRIVATE_SENTINEL":"token"}}});
    let value = generator_verdict(
        &source,
        &Exit {
            code: Some(0),
            signal: None,
        },
    )
    .unwrap();
    assert!(!value.to_string().contains("PRIVATE_SENTINEL"));
    assert_eq!(
        value["signalingLatencies"]["operations"]["chat_ack"]["histogramMs"],
        json!({"1":1})
    );
    let absent = generator_verdict(
        &summary(),
        &Exit {
            code: Some(0),
            signal: None,
        },
    )
    .unwrap();
    assert!(absent.get("chat").is_none() && absent.get("signalingLatencies").is_none());
}

#[test]
fn all_accounts_must_renew_and_cross_their_original_expiry_without_duplicate_identity() {
    assert_eq!(
        authentication_verdict(&authentication(), &accounts()).unwrap(),
        json!({"passed":true,"accounts":30,"httpRefreshes":30,"socketRenewals":30,"originalExpiryCrossed":30})
    );
    for (field, value) in [
        ("passed", json!(false)),
        ("httpRefreshes", json!(0)),
        ("socketRenewals", json!(0)),
        ("originalExpiryCrossed", json!(false)),
        ("latestExpiresAt", json!(1800000000)),
        ("failures", json!(1)),
        ("originalExpiresAt", Value::Null),
        ("accountId", json!(accounts()[1])),
    ] {
        let mut invalid = authentication();
        invalid[0][field] = value;
        assert!(
            authentication_verdict(&invalid, &accounts()).is_err(),
            "{field}"
        );
    }
}

#[test]
fn retained_inventory_binds_every_identity_owner_and_content_byte() {
    let (rows, inventory) = retained();
    assert_eq!(
        retention_verdict(&rows, &inventory, &accounts()).unwrap(),
        json!({"passed":true,"messages":30,"accounts":30})
    );
    for (field, value) in [
        ("content", json!("changed")),
        ("senderAccountId", json!(accounts()[1])),
        ("clientMessageId", json!("foreign")),
        ("messageId", json!("foreign")),
    ] {
        let mut invalid = rows.clone();
        invalid[0][field] = value;
        assert!(
            retention_verdict(&invalid, &inventory, &accounts()).is_err(),
            "{field}"
        );
    }
    let mut duplicate = rows.clone();
    duplicate[1] = duplicate[0].clone();
    assert!(retention_verdict(&duplicate, &inventory, &accounts()).is_err());
    let mut duplicate = inventory.clone();
    duplicate[1] = duplicate[0].clone();
    assert!(retention_verdict(&rows, &duplicate, &accounts()).is_err());
}

#[test]
fn resource_headroom_and_coverage_remain_distinct_from_missing_observations() {
    let original = samples();
    let result = measurement_resources(&original, 0.0, 1200000.0).unwrap();
    assert_eq!(result["passed"], true);
    assert_eq!(result["server"]["cpuPercentOfOneCore"].as_f64(), Some(20.0));
    assert_eq!(result["database"]["p99Ms"].as_f64(), Some(0.5));
    for (field, value) in [
        ("simplestchat_media_worker_cpu{worker=\"0\"}", json!(0.8)),
        ("simplestchat_memory_saturated", json!(1)),
        ("simplestchat_participants_active", json!(29)),
        (
            "simplestchat_database_errors_total{kind=\"pool_timeout\"}",
            json!(1),
        ),
    ] {
        let mut invalid = original.clone();
        invalid[100]["metrics"][field] = value;
        assert_eq!(
            measurement_resources(&invalid, 0.0, 1200000.0).unwrap()["passed"],
            false,
            "{field}"
        );
    }
    for field in [
        "server",
        "generator",
        "host",
        "cgroup",
        "metrics",
        "database",
    ] {
        let mut invalid = original.clone();
        for row in &mut invalid {
            row[field] = Value::Null;
        }
        assert!(
            measurement_resources(&invalid, 0.0, 1200000.0).is_err(),
            "{field}"
        );
    }
    let mut changed = original.clone();
    changed[100]["server"]["processes"][1]["startTicks"] = json!(2000);
    assert!(measurement_resources(&changed, 0.0, 1200000.0).is_err());
    let mut private = original.clone();
    for row in &mut private {
        row["server"]["processes"][0]["PRIVATE_SENTINEL"] = json!("token");
    }
    assert!(
        !measurement_resources(&private, 0.0, 1200000.0)
            .unwrap()
            .to_string()
            .contains("PRIVATE_SENTINEL")
    );
}

#[test]
fn missing_or_invalid_metrics_database_host_and_cgroup_observations_are_not_zeroes() {
    assert!(parse_metrics("#empty\n").is_err());
    let metrics=parse_metrics("simplestchat_connections_active 30\nsimplestchat_connections_active 29\nsimplestchat_invalid 1e999\n").unwrap();
    assert_eq!(metrics["simplestchat_connections_active"], json!(29));
    assert!(!metrics.contains_key("simplestchat_invalid"));
    assert_eq!(
        parse_database_probe("{\"connections\":5,\"active\":1,\"waiting\":0}\nTime: 0.321 ms\n")
            .unwrap(),
        json!({"connections":5,"active":1,"waiting":0,"queryRoundTripMs":0.321})
    );
    assert!(parse_database_probe("{\"connections\":5}\n").is_err());
    assert_eq!(
        parse_cgroup(
            "usage_usec 500\nnr_periods 10\nnr_throttled 1\n",
            "200000 100000\n"
        )
        .unwrap(),
        json!({"periods":10,"throttledPeriods":1,"quotaCores":2})
    );
    assert!(parse_cgroup("usage_usec 500", "max 100000").is_err());
    assert_eq!(
        parse_host(
            "cpu 10 0 20 60 5 2 3 0 99 88\n",
            "MemAvailable: 500000 kB\n"
        )
        .unwrap(),
        json!({"total":100,"idle":65,"steal":0,"availableBytes":512000000})
    );
    assert!(parse_host("cpu 1 2 3\n", "MemAvailable: 500000 kB\n").is_err());
    assert!(parse_host("cpu 10 0 20 60 5 2 3 0\n", "MemAvailable: 0 kB\n").is_err());
}

#[test]
fn failure_categories_are_closed_and_native_text_has_an_explicit_utf16_boundary() {
    assert_eq!(
        failure_code(&anyhow::anyhow!("PRIVATE_SENTINEL password")),
        "invalid_policy_observation"
    );
    assert_eq!(
        failure_code(&anyhow::anyhow!("incomplete_authentication_cohort")),
        "incomplete_authentication_cohort"
    );
    let long = "x".repeat(450);
    assert_eq!(
        failures(Some(&json!([long]))).unwrap()[0]
            .as_str()
            .unwrap()
            .len(),
        400
    );
    let split = format!("{}😀", "x".repeat(399));
    assert_eq!(
        failure_code(&failures(Some(&json!([split]))).unwrap_err()),
        "invalid_failure_reasons"
    );
    // Retained chat failures have no reference per-entry truncation rule.
    let reasons: Vec<_> = (0..101).map(|_| "x".repeat(401)).collect();
    let chat = selected_chat(&json!({"failureReasons":reasons})).unwrap();
    assert_eq!(chat["failureReasons"].as_array().unwrap().len(), 101);
    assert_eq!(chat["failureReasons"][0].as_str().unwrap().len(), 401);
}

#[test]
fn every_selected_latency_integer_has_the_same_exact_javascript_boundary() {
    let safe = 9_007_199_254_740_991_u64;
    let stats = json!({"count":1,"minMs":1,"maxMs":1,"avgMs":1,
        "p50Ms":1,"p95Ms":1,"p99Ms":1,"histogramMs":{"1":1}});
    for field in [
        "count", "minMs", "maxMs", "avgMs", "p50Ms", "p95Ms", "p99Ms",
    ] {
        let mut value = stats.clone();
        value[field] = json!(safe);
        assert!(
            selected_latencies(&json!({"operations":{"chat_ack":value}})).is_ok(),
            "{field}"
        );
        for outside in [safe + 1, safe + 2] {
            value[field] = json!(outside);
            assert_eq!(
                failure_code(
                    &selected_latencies(&json!({"operations":{"chat_ack":value}})).unwrap_err()
                ),
                "invalid_signaling_latencies",
                "{field}"
            );
        }
    }
    for endpoint in [false, true] {
        let mut value = stats.clone();
        value["histogramMs"] = if endpoint {
            json!({safe.to_string():1})
        } else {
            json!({"1":safe})
        };
        assert!(selected_latencies(&json!({"operations":{"chat_ack":value}})).is_ok());
        value["histogramMs"] = if endpoint {
            json!({(safe+1).to_string():1})
        } else {
            json!({"1":safe+1})
        };
        assert_eq!(
            failure_code(
                &selected_latencies(&json!({"operations":{"chat_ack":value}})).unwrap_err()
            ),
            "invalid_signaling_latencies"
        );
    }
}
