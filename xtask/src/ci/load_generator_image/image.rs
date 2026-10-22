//! Observed selected image metadata, without projecting arbitrary config values.

use super::{
    engine::{self, Clock, Core},
    plan::{self, Context},
};
use crate::{ci::process::Executor, tooling::Result};
use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Image {
    id: String,
    os: String,
    architecture: String,
    config: Config,
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Config {
    user: String,
    working_dir: String,
    entrypoint: Vec<String>,
    cmd: Vec<String>,
    labels: BTreeMap<String, String>,
    env: Vec<String>,
}

pub(super) async fn selected<E: Executor, C: Clock>(
    context: &Context,
    core: &mut Core<'_, E, C>,
) -> Result<String> {
    let bytes = core
        .run(core.command(
            &["image", "inspect", "--format", "{{json .}}", plan::TAG],
            false,
        ))
        .await?;
    let value: Image = serde_json::from_slice(&bytes).map_err(|_| plan::failure())?;
    if !engine::image_id(&value.id)
        || value.os != "linux"
        || value.architecture != "amd64"
        || value.config.user != "10001:10001"
        || value.config.working_dir != "/results"
        || value.config.entrypoint != ["/app/load_test"]
        || value.config.cmd != ["--help"]
        || value.config.labels.get("org.opencontainers.image.revision") != Some(&context.revision)
        || value
            .config
            .env
            .iter()
            .filter(|v| v.starts_with("SOURCE_REVISION="))
            .collect::<Vec<_>>()
            != [&format!("SOURCE_REVISION={}", context.revision)]
    {
        return Err(plan::failure());
    }
    Ok(value.id)
}
