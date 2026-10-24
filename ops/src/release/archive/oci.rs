//! OCI consistency is optional only when both OCI root members are absent.
//! Descriptor metadata is decoded before digest comparison, matching Python.

use super::index::Index;
use super::{ArchiveError as E, Result, sha256};
use crate::json::{self, Json};
use crate::release::manifest::Manifest;
use std::collections::BTreeMap;
use std::io::{Read, Seek};

fn schema_two(value: &Json) -> bool {
    value
        .get("schemaVersion")
        .is_some_and(|v| v.semantic_eq(&Json::Integer("2".into())))
}
fn descriptor_path(index: &Index, descriptor: &Json) -> Result<String> {
    let descriptor = descriptor.object().ok_or(E::DescriptorDigest)?;
    let digest = descriptor
        .get("digest")
        .and_then(Json::string)
        .ok_or(E::DescriptorDigest)?;
    let hex = digest
        .strip_prefix("sha256:")
        .filter(|s| {
            s.len() == 64
                && s.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
        .ok_or(E::DescriptorDigest)?;
    let name = format!("blobs/sha256/{hex}");
    let member = index
        .member(&name)
        .filter(|m| m.file)
        .ok_or(E::DescriptorSize)?;
    if descriptor
        .get("size")
        .and_then(Json::integer)
        .and_then(|n| u64::try_from(n).ok())
        != Some(member.size)
    {
        return Err(E::DescriptorSize);
    }
    if descriptor.get("urls").is_some_and(Json::truthiness) {
        return Err(E::DescriptorUrls);
    }
    Ok(name)
}
fn read_descriptor(
    file: &mut (impl Read + Seek),
    index: &Index,
    descriptor: &Json,
) -> Result<Json> {
    let bytes = index.read_bytes(file, &descriptor_path(index, descriptor)?)?;
    let value = json::decode(&bytes)?;
    if descriptor.get("digest").and_then(Json::string)
        != Some(format!("sha256:{}", sha256(&bytes)).as_str())
    {
        return Err(E::MetadataDigest);
    }
    Ok(value)
}
fn index_descriptor(index: &Json) -> Result<&Json> {
    if !schema_two(index) {
        return Err(E::Index);
    }
    let descriptors = index
        .get("manifests")
        .and_then(Json::array)
        .filter(|v| v.len() == 1)
        .ok_or(E::IndexCount)?;
    if descriptors[0].object().is_none() {
        return Err(E::IndexCount);
    }
    Ok(&descriptors[0])
}
fn annotations(value: Option<&Json>, manifest: &Manifest, outermost: bool) -> Result<()> {
    let empty = BTreeMap::new();
    let value = match value {
        Some(value) => value.object().ok_or(E::Annotations)?,
        None => &empty,
    };
    let image = value
        .get("io.containerd.image.name")
        .filter(|v| !matches!(v, Json::Null));
    let reference = value
        .get("org.opencontainers.image.ref.name")
        .filter(|v| !matches!(v, Json::Null));
    let tag = manifest.fields().image_tag.as_str();
    let prefixed = format!("docker.io/{tag}");
    let named =
        |value: &Json| matches!(value.string(), Some(name) if name == tag || name == prefixed);
    if image.is_some_and(|v| !named(v))
        || reference
            .is_some_and(|v| !named(v) && v.string() != Some(manifest.fields().revision.as_str()))
    {
        return Err(E::AnnotationTag);
    }
    if outermost && image.is_none() && reference.is_none() {
        return Err(E::AnnotationMissing);
    }
    Ok(())
}

pub(super) fn verify(
    file: &mut (impl Read + Seek),
    members: &Index,
    image: &BTreeMap<String, Json>,
    config: &Json,
    manifest: &Manifest,
) -> Result<()> {
    if !members.has("index.json") && !members.has("oci-layout") {
        return Ok(());
    }
    let layout = members.read_json(file, "oci-layout")?;
    if !layout.object().is_some_and(|v| {
        v.len() == 1 && v.get("imageLayoutVersion").and_then(Json::string) == Some("1.0.0")
    }) {
        return Err(E::Layout);
    }
    let mut index = members.read_json(file, "index.json")?;
    for depth in 0..8 {
        let descriptor = index_descriptor(&index)?;
        annotations(descriptor.get("annotations"), manifest, depth == 0)?;
        let resolved = read_descriptor(file, members, descriptor)?;
        let media = descriptor.get("mediaType").and_then(Json::string);
        if matches!(
            media,
            Some(
                "application/vnd.oci.image.index.v1+json"
                    | "application/vnd.docker.distribution.manifest.list.v2+json"
            )
        ) {
            index = resolved;
            continue;
        }
        if !matches!(
            media,
            Some(
                "application/vnd.oci.image.manifest.v1+json"
                    | "application/vnd.docker.distribution.manifest.v2+json"
            )
        ) || !schema_two(&resolved)
        {
            return Err(E::ImageManifest);
        }
        let oci_config = resolved
            .get("config")
            .filter(|v| v.object().is_some())
            .ok_or(E::ConfigMismatch)?;
        if image.get("Config").and_then(Json::string)
            != Some(descriptor_path(members, oci_config)?.as_str())
            || !read_descriptor(file, members, oci_config)?.semantic_eq(config)
        {
            return Err(E::ConfigMismatch);
        }
        let layers = resolved
            .get("layers")
            .and_then(Json::array)
            .ok_or(E::LayersMismatch)?;
        let mut paths = Vec::new();
        for layer in layers {
            paths.push(descriptor_path(members, layer)?);
        }
        let docker_layers = image
            .get("Layers")
            .and_then(Json::array)
            .ok_or(E::LayersMismatch)?;
        if paths.len() != docker_layers.len()
            || !paths
                .iter()
                .zip(docker_layers)
                .all(|(path, layer)| layer.string() == Some(path.as_str()))
        {
            return Err(E::LayersMismatch);
        }
        return Ok(());
    }
    Err(E::IndexDepth)
}
