//! Own the full lifetime of one pair of isolated containers and its evidence.

use super::{
    artifacts, commands, conclude,
    context::Context,
    files, json, parsers,
    process::{Limits, Request},
    process_io::Destination,
    types::{Plan, StepResult},
    watch,
};
use crate::capacity::format::general;
use anyhow::{Result, ensure};
use std::path::Path;
use std::time::Duration;

async fn transfer(context: &Context<'_>, generator: &str, directory: &Path) -> Result<()> {
    let archive = directory.join("generator-results.tar");
    let output = files::private_file(&archive)?;
    let errors = files::private_file(&directory.join("collection.stderr"))?;
    let result = context
        .engine
        .call(
            &["cp".into(), format!("{generator}:/results/."), "-".into()],
            Request {
                limits: Limits {
                    timeout: Duration::from_secs(120),
                    stdout: artifacts::MAX_ARCHIVE,
                    stderr: 2 * 1024 * 1024,
                },
                stdout: Destination::file(output),
                stderr: Destination::file(errors),
                cleanup: true,
                ..Request::default()
            },
        )
        .await?;
    ensure!(result.status == 0, "result_transfer_failed");
    artifacts::finish_collection(&archive, directory)?;
    Ok(())
}

async fn collect(
    context: &Context<'_>,
    names: (Option<&str>, Option<&str>),
    directory: &Path,
) -> Result<()> {
    let mut failure = match names.1 {
        Some(generator) => transfer(context, generator, directory).await.err(),
        None => Some(anyhow::anyhow!("owned generator is unavailable")),
    };
    for (name, file) in [(names.1, "generator.log"), (names.0, "server.log")] {
        let Some(name) = name else {
            continue;
        };
        let saved: Result<()> = async {
            let log = context.engine.logs(name).await?;
            let begin = log
                .char_indices()
                .rev()
                .nth(199_999)
                .map_or(0, |(index, _)| index);
            artifacts::new_file(&directory.join(file), &log.as_bytes()[begin..])
        }
        .await;
        if failure.is_none() {
            failure = saved.err();
        }
    }
    failure.map_or(Ok(()), Err)
}

async fn freeze(context: &Context<'_>, name: &str) -> Result<Option<String>> {
    let Some(id) = context.ownership.resolve(context.engine, name).await? else {
        return Ok(None);
    };
    let output = context
        .engine
        .call(
            &["stop".into(), "--time".into(), "5".into(), id.clone()],
            Request {
                cleanup: true,
                limits: Limits {
                    timeout: Duration::from_secs(30),
                    ..Limits::default()
                },
                ..Request::default()
            },
        )
        .await?;
    ensure!(output.status == 0, "owned container stop failed");
    Ok(Some(id))
}

async fn finalize(
    context: &Context<'_>,
    names: (&str, &str),
    directory: &Path,
    result: &mut StepResult,
) {
    // Freeze both verified identities before reading their archive/logs. A
    // stopped generator cannot mutate the files during the evidence transfer.
    let mut ids = [None, None];
    for (index, name) in [names.0, names.1].into_iter().enumerate() {
        match freeze(context, name).await {
            Ok(id) => ids[index] = id,
            Err(error) if result.error.is_empty() => {
                result.error = format!("owned container stop failed: {error}");
            }
            Err(_) => {}
        }
    }
    let collected: Result<()> = async {
        collect(context, (ids[0].as_deref(), ids[1].as_deref()), directory).await?;
        if let Some(server) = &ids[0]
            && context.engine.oom_killed(server).await?
        {
            result.server_exit = "the server ran out of memory".into();
        }
        if let Some(generator) = &ids[1] {
            result.generator_out_of_memory = context.engine.oom_killed(generator).await?;
        }
        Ok(())
    }
    .await;
    if collected.is_err() && result.error.is_empty() {
        result.error = "generator evidence collection failed: CapacityError".into();
    }
    for name in [names.1, names.0] {
        if let Err(error) = context.ownership.remove(context.engine, name).await
            && result.error.is_empty()
        {
            result.error = format!("owned container cleanup failed: {error}");
        }
    }
}

