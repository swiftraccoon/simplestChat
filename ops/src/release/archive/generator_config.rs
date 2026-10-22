//! Fixed generator runtime/config policy, independent of the signed server.

use super::{GeneratorSelection, MAX_LAYERS, digest};
use crate::{
    json::Json,
    release::archive::{ArchiveError as E, Result},
};

pub(super) fn validate(config: &Json, selection: &GeneratorSelection) -> Result<Vec<String>> {
    if config.get("os").and_then(Json::string) != Some("linux")
        || config.get("architecture").and_then(Json::string) != Some("amd64")
        || config
            .get("variant")
            .is_some_and(|value| value.string() != Some(""))
    {
        return Err(E::Platform);
    }
    let runtime = config
        .get("config")
        .filter(|value| value.object().is_some())
        .ok_or(E::User)?;
    if runtime.get("User").and_then(Json::string) != Some("10001:10001") {
        return Err(E::User);
    }
    if runtime
        .get("Labels")
        .and_then(|v| v.get("org.opencontainers.image.revision"))
        .and_then(Json::string)
        != Some(selection.revision.as_str())
    {
        return Err(E::Revision);
    }
    let env = runtime
        .get("Env")
        .and_then(Json::array)
        .filter(|v| v.len() <= 4096)
        .ok_or(E::Revision)?;
    let mut source = None;
    for value in env {
        let value = value.string().ok_or(E::Revision)?;
        if let Some(value) = value.strip_prefix("SOURCE_REVISION=")
            && source.replace(value).is_some()
        {
            return Err(E::Revision);
        }
    }
    if source != Some(selection.revision.as_str()) {
        return Err(E::Revision);
    }
    let command = |key, expected| {
        runtime
            .get(key)
            .and_then(Json::array)
            .is_some_and(|v| v.len() == 1 && v[0].string() == Some(expected))
    };
    if !command("Entrypoint", "/app/load_test")
        || !command("Cmd", "--help")
        || runtime.get("WorkingDir").and_then(Json::string) != Some("/results")
    {
        return Err(E::Command);
    }
    let root = config.get("rootfs").ok_or(E::Layers)?;
    if root.get("type").and_then(Json::string) != Some("layers") {
        return Err(E::Layers);
    }
    let values = root
        .get("diff_ids")
        .and_then(Json::array)
        .filter(|v| !v.is_empty() && v.len() <= MAX_LAYERS)
        .ok_or(E::Layers)?;
    values
        .iter()
        .map(|value| {
            let value = value.string().ok_or(E::Layers)?;
            digest(value)?;
            Ok(value.into())
        })
        .collect()
}
