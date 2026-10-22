//! Fresh private output, frozen inputs and alternating paired run order.

use super::step::{Context, Services};
use crate::bench::{
    artifacts, comparison, environment,
    options::{Options, Purpose},
    provenance::{self, ServerIdentity},
    verdict,
};
use anyhow::Result;
use serde_json::{Value, json};
use std::path::Path;

pub struct Manifest {
    pub baseline: ServerIdentity,
    pub candidate: ServerIdentity,
    pub generator_sha256: String,
    pub generator_source_identity: String,
    pub value: Value,
}

impl Manifest {
    pub fn server(&self, variant: &str) -> Result<&ServerIdentity> {
        match variant {
            "baseline" => Ok(&self.baseline),
            "candidate" => Ok(&self.candidate),
            _ => anyhow::bail!("Invalid benchmark variant"),
        }
    }
}

pub async fn manifest(
    options: &Options,
    services: &Services<'_>,
    executable: &Path,
) -> Result<Manifest> {
    let baseline = provenance::identity(
        services.host,
        services.environment,
        &options.baseline_root,
        &options.baseline_bin,
    )
    .await?;
    let candidate = provenance::identity(
        services.host,
        services.environment,
        &options.candidate_root,
        &options.candidate_bin,
    )
    .await?;
    let (generator_sha256, generator_bytes) = provenance::hash_file(&options.generator)?;
    let generator_source_identity =
        provenance::generator_source_identity(&options.generator_source_root)?;
    let (orchestrator_hash, orchestrator_bytes) = provenance::hash_file(executable)?;
    let environment =
        environment::observe(services.host, &options.candidate_root, services.environment).await?;
    let value = json!({"schemaVersion":1,"startedAt":services.clock.utc(),"options":options,
        "environment":environment,
        "servers":{"baseline":baseline,"candidate":candidate},
        "generator":{"binary":options.generator,"binarySha256":generator_sha256,
            "binaryBytes":generator_bytes,"sourceRoot":options.generator_source_root,
            "sourceIdentity":generator_source_identity},
        "orchestratorSha256":orchestrator_hash,"orchestratorBytes":orchestrator_bytes,
        "diagnosticReaderCommand":["report","diagnostics"],
        "mediaDiagnosticReaderCommand":["report","media"],
        "receiverStallReaderCommand":["report","receiver-stall"],
        "lifecycleDiagnosticReaderCommand":["report","lifecycle"],
        "readerCompatibility":"selected executable output must match linked fixed reader protocol",
        "limitations":[
            "Co-located server/generator: not production capacity.",
            concat!("Resource summaries require window coverage within1.5seconds ",
                "and at most10% failed samples."),
            concat!("Source snapshots and independently frozen executables do not attest a ",
                "reproducible build; ignored/ancestor Cargo settings, external native libraries, ",
                "environment and toolchains require separate evidence."),
            concat!("Source links identify link text, not external target contents; ",
                "nonUTF8 paths, ancestor links and linked Cargo.lock are refused."),
            "Diagnostic runs skip resource sampling and never produce performance comparisons.",
            concat!("RSS is a sampled peak; CPU counts each owned process, including in-process ",
                "media workers, without child-process resource attribution."),
            concat!("Synthetic queued RTP is not confirmed egress; receive counts do not ",
                "estimate loss without expected fan-out."),
            "Latency/CPU differences are descriptive, not an established regression budget."]});
    Ok(Manifest {
        baseline,
        candidate,
        generator_sha256,
        generator_source_identity,
        value,
    })
}

pub async fn execute(
    options: &Options,
    services: &Services<'_>,
    executable: &Path,
) -> Result<Value> {
    execute_inner(options, services, executable, None).await
}

pub(super) async fn execute_recorded(
    options: &Options,
    services: &Services<'_>,
    executable: &Path,
    observed: &super::receipt::ObservedHost<'_>,
) -> Result<Value> {
    execute_inner(options, services, executable, Some(observed)).await
}

async fn execute_inner(
    options: &Options,
    services: &Services<'_>,
    executable: &Path,
    observed: Option<&super::receipt::ObservedHost<'_>>,
) -> Result<Value> {
    artifacts::directory(&options.output)?;
    if let Some(observed) = observed {
        observed.bind_output(&options.output)?;
    }
    let mut rows = Vec::new();
    let outcome: Result<Value> = async {
        let manifest = manifest(options, services, executable).await?;
        artifacts::json(&options.output.join("manifest.json"), &manifest.value)?;
        for scenario in &options.scenarios {
            for repetition in 1..=options.repetitions {
                let order = if repetition % 2 == 1 {
                    ["baseline", "candidate"]
                } else {
                    ["candidate", "baseline"]
                };
                for variant in order {
                    let row = super::run_one(Context {
                        options,
                        scenario,
                        variant,
                        repetition,
                        manifest: &manifest,
                        services,
                    })
                    .await?;
                    if let Some(observed) = observed {
                        observed.completed_run();
                    }
                    rows.push(row);
                }
            }
        }
        let status = if options.purpose == Purpose::Diagnostic {
            serde_json::to_value(verdict::diagnostic(&rows))?
        } else {
            serde_json::to_value(verdict::performance(&rows))?
        };
        let comparisons = if options.purpose == Purpose::Performance && status["passed"] == true {
            comparison::compare(&rows)?
        } else {
            Value::Null
        };
        let mut result = json!({"purpose":options.purpose,"completed":true,
            "rows":rows,"comparison":comparisons});
        result
            .as_object_mut()
            .unwrap()
            .extend(status.as_object().unwrap().clone());
        Ok(result)
    }
    .await;
    match outcome {
        Ok(result) => {
            artifacts::json(&options.output.join("comparison.json"), &result)?;
            Ok(result)
        }
        Err(error) => {
            let _ = artifacts::json(
                &options.output.join("comparison.json"),
                &json!({
                "purpose":options.purpose,"completed":false,"passed":false,
                "rows":rows,"error":"local_benchmark_failed"}),
            );
            Err(error)
        }
    }
}
