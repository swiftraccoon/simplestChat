//! Sequential production-shape comparisons with injectable engine, clock and metrics.

use super::{
    engine::{Engine, require_image, valid_image_id},
    options::Options,
    report, step,
};
use crate::bench::{artifacts, metrics::Metrics, provenance};
use anyhow::Result;
use serde_json::{Value, json};
use std::path::Path;

const LIMITATIONS: [&str; 4] = [
    concat!(
        "Co-located server and generator inside one Linux VM on Apple silicon: ",
        "the CPU quota is the production shape, the cores are not the production cores."
    ),
    concat!(
        "Loopback media; impairment applies only when --netem is set and ",
        "then only to UDP on loopback."
    ),
    concat!(
        "Per-thread CPU uses first/last thread sums, so threads that exit inside ",
        "the window are excluded; cgroup usage and throttling are exact."
    ),
    concat!(
        "Synthetic RTP without simulcast: forwarding cost and delivery, ",
        "not browser quality or layer selection."
    ),
];

async fn image(engine: &Engine, image: &str) -> Result<Value> {
    let text = engine
        .run(
            &[
                "image".into(),
                "inspect".into(),
                "--format".into(),
                "{{.Id}} {{.Digest}}".into(),
                image.into(),
            ],
            30,
            false,
        )
        .await?;
    let mut fields = text.split_whitespace();
    let id = fields.next().unwrap_or("");
    anyhow::ensure!(
        valid_image_id(id),
        "Image identity is unavailable or invalid"
    );
    let mut identity = json!({"image":image,"id":id});
    if let Some(digest) = fields.next() {
        identity["digest"] = digest.into();
    }
    Ok(identity)
}

pub(super) async fn manifest(
    options: &Options,
    engine: &Engine,
    executable: &Path,
) -> Result<(Value, Options)> {
    let info: Value = serde_json::from_str(
        &engine
            .run(
                &["info".into(), "--format".into(), "json".into()],
                30,
                false,
            )
            .await?,
    )?;
    let server = image(engine, &options.server_image).await?;
    let generator = image(engine, &options.generator_image).await?;
    let mut selected = options.clone();
    selected.server_image = server["id"].as_str().unwrap().into();
    selected.generator_image = generator["id"].as_str().unwrap().into();
    let mut sidecars = serde_json::Map::new();
    if options.netem.is_some() {
        let identity = image(engine, &options.netem_image).await?;
        selected.netem_image = identity["id"].as_str().unwrap().into();
        sidecars.insert("netem".into(), identity);
    }
    if options.capture_handshakes {
        let identity = image(engine, &options.capture_image).await?;
        selected.capture_image = identity["id"].as_str().unwrap().into();
        sidecars.insert("capture".into(), identity);
    }
    let (hash, bytes) = provenance::hash_file(executable)?;
    Ok((
        json!({"schemaVersion":1,"startedAt":engine.clock.utc(),"options":options,
        "label":options.label,"sourceRevision":options.source_revision,
        "environment":{"orchestratorPlatform":std::env::consts::OS,
            "orchestratorArch":std::env::consts::ARCH,"implementation":"rust",
            "vm":{"kernel":info["host"]["kernel"],"arch":info["host"]["arch"],
                "cpus":info["host"]["cpus"],"memoryBytes":info["host"]["memTotal"],
                "cgroupVersion":info["host"]["cgroupVersion"],"podman":info["version"]["Version"]}},
        "server":server,"generator":generator,"sidecars":sidecars,"orchestratorSha256":hash,
        "orchestratorBytes":bytes,
        "limitations":LIMITATIONS}),
        selected,
    ))
}

pub(super) async fn execute(
    options: &Options,
    engine: &Engine,
    metrics: &dyn Metrics,
    executable: &Path,
) -> Result<Value> {
    for image in [&options.server_image, &options.generator_image] {
        require_image(image)?;
    }
    if options.netem.is_some() {
        require_image(&options.netem_image)?;
    }
    if options.capture_handshakes {
        require_image(&options.capture_image)?;
    }
    // Establish the same fresh, nonrecursive private output boundary before
    // querying the selected already-running engine. No VM start or image pull.
    artifacts::directory(&options.output)?;
    let mut rows = Vec::new();
    let result: Result<Value> = async {
        let (manifest, selected) = manifest(options, engine, executable).await?;
        artifacts::json(&options.output.join("manifest.json"), &manifest)?;
        for scenario in &options.scenarios {
            for repetition in 1..=options.repetitions {
                rows.push(
                    step::run(step::Context {
                        options: &selected,
                        scenario,
                        repetition,
                        manifest: &manifest,
                        engine,
                        metrics,
                    })
                    .await?,
                );
            }
        }
        report::comparison(&rows)
    }
    .await;
    match result {
        Ok(comparison) => {
            artifacts::json(&options.output.join("comparison.json"), &comparison)?;
            Ok(comparison)
        }
        Err(error) => {
            let evidence = json!({"purpose":"production-shape","completed":false,
                "passed":false,"rows":rows,"error":format!("{error:#}")});
            if let Err(evidence) =
                artifacts::json(&options.output.join("comparison.json"), &evidence)
            {
                return Err(error.context(format!("Comparison evidence failed: {evidence}")));
            }
            Err(error)
        }
    }
}
