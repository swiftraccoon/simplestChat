//! Independent cleanup and evidence attempts. One failed report never prevents
//! retiring another owned child, closing its log or retaining its exit status.

use super::step::{State, timeout_marker};
use crate::bench::{
    artifacts, options::Purpose, process::Exit, provenance, reports::Request, summary, verdict,
};
use anyhow::Result;
use serde_json::{Value, json};
use std::time::Duration;

fn record(errors: &mut Vec<&'static str>, label: &'static str, result: Result<()>) {
    if result.is_err() {
        errors.push(label);
    }
}

async fn stop(state: &mut State<'_>, role: &str, grace: Duration) -> Result<()> {
    let slot = match role {
        "server" => &mut state.server,
        "generator" => &mut state.generator,
        _ => &mut state.capture,
    };
    let Some(owned) = slot else {
        return Ok(());
    };
    let unobserved = owned.exit.is_none();
    let exit = owned.child.stop(grace).await?;
    owned.exit = Some(exit);
    owned.closed = true;
    if unobserved {
        match role {
            "server" => state.mark("server_exited"),
            "generator" => state.mark("generator_exited"),
            _ => {}
        }
    }
    Ok(())
}

fn append_issue(report: &mut Value, issue: &str) {
    report["coverage"]["complete"] = false.into();
    if let Some(issues) = report["coverage"]["issues"].as_array_mut()
        && !issues.iter().any(|value| value == issue)
    {
        issues.push(issue.into());
    }
}

async fn reports(state: &mut State<'_>, errors: &mut Vec<&'static str>) -> Result<()> {
    let options = state.context.options;
    if !options.full_diagnostics() {
        return Ok(());
    }
    anyhow::ensure!(
        state.server.as_ref().is_none_or(|owned| owned.closed)
            && state.generator.as_ref().is_none_or(|owned| owned.closed),
        "Stopped benchmark inputs unavailable"
    );
    let samples = state
        .sampler
        .as_ref()
        .map(|sampler| json!(sampler.samples))
        .unwrap_or_else(|| json!([]));
    let summary_value = artifacts::read_json(
        &state.directory.join("load_test_summary.json"),
        32 * 1024 * 1024,
    )
    .ok();
    let mut admitted = summary_value.as_ref().is_some_and(|value| {
        timeout_marker(&state.directory).is_ok_and(|timed_out| {
            summary::admits_diagnostics(
                value,
                options,
                &state.context.manifest.generator_sha256,
                timed_out,
            )
        })
    });
    let results = state.directory.join("load_test_results.json");
    // Preserve the owner's read admission before a selected reader can reopen
    // the path. A rejected/malformed result cannot be substituted by a later
    // read, even though the stopped-input reader also enforces its own bounds.
    admitted &= artifacts::read_json(&results, 32 * 1024 * 1024).is_ok();
    let mut stall_written = false;
    let mut stall_available = false;
    let stall = state
        .context
        .services
        .reports
        .read(Request {
            mode: "receiver-stall",
            path: &results,
            input: &samples,
            admitted,
            expected_participants: state.context.scenario.clients,
        })
        .await;
    match stall {
        Ok(value) => {
            stall_available = value["coverage"]["available"] == true;
            let result =
                artifacts::json(&state.directory.join("receiver-stall-report.json"), &value);
            stall_written = result.is_ok();
            record(errors, "receiver_stall_report_write_failed", result);
        }
        Err(_) => errors.push("receiver_stall_report_failed"),
    }
    let timeline = state.timeline.as_ref().unwrap();
    let log = state.directory.join("server.log");
    let mut lifecycle = state
        .context
        .services
        .reports
        .read(Request {
            mode: "lifecycle",
            path: &log,
            input: timeline,
            admitted: true,
            expected_participants: state.context.scenario.clients,
        })
        .await
        .unwrap_or_else(|_| {
            json!({"schemaVersion":1,"coverage":{
        "available":false,"complete":false,"issues":["lifecycle_report_failed"]}})
        });
    if !state.timeline_saved {
        append_issue(&mut lifecycle, "timeline_write_failed");
    }
    if artifacts::json(
        &state.directory.join("server-lifecycle-report.json"),
        &lifecycle,
    )
    .is_err()
    {
        append_issue(&mut lifecycle, "lifecycle_report_write_failed");
    }
    let mut media = state
        .context
        .services
        .reports
        .read(Request {
            mode: "media",
            path: &results,
            input: &samples,
            admitted,
            expected_participants: state.context.scenario.clients,
        })
        .await
        .unwrap_or_else(|_| {
            json!({"schemaVersion":1,"coverage":{
        "available":false,"complete":false,"issues":["media_report_failed"]},
        "samples":[],"consumers":[]})
        });
    if artifacts::json(&state.directory.join("server-media-report.json"), &media).is_err() {
        append_issue(&mut media, "media_report_write_failed");
    }
    let recorder = state.directory.join("server-diagnostics.jsonl");
    let diagnostics = state
        .context
        .services
        .reports
        .read(Request {
            mode: "diagnostics",
            path: &recorder,
            input: &Value::Null,
            admitted: true,
            expected_participants: state.context.scenario.clients,
        })
        .await;
    let diagnostics = match diagnostics {
        Ok(value) => {
            let saved = artifacts::json(
                &state.directory.join("server-diagnostics-report.json"),
                &value,
            );
            record(errors, "diagnostic_report_write_failed", saved);
            Some(value)
        }
        Err(_) => {
            errors.push("diagnostic_report_failed");
            None
        }
    };
    if let Some(row) = &mut state.row {
        for (field, report, file) in [
            (
                "diagnosticCoverage",
                diagnostics.as_ref(),
                "server-diagnostics-report.json",
            ),
            (
                "mediaDiagnosticCoverage",
                Some(&media),
                "server-media-report.json",
            ),
            (
                "lifecycleDiagnosticCoverage",
                Some(&lifecycle),
                "server-lifecycle-report.json",
            ),
        ] {
            let mut coverage = report
                .map(|report| report["coverage"].clone())
                .unwrap_or_else(|| json!({"available":false,"complete":false}));
            coverage["requested"] = true.into();
            coverage["report"] = file.into();
            row[field] = coverage;
        }
        row["receiverStallReport"] = json!({"requested":true,"written":stall_written,
            "available":stall_available});
    }
    Ok(())
}

