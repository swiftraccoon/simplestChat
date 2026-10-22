//! The generator's sampled window and independent stopped-artifact assessments.

use super::{
    config::{Config, GENERATOR_MS},
    controller::{Services, check, fence},
    database::Database,
    files::{Directory, Record},
    policy, report,
};
use crate::bench::process::{Child, Exit};
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::time::Duration;

pub(super) struct Window {
    pub origin: f64,
    pub wall: i64,
    pub rows: Vec<Value>,
}
impl Window {
    pub fn new(services: &Services<'_>) -> Result<Self> {
        let wall = chrono::DateTime::parse_from_rfc3339(&services.clock.utc())?.timestamp_millis();
        Ok(Self {
            origin: services.clock.elapsed_ms(),
            wall,
            rows: Vec::new(),
        })
    }
}

pub(super) async fn run(
    config: &Config,
    services: &Services<'_>,
    database: &Database<'_>,
    server: &mut dyn Child,
    generator: &mut dyn Child,
    samples: &mut Record,
    window: &mut Window,
) -> Result<Exit> {
    let mut tick = 0;
    loop {
        check(config, services).await?;
        if let Some(exit) = generator.poll().await? {
            // The active deadline also applies when terminal acceptance wins a timer race.
            if exit.success() {
                fence(config, services)?;
            }
            return Ok(exit);
        }
        super::controller::running(server.poll().await?)?;
        ensure!(
            services.clock.elapsed_ms() - window.origin < GENERATOR_MS,
            "generator_deadline"
        );
        let elapsed = services.clock.elapsed_ms() - window.origin;
        let mut row = services
            .observations
            .sample(server.pid(), generator.pid())
            .await;
        check(config, services).await?;
        ensure!(row.is_object(), "invalid_resource_observation");
        row["elapsedMs"] = elapsed.into();
        let expected = tick % 10 == 0;
        row["metricsExpected"] = expected.into();
        if expected {
            let (metrics, db) = tokio::join!(
                services.http.metrics(),
                database.query(super::database::PROBE, true)
            );
            if db.as_ref().err().is_some_and(super::database::fatal_probe) {
                return Err(db.expect_err("observed probe failure"));
            }
            check(config, services).await?;
            row["metrics"] = metrics
                .ok()
                .and_then(|v| policy::parse_metrics(&v).ok())
                .map_or(Value::Null, Value::Object);
            row["database"] = db.unwrap_or(Value::Null);
        }
        check(config, services).await?;
        ensure!(window.rows.len() < 5000, "resource_sample_limit");
        let mut bytes = serde_json::to_vec(&row)?;
        ensure!(bytes.len() <= 65_536, "resource_sample_limit");
        bytes.push(b'\n');
        samples.append(&bytes)?;
        window.rows.push(row);
        tick += 1;
        tokio::select! {
            biased;
            code = services.host.cancelled() => {
                return Err(super::controller::Interrupted(code).into());
            },
            () = services.clock.sleep(Duration::from_millis(500)) => {},
        }
    }
}

pub(super) struct Cohort<'a> {
    pub accounts: &'a [String],
    pub room: &'a str,
}

pub(super) async fn assess(
    services: &Services<'_>,
    database: &Database<'_>,
    directory: &Directory,
    window: &Window,
    cohort: Cohort<'_>,
    exit: &Exit,
    report_value: &mut Value,
) -> Result<Option<u8>> {
    let Cohort { accounts, room } = cohort;
    let summary: Value =
        serde_json::from_slice(&directory.read("load_test_summary.json", 8 * 1024 * 1024)?)?;
    // A missing secondary artifact cannot erase independent completed-run
    // assessments. It still refuses a successful six-artifact result.
    let results_valid = directory
        .read("load_test_results.json", 32 * 1024 * 1024)
        .ok()
        .is_some_and(|bytes| serde_json::from_slice::<Value>(&bytes).is_ok());
    report_value["generator"] = report::assess(policy::generator_verdict(&summary, exit));
    report_value["completed"] = report_value["generator"]["completed"].clone();
    report_value["phase"] = "authentication".into();
    report_value["authentication"] = report::assess(policy::authentication_verdict(
        &summary["authentication"],
        accounts,
    ));
    report_value["phase"] = "resource_validation".into();
    report_value["resources"] = report::assess((|| -> Result<Value> {
        ensure!(
            report_value["generator"]["fullWindow"] == true,
            "full_measurement_window_not_completed"
        );
        let started = summary["run"]["startedAt"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("invalid_measurement_anchor"))?;
        let start = (chrono::DateTime::parse_from_rfc3339(started)?.timestamp_millis()
            - window.wall) as f64
            + 90_000.0;
        Ok(report::resources(
            policy::measurement_resources(&window.rows, start, start + 1_200_000.0)?,
            &summary,
        ))
    })());
    report_value["phase"] = "retention_validation".into();
    let retention = async {
        let inventory: Value = directory
            .read("chat_inventory.json", 8 * 1024 * 1024)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .ok_or_else(|| anyhow::anyhow!("evidence_validation_failed"))?;
        let rows = database.retention(room).await?;
        let value = policy::retention_verdict(&rows, &inventory, accounts)?;
        ensure!(
            value["messages"] == summary["chat"]["acknowledgements"]
                && summary["chat"]["passed"] == true,
            "retention_chat_ack_mismatch"
        );
        ensure!(
            database.retention_days(room).await? == 7,
            "room_history_retention_not_enabled"
        );
        Ok(value)
    }
    .await;
    let primary = retention
        .as_ref()
        .err()
        .and_then(super::controller::known_status);
    report_value["retention"] = report::assess(retention);
    let bytes = summary["measurement"]["bytesReceived"].as_f64();
    let packets = summary["measurement"]["packetsReceived"].as_f64();
    report_value["media"] = json!({"payloadBytesReceived":bytes,"packetsReceived":packets,
        "receivedPayloadMbps":bytes.map(|v| v*8.0/1_200_000_000.0),
        "estimatedIpv4MediaMbps":bytes.zip(packets).map(|(b,p)|(b+p*62.0)*8.0/1_200_000_000.0)});
    let verdicts = ["generator", "authentication", "resources", "retention"];
    report_value["passed"] = (results_valid
        && report_value["completed"] == true
        && verdicts
            .iter()
            .all(|key| report_value[*key]["passed"] == true))
    .into();
    report_value["failure"] = verdicts
        .iter()
        .find_map(|key| {
            (report_value[*key]["passed"] != true).then(|| report_value[*key]["failure"].clone())
        })
        .unwrap_or(Value::Null);
    if !results_valid && report_value["failure"].is_null() {
        report_value["failure"] = "generator_results_unavailable".into();
    }
    report_value["phase"] = "completed".into();
    ensure!(
        services.clock.elapsed_ms().is_finite(),
        "invalid_measurement_clock"
    );
    Ok(primary)
}
