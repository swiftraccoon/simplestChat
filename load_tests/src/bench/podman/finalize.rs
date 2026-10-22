//! Every owned finalizer is attempted. Failure evidence never becomes a passing row.

use super::{engine::MIB, report, step::Run};
use crate::bench::{artifacts, process::Sink};
use anyhow::Result;
use serde_json::{Value, json};
use std::time::Duration;

fn record(errors: &mut Vec<String>, label: &str, result: Result<()>) {
    if let Err(error) = result {
        errors.push(format!("{label}: {error:#}"));
    }
}

pub(super) async fn run(run: &mut Run<'_>) -> Vec<String> {
    let mut errors = Vec::new();
    for sampler in &mut run.samplers {
        record(&mut errors, "stop sampler", sampler.stop().await);
    }
    record(
        &mut errors,
        "collect generator",
        run.collect_generator().await,
    );
    record(&mut errors, "collect capture", run.collect_capture().await);
    let server = run.freeze(&run.server, 20).await;
    match server {
        Ok(Some((_, state))) => run.server_exit = Some(state),
        Ok(None) => {}
        Err(error) => errors.push(format!("stop server: {error:#}")),
    }
    for (name, file) in [
        (&run.generator, "generator.log"),
        (&run.server, "server.log"),
    ] {
        let result = async {
            let Some(id) = run.ownership.resolve(run.context.engine, name).await? else {
                return Ok(());
            };
            let output = run
                .context
                .engine
                .call(
                    &["logs".into(), id],
                    Duration::from_secs(30),
                    64 * MIB,
                    Sink::File(artifacts::private_file(&run.directory.join(file))?),
                    Sink::File(artifacts::private_file(
                        &run.directory.join(format!("{file}.stderr")),
                    )?),
                    true,
                )
                .await?;
            anyhow::ensure!(
                output.exit.code == Some(0) && output.exit.signal.is_none(),
                "Stopped container log transfer failed"
            );
            Ok(())
        }
        .await;
        record(&mut errors, "collect logs", result);
    }
    record(
        &mut errors,
        "server exit evidence",
        artifacts::json(&run.directory.join("server-exit.json"), &run.server_exit),
    );
    record(
        &mut errors,
        "generator exit evidence",
        artifacts::json(
            &run.directory.join("generator-exit.json"),
            &run.generator_exit,
        ),
    );
    let resources = run
        .data
        .lock()
        .map_err(|_| anyhow::anyhow!("Owned sampler state unavailable"))
        .and_then(|data| artifacts::json(&run.directory.join("resources.json"), &*data));
    record(&mut errors, "resource evidence", resources);
    // Removing one owned container must not suppress cleanup of the others.
    for name in [
        &run.generator,
        &run.capture,
        &format!("bench-netem-{}", run.id),
        &format!("bench-udp-{}", run.id),
        &run.server,
    ] {
        record(
            &mut errors,
            "remove owned container",
            run.ownership.remove(run.context.engine, name).await,
        );
    }
    if let Some(row) = &mut run.result {
        row["serverExit"] = match &run.server_exit {
            Some(state) if state.status == "exited" => json!({"code":state.code,"signal":null}),
            other => json!({"code":null,"signal":null,"error":serde_json::to_string(other).ok()}),
        };
        record(
            &mut errors,
            "complete row",
            report::completed(row, &run.context.engine.clock.utc()),
        );
        if !errors.is_empty() {
            row["cleanupErrors"] = json!(errors);
            row["cleanupComplete"] = Value::Bool(false);
            row["passed"] = Value::Bool(false);
        }
        record(
            &mut errors,
            "result evidence",
            artifacts::json(&run.directory.join("result.json"), row),
        );
    }
    errors
}