pub(super) async fn run(state: &mut State<'_>) -> Result<()> {
    let mut errors = Vec::new();
    let services = state.context.services;
    // Observability is best effort and never replaces the primary failure.
    if state
        .server
        .as_ref()
        .is_some_and(|owned| owned.exit.is_none())
        && let Ok(last) = services.metrics.fetch(&state.origin, &state.token).await
    {
        let _ = artifacts::new_file(
            &state.directory.join("metrics-stop.txt"),
            last.raw.as_bytes(),
        );
    }
    record(
        &mut errors,
        "generator_stop_failed",
        stop(state, "generator", Duration::from_secs(5)).await,
    );
    record(
        &mut errors,
        "capture_stop_failed",
        stop(state, "capture", Duration::from_secs(5)).await,
    );
    if state
        .server
        .as_ref()
        .is_some_and(|owned| owned.exit.is_none())
    {
        state.mark("server_stop_requested");
    }
    record(
        &mut errors,
        "server_stop_failed",
        stop(state, "server", Duration::from_secs(20)).await,
    );
    let now = services.clock.elapsed_ms() - state.start;
    let alive = state.alive();
    if let Some(sample) = &mut state.pending_media
        && sample["status"].is_null()
    {
        state.sampler.as_ref().unwrap().complete(
            sample,
            json!({"status":"error","issue":"media_request_failed"}),
            now,
            alive.clone(),
        );
    }
    if let Some(sampler) = &mut state.sampler {
        if let Some(sample) = state.pending_media.take() {
            sampler.save(sample);
        }
        sampler.finish(now, alive);
    }
    let inputs_closed = state.server.as_ref().is_none_or(|owned| owned.closed)
        && state.generator.as_ref().is_none_or(|owned| owned.closed);
    if let Some(timeline) = &mut state.timeline {
        if timeline["workload"].is_null()
            && inputs_closed
            && let Ok(value) = artifacts::read_json(
                &state.directory.join("load_test_summary.json"),
                8 * 1024 * 1024,
            )
        {
            timeline["workload"] = summary::lifecycle_workload(
                &value,
                state.context.options,
                &state.context.manifest.generator_sha256,
            )
            .unwrap_or(Value::Null);
        }
        let result = artifacts::json(&state.directory.join("lifecycle-timeline.json"), timeline);
        state.timeline_saved = result.is_ok();
        record(&mut errors, "timeline_write_failed", result);
    }
    for (name, exit) in [
        (
            "server-exit.json",
            state.server.as_ref().and_then(|owned| owned.exit.as_ref()),
        ),
        (
            "generator-exit.json",
            state
                .generator
                .as_ref()
                .and_then(|owned| owned.exit.as_ref()),
        ),
    ] {
        record(
            &mut errors,
            "exit_evidence_write_failed",
            artifacts::json(&state.directory.join(name), &exit),
        );
    }
    let report_result = reports(state, &mut errors).await;
    record(&mut errors, "report_collection_failed", report_result);
    // Recheck the selected generator even when diagnostics are disabled. A
    // changed path must not retain a passing performance row.
    record(
        &mut errors,
        "generator_identity_changed",
        provenance::verify_executable(
            &state.context.options.generator,
            &state.context.manifest.generator_sha256,
            "Generator",
        ),
    );
    if let Some(row) = &mut state.row {
        row["serverExit"] =
            serde_json::to_value(state.server.as_ref().and_then(|owned| owned.exit.as_ref()))?;
        if state.context.options.purpose == Purpose::Diagnostic {
            if !state.context.options.full_diagnostics() {
                row["diagnosticCoverage"] = json!({"requested":false,"complete":null,
                    "scope":"not_requested"});
                for key in ["mediaDiagnosticCoverage", "lifecycleDiagnosticCoverage"] {
                    row[key] = json!({"requested":false,"complete":null});
                }
                row["receiverStallReport"] = json!({"requested":false,"written":null,
                    "available":null});
            }
            let status = verdict::diagnostic(std::slice::from_ref(row));
            row["serverShutdownPassed"] = status.server_shutdown_passed.into();
            row["passed"] = status.passed.into();
        } else {
            let status = serde_json::to_value(verdict::performance(std::slice::from_ref(row)))?;
            row.as_object_mut()
                .unwrap()
                .extend(status.as_object().unwrap().clone());
            row["completedAt"] = services.clock.utc().into();
        }
    }
    if let Some(capture) = &state.capture {
        record(
            &mut errors,
            "capture_exit_write_failed",
            artifacts::json(&state.directory.join("capture-exit.json"), &capture.exit),
        );
        if !capture.exit.as_ref().is_some_and(Exit::success) {
            errors.push("capture_exit_failed");
        }
    }
    record(
        &mut errors,
        "resources_write_failed",
        artifacts::json(&state.directory.join("resources.json"), &state.samples),
    );
    // Publish success only after every owned evidence attempt has finished.
    // A later resource/capture write failure cannot leave a passing result.
    if let Some(row) = &mut state.row {
        if !errors.is_empty() {
            row["passed"] = false.into();
            row["finalizerIssues"] = json!(errors);
        }
        record(
            &mut errors,
            "result_write_failed",
            artifacts::json(&state.directory.join("result.json"), row),
        );
    }
    if !errors.is_empty() {
        let _ = artifacts::json(&state.directory.join("finalizer-issues.json"), &errors);
        anyhow::bail!("Benchmark finalization failed");
    }
    Ok(())
}
