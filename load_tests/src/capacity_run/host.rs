//! Image provenance and a bounded isolated host-facts probe.

use super::{
    engine::{Engine, sandbox},
    json,
    ownership::Ownership,
    parsers,
};
use anyhow::{Result, ensure};
use serde::Serialize;

#[derive(Clone, Serialize)]
pub(super) struct Image {
    pub(super) reference: String,
    pub(super) id: String,
    pub(super) revision: String,
}
#[derive(Clone, Serialize)]
pub(super) struct Images {
    pub(super) server: Image,
    pub(super) generator: Image,
}
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Host {
    pub(super) cpu_model: String,
    pub(super) platform: String,
    pub(super) logical_cpus: i64,
    pub(super) memory_mib: i64,
    pub(super) idle_busy_share: f64,
    pub(super) idle_steal_share: f64,
    pub(super) rmem_max: i64,
    pub(super) kernel: String,
}

pub(super) async fn image(engine: &dyn Engine, reference: &str) -> Result<Image> {
    let id = engine
        .run(
            &[
                "image".into(),
                "inspect".into(),
                "--format".into(),
                "{{.Id}}".into(),
                reference.into(),
            ],
            30,
            true,
        )
        .await?
        .trim()
        .to_owned();
    let revision = engine
        .run(
            &[
                "image".into(),
                "inspect".into(),
                "--format".into(),
                "{{index .Config.Labels \"org.opencontainers.image.revision\"}}".into(),
                reference.into(),
            ],
            30,
            true,
        )
        .await?
        .trim()
        .to_owned();
    Ok(Image {
        reference: reference.into(),
        id,
        revision: if revision == "<no value>" {
            String::new()
        } else {
            revision
        },
    })
}

pub(super) fn rounded(value: f64, digits: usize) -> f64 {
    // Formatting rounds the original binary value, as Python round does;
    // multiplying before rounding would introduce a different midpoint.
    format!("{value:.digits$}")
        .parse()
        .expect("a formatted f64 parses")
}

pub(super) fn parse(output: &str) -> Result<Host> {
    let parts = output.split("@@").map(str::trim).collect::<Vec<_>>();
    ensure!(
        parts.len() == 7,
        "could not read the host's /proc facts through the server image"
    );
    let a = parsers::proc_stat(parts[2])?;
    let b = parsers::proc_stat(parts[3])?;
    let total = (b.total - a.total).max(1) as f64;
    Ok(Host {
        cpu_model: parsers::cpu_model(parts[0]),
        platform: parsers::platform(parts[6]),
        logical_cpus: parsers::count_cpus(parts[2]),
        memory_mib: parsers::memory_kib(parts[1])? / 1024,
        idle_busy_share: rounded(1.0 - (b.idle - a.idle) as f64 / total, 4),
        idle_steal_share: rounded((b.steal - a.steal) as f64 / total, 4),
        rmem_max: parts[4].parse().unwrap_or(0),
        kernel: parts[5].into(),
    })
}

pub(super) async fn facts(
    engine: &dyn Engine,
    image: &str,
    ownership: &Ownership,
    run_id: &str,
) -> Result<Host> {
    let name = format!("capacity-probe-{run_id}");
    ownership.remember(&name, image, "")?;
    let mut arguments = vec!["run".into(), "--rm".into()];
    arguments.extend(sandbox(engine));
    arguments.extend([
        "--cpus".into(),
        "0.25".into(),
        "--memory".into(),
        "128m".into(),
        "--memory-swap".into(),
        "128m".into(),
        "--pids-limit".into(),
        "64".into(),
        "--name".into(),
        name.clone(),
        "--label".into(),
        format!("simplestchat.capacity.run={run_id}"),
        "--network".into(),
        "none".into(),
        "--entrypoint".into(),
        "sh".into(),
        image.into(),
        "-c".into(),
        concat!(
            "cat /proc/cpuinfo; echo @@; cat /proc/meminfo; echo @@; cat /proc/stat; echo @@; ",
            "sleep 5; cat /proc/stat; echo @@; cat /proc/sys/net/core/rmem_max; echo @@; ",
            "cat /proc/sys/kernel/osrelease; echo @@; ",
            "cat /sys/class/dmi/id/sys_vendor /sys/class/dmi/id/product_name 2>/dev/null; true"
        )
        .into(),
    ]);
    let result = engine.run(&arguments, 120, true).await;
    let cleanup = ownership.remove(engine, &name).await;
    let output = match (result, cleanup) {
        (Ok(output), Ok(())) => output,
        (Err(error), Ok(())) => return Err(error),
        (result, Err(error)) => {
            return Err(error.context(result.err().map_or_else(
                || "host probe cleanup failed".into(),
                |original| format!("{original}; host probe cleanup failed"),
            )));
        }
    };
    parse(&output)
}

pub(super) fn warnings(host: &Host) -> Vec<String> {
    let mut warnings = Vec::new();
    if host.idle_busy_share > 0.1 {
        warnings.push(format!(
            "the host was {:.0}% busy before any load",
            host.idle_busy_share * 100.0
        ));
    }
    if host.logical_cpus < 2 {
        warnings.push(
            "one CPU cannot hold both the server and the generator; figures are rough".into(),
        );
    }
    if host.rmem_max < 2_097_152 {
        warnings.push(
            concat!(
                "net.core.rmem_max clamps the workers' 1 MiB socket buffers, so the kernel drops ",
                "bursts the server would carry and the ceilings come out low; first run ",
                "sysctl -w net.core.rmem_max=2097152 net.core.wmem_max=2097152"
            )
            .into(),
        );
    }
    warnings
}

pub(super) fn failure(error: &anyhow::Error, interrupted: u8) -> Result<Vec<u8>> {
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Failure<'a> {
        schema_version: u32,
        complete: bool,
        interrupted: u8,
        error: &'a str,
    }
    json::bytes(&Failure {
        schema_version: 1,
        complete: false,
        interrupted,
        error: &error.to_string(),
    })
}
