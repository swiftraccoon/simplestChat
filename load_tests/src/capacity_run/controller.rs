//! Compose one measured calibration; injected engines and clocks keep tests offline.

use super::{
    artifacts,
    clock::Clock,
    context::Context,
    engine::Engine,
    files, host, json, judge,
    options::Options,
    ownership::Ownership,
    process::Cancellation,
    report, search, step,
    types::{Limits, Search, StepResult, Trial, Verdict, Workload},
};
use crate::capacity::format::general;
use anyhow::{Result, ensure};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

struct Logger<'a> {
    file: File,
    clock: &'a dyn Clock,
}
impl Logger<'_> {
    fn line(&mut self, message: &str) -> Result<()> {
        let utc = self.clock.utc();
        let time = utc.get(11..19).unwrap_or("00:00:00");
        let line = format!("{time} {message}\n");
        self.file.write_all(line.as_bytes())?;
        self.file.flush()?;
        std::io::stdout().lock().write_all(line.as_bytes())?;
        Ok(())
    }
}

fn memory(host: i64, share: f64, maximum: i64) -> String {
    format!("{}m", ((host as f64 * share) as i64).min(maximum).max(512))
}

async fn workload(
    context: &Context<'_>,
    search: Search,
    limits: Limits,
    timing: (i64, i64),
    first_index: usize,
    logger: &mut Logger<'_>,
) -> Result<Vec<(StepResult, Verdict)>> {
    let mut history: Vec<(StepResult, Verdict)> = Vec::new();
    loop {
        if context.cancellation.code() != 0 {
            break;
        }
        let trials = history
            .iter()
            .map(|(step, verdict)| Trial::from_step(step, verdict))
            .collect::<Vec<_>>();
        let Some(size) = search::next(search, &trials, limits, context.shape.generator_cpus) else {
            break;
        };
        ensure!(
            (1..=10_000_000).contains(&size),
            "projected workload is outside the supported range"
        );
        let plan = super::types::Plan {
            workload: search.workload,
            size,
            meeting_size: search.meeting_size,
            warmup_seconds: timing.0,
            duration_seconds: timing.1,
            workers: context.shape.workers,
        };
        logger.line(&format!(
            "{} at {size} ({} clients in {} rooms)",
            search.workload,
            plan.clients(),
            plan.rooms()
        ))?;
        let result = step::run(context, plan, first_index + history.len()).await?;
        let verdict = judge::judge(&result, limits);
        let outcome = if verdict.passed {
            "pass"
        } else if verdict.valid {
            "fail"
        } else {
            "invalid"
        };
        let detail = if verdict.reasons.is_empty() {
            String::new()
        } else {
            format!(": {}", verdict.reasons.join("; "))
        };
        let video = if result.video_start_p99_ms != 0.0 {
            format!(
                ", video start P99 {:.1} s",
                result.video_start_p99_ms / 1000.0
            )
        } else {
            String::new()
        };
        logger.line(&format!(
            "  {outcome}, busiest worker {:.2}, drops {:.3}%, generator {:.2} cores{video}{detail}",
            result.busiest_worker(),
            result.drop_share() * 100.0,
            result.generator_cores
        ))?;
        if result.host_steal > limits.steal_share {
            logger.line(&format!(
                concat!(
                    "  warning: the hypervisor withheld {:.0}% of the host's CPU ",
                    "(steal); repeat the calibration to see whether that is typical here"
                ),
                result.host_steal * 100.0
            ))?;
        }
        history.push((result, verdict));
    }
    Ok(history)
}

pub(super) fn output(options: &Options, clock: &dyn Clock) -> PathBuf {
    options
        .output
        .clone()
        .filter(|value| !value.as_os_str().is_empty())
        .unwrap_or_else(|| {
            let stamp = clock.utc();
            let date = stamp.get(..19).unwrap_or("unknown").replace(['-', ':'], "");
            PathBuf::from(format!("results/capacity.{date}Z"))
        })
}