pub(super) async fn run(context: &Context<'_>, plan: Plan, index: usize) -> Result<StepResult> {
    let name = format!("{}-{index}", context.run_id);
    let server = format!("capacity-server-{name}");
    let generator = format!("capacity-gen-{name}");
    let token = hex::encode(rand::random::<[u8; 24]>());
    let directory = context
        .output
        .join(format!("{index:02}-{}-{}", plan.workload, plan.size));
    files::directory(&directory)?;
    let ports = (0..context.shape.workers)
        .map(|worker| 40000 + worker)
        .collect::<Vec<_>>();
    let mut result = StepResult::new(plan, context.browser, context.clock.utc());
    let mut marks = (None, None, None);
    let executed: Result<()> = async {
        context
            .ownership
            .remember(&server, context.server_image, "")?;
        let id = context
            .engine
            .run(&commands::server(context, &server, &token), 30, true)
            .await?;
        context
            .ownership
            .remember(&server, context.server_image, id.trim())?;
        let server_id = id.trim().to_owned();
        watch::wait_ready(context, &server_id).await?;
        let before = parsers::udp(
            &context.engine.read(&server_id, "/proc/net/snmp").await?,
            &context.engine.read(&server_id, "/proc/net/udp").await?,
            &ports,
        )?;
        let limit = context
            .engine
            .read(&server_id, "/sys/fs/cgroup/memory.max")
            .await?;
        let limit = limit.trim();
        result.memory_limit_bytes =
            if !limit.is_empty() && limit.bytes().all(|byte| byte.is_ascii_digit()) {
                limit.parse()?
            } else {
                0
            };
        context
            .ownership
            .remember(&generator, context.generator_image, "")?;
        let id = context
            .engine
            .run(
                &commands::generator(context, plan, &generator, &server_id),
                30,
                true,
            )
            .await?;
        context
            .ownership
            .remember(&generator, context.generator_image, id.trim())?;
        let generator_id = id.trim().to_owned();
        watch::monitor(
            context,
            plan,
            (&server_id, &generator_id),
            &token,
            &mut result,
            &mut marks,
        )
        .await?;
        if context.engine.running(&server_id).await? {
            let after = parsers::udp(
                &context.engine.read(&server_id, "/proc/net/snmp").await?,
                &context.engine.read(&server_id, "/proc/net/udp").await?,
                &ports,
            )?;
            result.namespace_datagrams = after.in_datagrams - before.in_datagrams;
            result.socket_drops = after.socket_drops - before.socket_drops;
            result.rcvbuf_errors = after.rcvbuf_errors - before.rcvbuf_errors;
            let metrics = watch::scrape(context.engine, &server_id, &token).await;
            let refused = metrics
                .get("simplestchat_joins_refused_saturated_total")
                .copied()
                .unwrap_or(0.0);
            ensure!(
                refused.is_finite() && refused >= i64::MIN as f64 && refused < -(i64::MIN as f64),
                "refused joins counter is outside the integer range"
            );
            result.refused_joins = refused as i64;
            let metrics = metrics
                .iter()
                .map(|(key, value)| format!("{key} {}", general(*value)))
                .collect::<Vec<_>>()
                .join("\n");
            artifacts::new_file(&directory.join("metrics.txt"), metrics.as_bytes())?;
        } else if result.server_exit.is_empty() {
            result.server_exit = "the server stopped during the run".into();
        }
        Ok(())
    }
    .await;
    if let Err(error) = executed {
        result.error = error.to_string();
    }
    finalize(context, (&server, &generator), &directory, &mut result).await;
    conclude::conclude(
        &mut result,
        marks,
        &directory.join("generator/load_test_summary.json"),
    );
    artifacts::new_file(&directory.join("step.json"), &json::bytes(&result)?)?;
    Ok(result)
}
