//! Fixed public evidence. Credential values, private paths and child diagnostics
//! are never serialized. Source hashes describe this implementation, not CI trust.

use super::{config::Config, policy};
use crate::bench::{
    clock::Clock,
    process::{Exit, Retirement},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

pub(super) fn initial(config: &Config, clock: &dyn Clock) -> Value {
    json!({"schemaVersion":1,"purpose":"account-room-operating-point",
        "completed":false,"passed":false,"startedAt":clock.utc(),"revision":config.revision,
        "workload":{"clients":30,"rooms":1,"durationSeconds":1200,"rampSeconds":60,
            "warmupSeconds":30,"receiveMode":"balanced","chatIntervalMs":30000,
            "historyRetentionDays":7,"mediaWorkers":1},
        "setup":{"registrationsPerIpPerHour":100,"authenticationRequestsPerIpPerMinute":300,
            "sourceAddresses":30,"registration":"real HTTP requests",
            "room":"one retained, registered-only room"},
        "validityLimits":{"hostBusyShare":0.85,"hostStealShare":0.05,
            "sharedCgroupThrottledShare":0.05,"mediaWorkerCores":0.7,
            "generatorRuntimeLagMs":500,"resourceSamplingGapMs":1500},
        "limitations":[
            concat!("Co-located hosted Linux generator, server and PostgreSQL; ",
                "one operating point, not production maximum capacity."),
            concat!("Synthetic RTP forwarding, not thirty browser decoders or real devices, ",
                "WAN, TLS or TURN."),
            concat!("Database query probe excludes application pool acquisition; ",
                "chat acknowledgement latency includes persistence and queuing."),
            concat!("Media egress reports confirmed received RTP payload bytes; ",
                "wire overhead is an explicit estimate."),
            concat!("Setup uses the disposable helper registration/auth budgets; ",
                "room/media/default admission limits are retained."),
            concat!("One Rust owner records reserved server/generator group retirement; ",
                "no engine, root or separate-session closure is claimed.")
        ],"phase":"setup","failure":null})
}

pub(super) fn implementation() -> Value {
    let mut digest = Sha256::new();
    for (name, bytes) in [
        (
            "account_room.rs",
            include_bytes!("../account_room.rs").as_slice(),
        ),
        ("controller.rs", include_bytes!("controller.rs").as_slice()),
        ("config.rs", include_bytes!("config.rs").as_slice()),
        ("database.rs", include_bytes!("database.rs").as_slice()),
        ("http.rs", include_bytes!("http.rs").as_slice()),
        ("sampling.rs", include_bytes!("sampling.rs").as_slice()),
        ("policy.rs", include_bytes!("policy.rs").as_slice()),
        ("files.rs", include_bytes!("files.rs").as_slice()),
        ("measure.rs", include_bytes!("measure.rs").as_slice()),
        ("report.rs", include_bytes!("report.rs").as_slice()),
    ] {
        digest.update((name.len() as u32).to_be_bytes());
        digest.update(name.as_bytes());
        digest.update((bytes.len() as u64).to_be_bytes());
        digest.update(bytes);
    }
    json!({"implementation":"rust-owned-account-room-v1",
        "sourceScope":"embedded-controller-modules-u32-name-u64-bytes-sha256",
        "sourceSha256":hex::encode(digest.finalize()),
        "relationship":concat!("embedded source identity; ",
            "no authenticated build or complete input closure claim")})
}

pub(super) fn assess(result: anyhow::Result<Value>) -> Value {
    result.unwrap_or_else(|error| {
        if let Some(code) = super::database::failure_code(&error) {
            return json!({"passed":false,"failure":code});
        }
        let text = error.to_string();
        let code = match text.as_str() {
            "full_measurement_window_not_completed" => "full_measurement_window_not_completed",
            "retention_chat_ack_mismatch" => "retention_chat_ack_mismatch",
            "room_history_retention_not_enabled" => "room_history_retention_not_enabled",
            _ => policy::failure_code(&error),
        };
        json!({"passed":false,"failure":code})
    })
}

pub(super) fn cleanup(retirement: Option<&Retirement>, stopped: bool, started: bool) -> Value {
    json!({"started":started,
        "forced":if started {retirement.and_then(|v| v.leader_grace_expired)} else {Some(false)},
        "stopped":!started || (stopped && retirement
            .is_some_and(|v| v.group_cleanup_passed && v.streams_closed)),
        "scope":"reserved CLI group and owned streams"})
}

pub(super) fn shutdown(
    pid: Option<u32>,
    status: u8,
    exit: Option<&Exit>,
    retirement: Option<&Retirement>,
    passed: bool,
    clock: &dyn Clock,
) -> Value {
    json!({"schemaVersion":2,"serverPid":pid,"statusBeforeCleanup":status,
        "groupSignals":retirement.map(|v| json!({"termAttempted":v.term_attempted,
            "termSent":v.term_sent,"killAttempted":v.kill_attempted,"killSent":v.kill_sent})),
        "leaderTerminalBeforeCleanup":retirement.and_then(|v| v.leader_terminal_before_cleanup),
        "leaderGraceExpired":retirement.and_then(|v| v.leader_grace_expired),
        "waitStatus":exit,"groupCleanupPassed":retirement.map(|v| v.group_cleanup_passed),
        "streamsClosed":retirement.map(|v| v.streams_closed),
        "waitStatusInterpretation":concat!("actual reaped leader code/signal; ",
            "final group KILL is not leader escalation"),
        "scope":"owned reserved CLI group and streams; no separate-session or engine closure",
        "passed":passed,"finishedAt":clock.utc()})
}

pub(super) fn resources(mut value: Value, summary: &Value) -> Value {
    let lag = summary["runtimeLag"]["maxMsBySecond"]
        .as_array()
        .and_then(|values| values.get(90..1290));
    let covered = lag.is_some_and(|values| {
        values.len() == 1200
            && values
                .iter()
                .all(|v| v.as_f64().is_some_and(|v| v.is_finite() && v >= 0.0))
    });
    let maximum = lag.filter(|_| covered).map(|values| {
        values
            .iter()
            .filter_map(Value::as_f64)
            .fold(0.0_f64, f64::max)
    });
    value["generatorLagCoverage"] = covered.into();
    value["generatorMaxLagMs"] = maximum.map_or(Value::Null, Value::from);
    let passed = value["passed"] == true && covered && maximum.is_some_and(|v| v <= 500.0);
    value["passed"] = passed.into();
    if !passed {
        value["failure"] = "resource_validity_or_headroom_failed".into();
    }
    value
}