pub(super) async fn run(
    options: &Options,
    engine: &dyn Engine,
    clock: &dyn Clock,
    cancellation: &Cancellation,
    output: &Path,
) -> Result<()> {
    files::directory(output)?;
    let mut logger = Logger {
        file: files::private_file(&output.join("calibration.log"))?,
        clock,
    };
    let measured: Result<()> = async {
        let images = host::Images {
            server: host::image(engine, &options.server_image).await?,
            generator: host::image(engine, &options.generator_image).await?,
        };
        logger.line("reading host facts (a 5 s idle sample)")?;
        let run_id = hex::encode(rand::random::<[u8; 8]>());
        let ownership = Ownership::new(output, &run_id);
        let host = host::facts(engine, &options.server_image, &ownership, &run_id).await?;
        ensure!(
            host.logical_cpus >= 1 && host.memory_mib > 0,
            "host facts lack CPU or memory capacity"
        );
        let deployment = report::Deployment::new(&host, options)?;
        let shapes = Workload::ALL
            .into_iter()
            .map(|workload| {
                (
                    workload,
                    search::shape(
                        workload,
                        host.logical_cpus,
                        deployment.workers,
                        options.server_cpus,
                        options.generator_cpus,
                    ),
                )
            })
            .collect::<BTreeMap<_, _>>();
        for warning in host::warnings(&host) {
            logger.line(&format!("warning: {warning}"))?;
        }
        let mut context = Context {
            engine,
            clock,
            cancellation,
            ownership: &ownership,
            run_id: &run_id,
            server_image: &options.server_image,
            generator_image: &options.generator_image,
            revisions: (
                revision(&images.server.revision),
                revision(&images.generator.revision),
            ),
            shape: shapes[&Workload::Meetings],
            worker_threshold: deployment.worker_threshold,
            server_memory: memory(host.memory_mib, 0.3, 4096),
            generator_memory: memory(host.memory_mib, 0.5, 8192),
            browser: &options.browser,
            output,
            server_env: &options.server_env,
        };
        let limits = Limits {
            worker_load: deployment.worker_threshold,
            ..Limits::default()
        };
        let timing = if options.quick { (20, 30) } else { (30, 60) };
        let steps = options
            .steps
            .filter(|value| *value != 0)
            .unwrap_or(if options.quick { 3 } else { 6 });
        logger.line(&format!(
            "host {} CPUs, {}; server {}, generator {}",
            host.logical_cpus, host.cpu_model, context.server_memory, context.generator_memory
        ))?;
        let mut results = report::Results::new();
        let mut index = 1;
        let mut interrupted = None;
        for selected in Workload::ALL {
            if !options.workloads.contains(&selected) || cancellation.code() != 0 {
                continue;
            }
            context.shape = shapes[&selected];
            logger.line(&format!(
                "{selected}: server {} CPUs ({} workers), generator {} CPUs",
                general(context.shape.server_cpus),
                context.shape.workers,
                general(context.shape.generator_cpus)
            ))?;
            let search = search::search(
                selected,
                context.shape,
                options.meeting_size,
                steps,
                options.first_size,
                options.browser.audio_only,
            );
            let history = workload(&context, search, limits, timing, index, &mut logger).await?;
            index += history.len();
            if cancellation.code() != 0 {
                interrupted = Some(selected);
            }
            results.insert(selected, history);
        }
        context.shape = shapes[&Workload::Meetings];
        let ceilings = Workload::ALL
            .into_iter()
            .map(|workload| {
                let mut ceiling = search::ceiling(
                    workload,
                    results.get(&workload).map_or(&[][..], Vec::as_slice),
                    shapes[&workload].generator_cpus,
                    shapes[&workload].workers,
                );
                if interrupted == Some(workload)
                    && ceiling.ceiling.is_some()
                    && ceiling.bound != "measured"
                {
                    ceiling.bound = "interrupted".into();
                }
                (workload, ceiling)
            })
            .collect();
        let report = report::build(
            options,
            report::Measured {
                context: &context,
                host: &host,
                images: &images,
                deployment: &deployment,
                shapes: &shapes,
                limits,
            },
            &results,
            &ceilings,
        )?;
        files::replace(output, "calibration.json", &json::bytes(&report)?)?;
        let mut stdout = std::io::stdout().lock();
        for line in report::summary(&report) {
            writeln!(stdout, "{line}")?;
        }
        writeln!(
            stdout,
            "\nReport: {}",
            output.join("calibration.json").display()
        )?;
        Ok(())
    }
    .await;
    if let Err(error) = &measured {
        // A pre-window failure still leaves a private, explicit failed receipt.
        // The original ownership journal remains available for exact recovery.
        let saved = artifacts::new_file(
            &output.join("failure.json"),
            &host::failure(error, cancellation.code())?,
        );
        if let Err(save) = saved {
            return Err(anyhow::anyhow!(
                "{error}; failed to save failure evidence: {save}"
            ));
        }
    }
    measured
}
fn revision(value: &str) -> String {
    if value.is_empty() {
        "unknown".into()
    } else {
        value.chars().take(16).collect()
    }
}
