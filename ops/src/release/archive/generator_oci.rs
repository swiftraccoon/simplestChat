//! Complete one-branch OCI/containerd graph. Every referenced stored blob must
//! be present and match its descriptor, including layers, before identity exists.

use super::{CANONICAL, Content, Docker, digest};
use crate::{
    json::Json,
    release::archive::{ArchiveError as E, Result, checkpoint},
};
use std::{
    collections::BTreeSet,
    io::{Read, Seek},
};

const INDEX: [&str; 2] = [
    "application/vnd.oci.image.index.v1+json",
    "application/vnd.docker.distribution.manifest.list.v2+json",
];
const MANIFEST: [&str; 2] = [
    "application/vnd.oci.image.manifest.v1+json",
    "application/vnd.docker.distribution.manifest.v2+json",
];
const CONFIG: [&str; 2] = [
    "application/vnd.oci.image.config.v1+json",
    "application/vnd.docker.container.image.v1+json",
];
const RAW: [&str; 2] = [
    "application/vnd.oci.image.layer.v1.tar",
    "application/vnd.docker.image.rootfs.diff.tar",
];
const COMPRESSED: [&str; 3] = [
    "application/vnd.oci.image.layer.v1.tar+gzip",
    "application/vnd.oci.image.layer.v1.tar+zstd",
    "application/vnd.docker.image.rootfs.diff.tar.gzip",
];

struct Descriptor {
    digest: String,
    path: String,
    size: u64,
    media: String,
}
fn schema(value: &Json) -> bool {
    value.get("schemaVersion").and_then(Json::integer) == Some(2)
}
fn annotations(value: Option<&Json>, required: bool) -> Result<()> {
    let Some(value) = value else {
        return if required {
            Err(E::AnnotationMissing)
        } else {
            Ok(())
        };
    };
    let value = value.object().ok_or(E::Annotations)?;
    let image = value.get("io.containerd.image.name");
    let reference = value.get("org.opencontainers.image.ref.name");
    if image.is_some_and(|v| v.string() != Some(CANONICAL))
        || reference.is_some_and(|v| v.string() != Some("ci"))
    {
        return Err(E::AnnotationTag);
    }
    if required && (image.is_none() || reference.is_none()) {
        return Err(E::AnnotationMissing);
    }
    Ok(())
}
fn descriptor(value: &Json) -> Result<Descriptor> {
    let digest_text = value
        .get("digest")
        .and_then(Json::string)
        .ok_or(E::DescriptorDigest)?;
    let path = format!("blobs/sha256/{}", digest(digest_text)?);
    let size = value
        .get("size")
        .and_then(Json::integer)
        .and_then(|v| u64::try_from(v).ok())
        .filter(|v| *v <= super::super::MAX_ARCHIVE_BYTES)
        .ok_or(E::DescriptorSize)?;
    if value
        .get("urls")
        .is_some_and(|v| !v.array().is_some_and(|v| v.is_empty()))
        || value.get("data").is_some()
    {
        return Err(E::DescriptorUrls);
    }
    if let Some(platform) = value.get("platform")
        && (platform.get("os").and_then(Json::string) != Some("linux")
            || platform.get("architecture").and_then(Json::string) != Some("amd64")
            || platform
                .get("variant")
                .is_some_and(|v| v.string() != Some("")))
    {
        return Err(E::Platform);
    }
    annotations(value.get("annotations"), false)?;
    Ok(Descriptor {
        digest: digest_text.into(),
        path,
        size,
        media: value
            .get("mediaType")
            .and_then(Json::string)
            .ok_or(E::ImageManifest)?
            .into(),
    })
}
fn verified<R: Read + Seek, F: FnMut() -> bool>(
    content: &mut Content<'_, R, F>,
    desc: &Descriptor,
) -> Result<()> {
    checkpoint(content.check)?;
    let (hash, size) = content.hash(&desc.path)?;
    if size != desc.size {
        return Err(E::DescriptorSize);
    }
    if desc.digest != format!("sha256:{hash}") {
        return Err(E::MetadataDigest);
    }
    Ok(())
}
fn only(value: &Json) -> Result<&Json> {
    if !schema(value) || value.get("subject").is_some() || value.get("artifactType").is_some() {
        return Err(E::Index);
    }
    value
        .get("manifests")
        .and_then(Json::array)
        .filter(|v| v.len() == 1)
        .map(|v| &v[0])
        .ok_or(E::IndexCount)
}

pub(super) fn verify<R: Read + Seek, F: FnMut() -> bool>(
    content: &mut Content<'_, R, F>,
    docker: &Docker,
) -> Result<String> {
    let layout = content.json("oci-layout")?;
    if !layout.object().is_some_and(|v| {
        v.len() == 1 && v.get("imageLayoutVersion").and_then(Json::string) == Some("1.0.0")
    }) {
        return Err(E::Layout);
    }
    let index = content.json("index.json")?;
    if index
        .get("mediaType")
        .is_some_and(|value| value.string() != Some(INDEX[0]))
    {
        return Err(E::Index);
    }
    let outer = only(&index)?;
    annotations(outer.get("annotations"), true)?;
    let mut current = descriptor(outer)?;
    let root = current.digest.clone();
    let mut visited = BTreeSet::new();
    for _ in 0..8 {
        checkpoint(content.check)?;
        if !visited.insert(current.digest.clone()) {
            return Err(E::IndexDepth);
        }
        verified(content, &current)?;
        let value = content.json(&current.path)?;
        if value
            .get("mediaType")
            .is_some_and(|value| value.string() != Some(current.media.as_str()))
        {
            return Err(E::ImageManifest);
        }
        if INDEX.contains(&current.media.as_str()) {
            current = descriptor(only(&value)?)?;
            continue;
        }
        if !MANIFEST.contains(&current.media.as_str())
            || !schema(&value)
            || value.get("subject").is_some()
            || value.get("artifactType").is_some()
        {
            return Err(E::ImageManifest);
        }
        let config = descriptor(value.get("config").ok_or(E::ConfigMismatch)?)?;
        if !CONFIG.contains(&config.media.as_str()) || config.path != docker.config_name {
            return Err(E::ConfigMismatch);
        }
        verified(content, &config)?;
        if config.digest != format!("sha256:{}", docker.config_hash)
            || !content.json(&config.path)?.semantic_eq(&docker.config)
        {
            return Err(E::ConfigMismatch);
        }
        let layers = value
            .get("layers")
            .and_then(Json::array)
            .filter(|v| v.len() == docker.layers.len())
            .ok_or(E::LayersMismatch)?;
        for (number, layer) in layers.iter().enumerate() {
            let layer = descriptor(layer)?;
            if layer.path != docker.layers[number]
                || !(RAW.contains(&layer.media.as_str())
                    || COMPRESSED.contains(&layer.media.as_str()))
            {
                return Err(E::LayersMismatch);
            }
            verified(content, &layer)?;
            if RAW.contains(&layer.media.as_str()) && layer.digest != docker.diff_ids[number] {
                return Err(E::LayersMismatch);
            }
        }
        return Ok(root);
    }
    Err(E::IndexDepth)
}
