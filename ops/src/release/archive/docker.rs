//! Docker's selected image and runtime identity, in the reference failure order.

use super::index::{Index, member_name};
use super::{ArchiveError as E, Result, oci};
use crate::json::{self, Json};
use crate::release::manifest::Manifest;
use std::io::{Read, Seek};

pub(super) fn verify(
    file: &mut (impl Read + Seek),
    index: &Index,
    manifest: &Manifest,
) -> Result<(Vec<u8>, Option<String>)> {
    let images = index.read_json(file, "manifest.json")?;
    let images = images
        .array()
        .filter(|v| v.len() == 1)
        .ok_or(E::ImageCount)?;
    let image = images[0].object().ok_or(E::ImageCount)?;
    let tag = manifest.fields().image_tag.as_str();
    if image
        .get("RepoTags")
        .and_then(Json::array)
        .is_none_or(|v| v.len() != 1 || v[0].string() != Some(tag))
    {
        return Err(E::Tag);
    }
    let layers = image
        .get("Layers")
        .and_then(Json::array)
        .filter(|v| !v.is_empty())
        .ok_or(E::Layers)?;
    for layer in layers {
        let name = member_name(layer.string().ok_or(E::Path)?)?;
        if !index.member(name).is_some_and(|m| m.file) {
            return Err(E::Layer);
        }
    }
    let config_name = image.get("Config").and_then(Json::string).ok_or(E::Path)?;
    let bytes = index.read_bytes(file, config_name)?;
    let config = json::decode(&bytes)?;
    let runtime_manifest = oci::verify(file, index, image, &config, manifest)?;
    if index.has("repositories") {
        let repositories = index.read_json(file, "repositories")?;
        let (repository, tag) = tag.rsplit_once(':').unwrap();
        let valid = repositories
            .object()
            .filter(|v| v.len() == 1)
            .and_then(|v| v.get(repository))
            .and_then(Json::object)
            .is_some_and(|v| v.len() == 1 && v.contains_key(tag));
        if !valid {
            return Err(E::Repositories);
        }
    }
    validate_config(&config, manifest)?;
    Ok((bytes, runtime_manifest))
}

fn validate_config(config: &Json, manifest: &Manifest) -> Result<()> {
    if config.object().is_none()
        || config.get("os").and_then(Json::string) != Some("linux")
        || config.get("architecture").and_then(Json::string) != Some("amd64")
    {
        return Err(E::Platform);
    }
    let runtime = config
        .get("config")
        .filter(|v| v.object().is_some())
        .ok_or(E::User)?;
    if runtime.get("User").and_then(Json::string) != Some("10001:10001") {
        return Err(E::User);
    }
    if runtime
        .get("Labels")
        .and_then(|v| v.get("org.opencontainers.image.revision"))
        .and_then(Json::string)
        != Some(manifest.fields().revision.as_str())
    {
        return Err(E::Revision);
    }
    if runtime
        .get("Cmd")
        .and_then(Json::array)
        .is_none_or(|v| v.len() != 1 || v[0].string() != Some("/app/simplestChat"))
    {
        return Err(E::Command);
    }
    if !matches!(runtime.get("Entrypoint"), None | Some(Json::Null))
        && !runtime
            .get("Entrypoint")
            .and_then(Json::array)
            .is_some_and(|v| v.is_empty())
    {
        return Err(E::Command);
    }
    Ok(())
}
