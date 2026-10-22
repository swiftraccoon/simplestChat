//! Wire compatibility for extracted evidence, independent of any live workload.

use super::*;
use serde_json::{Value, json};

#[test]
fn extracted_measurements_keep_original_field_order_names_and_numeric_types() {
    let latency = LatencyStats {
        count: 3,
        min_ms: 1,
        max_ms: 50,
        avg_ms: 17,
        p50_ms: 1,
        p95_ms: 50,
        p99_ms: 50,
        histogram_ms: [(1, 2), (50, 1)].into(),
    };
    let bytes = serde_json::to_string(&latency).unwrap();
    assert_eq!(
        bytes,
        concat!(
            "{\"count\":3,\"minMs\":1,\"maxMs\":50,\"avgMs\":17,\"p50Ms\":1,",
            "\"p95Ms\":50,\"p99Ms\":50,\"histogramMs\":{\"1\":2,\"50\":1}}"
        )
    );
    let restored: LatencyStats = serde_json::from_str(&bytes).unwrap();
    assert_eq!(serde_json::to_string(&restored).unwrap(), bytes);

    let measurement = MeasurementMetrics {
        duration_ms: 20_000,
        packets_queued: 100,
        packets_received: 90,
        bytes_queued: 1000,
        bytes_received: 900,
    };
    let bytes = serde_json::to_string(&measurement).unwrap();
    assert_eq!(
        bytes,
        concat!(
            "{\"durationMs\":20000,\"packetsQueued\":100,\"packetsReceived\":90,",
            "\"bytesQueued\":1000,\"bytesReceived\":900}"
        )
    );
    let restored: MeasurementMetrics = serde_json::from_str(&bytes).unwrap();
    assert_eq!(serde_json::to_string(&restored).unwrap(), bytes);
}

#[test]
fn defaults_remain_zeroed_values_without_implicit_missing_field_acceptance() {
    assert_eq!(
        serde_json::to_value(LatencyStats::default()).unwrap(),
        json!({
            "count":0,"minMs":0,"maxMs":0,"avgMs":0,"p50Ms":0,"p95Ms":0,"p99Ms":0,
            "histogramMs":{}
        })
    );
    assert_eq!(
        serde_json::to_value(MeasurementMetrics::default()).unwrap(),
        json!({
            "durationMs":0,"packetsQueued":0,"packetsReceived":0,"bytesQueued":0,"bytesReceived":0
        })
    );
    assert!(serde_json::from_value::<LatencyStats>(json!({})).is_err());
    assert!(serde_json::from_value::<MeasurementMetrics>(json!({})).is_err());
    let mut measurement = serde_json::to_value(MeasurementMetrics::default()).unwrap();
    for invalid in [json!(-1), json!(0.5), json!(true), json!("0")] {
        measurement["packetsReceived"] = invalid;
        assert!(serde_json::from_value::<MeasurementMetrics>(measurement.clone()).is_err());
    }
}

#[test]
fn realized_subscription_edges_keep_the_existing_private_report_wire_shape() {
    let edge = PlannedSubscriptionEdge {
        publisher_client_id: "publisher".into(),
        producer_id: "producer".into(),
        consumer_id: "consumer".into(),
        is_audio: false,
    };
    let bytes = serde_json::to_string(&edge).unwrap();
    assert_eq!(
        bytes,
        concat!(
            "{\"publisherClientId\":\"publisher\",\"producerId\":\"producer\",",
            "\"consumerId\":\"consumer\",\"isAudio\":false}"
        )
    );
    let restored: PlannedSubscriptionEdge = serde_json::from_str(&bytes).unwrap();
    assert_eq!(serde_json::to_string(&restored).unwrap(), bytes);
    assert!(serde_json::from_value::<PlannedSubscriptionEdge>(json!({})).is_err());
}

#[test]
fn typed_run_metadata_matches_both_original_json_builders_byte_for_byte() {
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Configuration {
        num_clients: usize,
        publish_ratio: f64,
        #[serde(skip)]
        _private: &'static str,
    }
    let configuration = Configuration {
        num_clients: 12,
        publish_ratio: 0.5,
        _private: "private runtime state stays excluded",
    };
    let provenance = json!({"generatorRevision":"revision","logicalCpus":4});
    let started_at = "2026-10-21T10:00:00+00:00";
    let finished_at = "2026-10-21T10:01:00+00:00";
    for (completed, passed, failures) in [
        (true, true, Vec::new()),
        (
            false,
            false,
            vec!["Hard deadline exceeded; detailed metrics unavailable".to_owned()],
        ),
    ] {
        let before = json!({"run":{
            "completed":completed,"passed":passed,"failureReasons":failures,
            "startedAt":started_at,"finishedAt":finished_at,
            "configuration":configuration,"provenance":provenance
        }});
        let run = RunMetadata {
            completed,
            passed,
            failure_reasons: failures,
            started_at: started_at.into(),
            finished_at: finished_at.into(),
            configuration: &configuration,
            provenance: &provenance,
        };
        let after = json!({"run":run});
        assert_eq!(
            serde_json::to_vec_pretty(&after).unwrap(),
            serde_json::to_vec_pretty(&before).unwrap()
        );
        assert!(!serde_json::to_string(&after).unwrap().contains("private"));
        let restored: RunMetadata<Value, Value> =
            serde_json::from_value(after["run"].clone()).unwrap();
        assert_eq!(serde_json::to_value(restored).unwrap(), before["run"]);
    }
    assert!(serde_json::from_value::<RunMetadata<Value, Value>>(json!({})).is_err());
}
