//! Interpret bounded generator evidence without treating incomplete windows as passes.

use super::{
    artifacts, parsers,
    types::StepResult,
    watch::{Marks, apply_marks},
};
use crate::capacity::json::{self, Json};
use anyhow::{Result, bail};
use std::path::Path;

fn count(value: &Json, name: &str) -> Result<i64> {
    value.get(name).map_or(Ok(0), |value| value.integer(name))
}
fn number(value: &Json, name: &str) -> Result<f64> {
    value.get(name).map_or(Ok(0.0), |value| value.number(name))
}

pub(super) fn read_summary(result: &mut StepResult, summary: &Json) -> Result<()> {
    summary.object("summary")?;
    let run = &summary["run"];
    run.object("run")?;
    result.generator_passed = run["passed"].is_true();
    let failures = run
        .get("failureReasons")
        .map_or(Ok(&[][..]), |value| value.array("failureReasons"))?;
    result.generator_failures = failures.iter().take(5).map(Json::display).collect();
    result.generator_failure_count = failures.len() as i64;
    result.generator_errors = count(summary, "totalErrors")?;
    result.failed_connections = count(summary, "failedConnections")?;
    result.failed_consumers = count(summary, "failedConsumers")?;
    result.validated_consumers = count(summary, "validatedConsumers")?;
    let Some(measurement) = summary.get("measurement") else {
        return Ok(());
    };
    if summary.get("totalDatagramsSent").is_none() {
        bail!("the generator does not count its datagrams; rebuild the load-test image");
    }
    result.sent_datagrams = count(summary, "totalDatagramsSent")?;
    measurement.object("measurement")?;
    result.received_packets = count(measurement, "packetsReceived")?;
    result.received_payload_bytes = count(measurement, "bytesReceived")?;
    result.measurement_seconds = number(measurement, "durationMs")? / 1000.0;
    if let Some(value @ Json::Object(_)) = summary.get("receiveMediaReady") {
        result.receive_ready_p99_ms = number(value, "p99Ms")?;
    }
    if let Some(value @ Json::Object(_)) = summary.get("videoStart") {
        result.video_start_p99_ms = number(value, "p99Ms")?;
    }
    Ok(())
}

pub(super) fn read_clients(result: &mut StepResult, clients: &Json) -> Result<()> {
    for client in clients.array("clients")? {
        client.object("client")?;
        if let Some(bitrate) = client
            .get("lastAvailableBitrate")
            .filter(|value| !matches!(value, Json::Null))
            && bitrate.number("bitrate")? >= 0.95 * 3_000_000.0
        {
            result.viewers_at_cap += 1;
        }
        let deliveries = client
            .get("consumerDelivery")
            .map_or(Ok(&[][..]), |value| value.array("consumerDelivery"))?;
        for consumer in deliveries {
            consumer.object("consumer")?;
            if matches!(consumer["passed"], Json::Bool(false))
                && matches!(consumer["isAudio"], Json::Bool(false))
            {
                result.failed_video_consumers += 1;
            }
        }
    }
    Ok(())
}

pub(super) fn conclude(result: &mut StepResult, marks: Marks, summary: &Path) {
    let (setup, start, end) = marks;
    if let (Some(start), Some(end)) = (start, end) {
        apply_marks(result, start, end);
    }
    if let (Some(setup), Some(start)) = (setup, start) {
        result.generator_setup_throttled = parsers::throttled(setup.generator, start.generator);
    }
    if !summary.exists() {
        if result.error.is_empty() && !result.generator_out_of_memory {
            result.error = "the generator wrote no summary".into();
        }
        return;
    }
    let read: Result<()> = (|| {
        let summary_value = json::parse(&artifacts::read_regular(summary, 8 * 1024 * 1024)?)?;
        read_summary(result, &summary_value)?;
        let clients = summary.with_file_name("load_test_results.json");
        if clients.try_exists()? {
            read_clients(
                result,
                &json::parse(&artifacts::read_regular(&clients, artifacts::MAX_RESULT)?)?,
            )?;
        }
        Ok(())
    })();
    if let Err(error) = read {
        if result.error.is_empty() {
            result.error = format!("the generator's summary is unusable: {error}");
        }
        return;
    }
    if !result.error.is_empty() || !result.server_exit.is_empty() || result.generator_out_of_memory
    {
        return;
    }
    if end.is_none() {
        result.error =
            "no CPU readings cover the window, so the generator's throttling is unknown".into();
    } else if result.load_scrapes < 2 {
        result.error = concat!(
            "the server's per-worker loads were not readable through the window, ",
            "so its busiest worker is unknown"
        )
        .into();
    }
}
