//! Separate generator archive profile. The signed server verifier is unchanged.
//! No layer is unpacked, decompressed, executed or interpreted as code.

#[path = "generator_config.rs"]
mod config;
#[path = "generator_oci.rs"]
mod oci;

use super::{ArchiveError as E, Limits, Result, checkpoint, index::Index, sha256};
use crate::{
    files::Snapshot,
    json::{self, Json},
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    io::{Read, Seek},
    path::Path,
};

pub(super) const TAG: &str = "simplestchat-loadtest:ci";
pub(super) const CANONICAL: &str = "docker.io/library/simplestchat-loadtest:ci";
pub(super) const MAX_LAYERS: usize = 1024;

/// Caller-selected observed daemon identity and source revision. Neither is
/// authenticated by this constructor; the tag/runtime profile cannot be chosen.
#[derive(Debug)]
pub struct GeneratorSelection {
    image_id: String,
    revision: String,
}
impl GeneratorSelection {
    pub fn new(observed_image_id: &str, revision: &str) -> Result<Self> {
        digest(observed_image_id)?;
        if revision.len() != 40 || !hex(revision) {
            return Err(E::Revision);
        }
        Ok(Self {
            image_id: observed_image_id.into(),
            revision: revision.into(),
        })
    }
}

/// Only fixed vocabulary, measured hashes/counts and the admitted revision.
/// A Docker image identity may name a parent index, not the image config hash.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GeneratorIdentity {
    archive_sha256: String,
    archive_bytes: u64,
    observed_image_id: String,
    config_sha256: String,
    selected_root_digest: Option<String>,
    format: &'static str,
    layers: usize,
    platform: &'static str,
    image_tag: &'static str,
    revision: String,
}
impl GeneratorIdentity {
    pub fn archive_sha256(&self) -> &str {
        &self.archive_sha256
    }
    pub fn archive_bytes(&self) -> u64 {
        self.archive_bytes
    }
}

fn hex(value: &str) -> bool {
    value
        .bytes()
        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
pub(super) fn digest(value: &str) -> Result<&str> {
    value
        .strip_prefix("sha256:")
        .filter(|v| v.len() == 64 && hex(v))
        .ok_or(E::DescriptorDigest)
}

pub(super) struct Content<'a, R, F> {
    file: &'a mut R,
    index: &'a Index,
    check: &'a mut F,
    hashes: BTreeMap<String, (String, u64)>,
}
impl<R: Read + Seek, F: FnMut() -> bool> Content<'_, R, F> {
    fn bytes(&mut self, name: &str) -> Result<Vec<u8>> {
        checkpoint(self.check)?;
        let bytes = self.index.read_bytes(self.file, name)?;
        checkpoint(self.check)?;
        Ok(bytes)
    }
    fn json(&mut self, name: &str) -> Result<Json> {
        let value = json::decode(&self.bytes(name)?)?;
        checkpoint(self.check)?;
        Ok(value)
    }
    fn hash(&mut self, name: &str) -> Result<(String, u64)> {
        if let Some(value) = self.hashes.get(name) {
            return Ok(value.clone());
        }
        let value = self.index.hash_member(self.file, name, self.check)?;
        self.hashes.insert(name.into(), value.clone());
        Ok(value)
    }
}

struct Docker {
    config_name: String,
    config_hash: String,
    config: Json,
    layers: Vec<String>,
    diff_ids: Vec<String>,
}
fn docker<R: Read + Seek, F: FnMut() -> bool>(
    content: &mut Content<'_, R, F>,
    selection: &GeneratorSelection,
) -> Result<Docker> {
    let images = content.json("manifest.json")?;
    let images = images
        .array()
        .filter(|v| v.len() == 1)
        .ok_or(E::ImageCount)?;
    let image = images[0].object().ok_or(E::ImageCount)?;
    let tags = image.get("RepoTags").and_then(Json::array).ok_or(E::Tag)?;
    if tags.len() != 1 || tags[0].string() != Some(TAG) {
        return Err(E::Tag);
    }
    let config_name = image.get("Config").and_then(Json::string).ok_or(E::Path)?;
    if super::index::member_name(config_name)? != config_name {
        return Err(E::NoncanonicalPath);
    }
    let bytes = content.bytes(config_name)?;
    let config = json::decode(&bytes)?;
    let diff_ids = config::validate(&config, selection)?;
    let layers = image
        .get("Layers")
        .and_then(Json::array)
        .filter(|v| !v.is_empty() && v.len() <= MAX_LAYERS)
        .ok_or(E::Layers)?;
    let mut paths = Vec::new();
    for layer in layers {
        checkpoint(content.check)?;
        let name = layer.string().ok_or(E::Path)?;
        if super::index::member_name(name)? != name {
            return Err(E::NoncanonicalPath);
        }
        if !content.index.member(name).is_some_and(|value| value.file) {
            return Err(E::Layer);
        }
        paths.push(name.into());
    }
    if paths.len() != diff_ids.len() {
        return Err(E::LayersMismatch);
    }
    if content.index.has("repositories") {
        let repositories = content.json("repositories")?;
        let repository = repositories
            .object()
            .filter(|v| v.len() == 1)
            .and_then(|v| v.get("simplestchat-loadtest"))
            .and_then(Json::object)
            .filter(|v| v.len() == 1)
            .and_then(|v| v.get("ci"));
        if repository.and_then(Json::string)
            != diff_ids
                .last()
                .and_then(|value| value.strip_prefix("sha256:"))
        {
            return Err(E::Repositories);
        }
    }
    Ok(Docker {
        config_name: config_name.into(),
        config_hash: sha256(&bytes),
        config,
        layers: paths,
        diff_ids,
    })
}

fn validate<R: Read + Seek, F: FnMut() -> bool>(
    content: &mut Content<'_, R, F>,
    selection: &GeneratorSelection,
) -> Result<(&'static str, String, Option<String>, usize)> {
    let docker = docker(content, selection)?;
    let classic = selection.image_id == format!("sha256:{}", docker.config_hash);
    let root = if content.index.has("index.json") || content.index.has("oci-layout") {
        Some(oci::verify(content, &docker)?)
    } else {
        None
    };
    if classic {
        for (name, expected) in docker.layers.iter().zip(&docker.diff_ids) {
            let (actual, _) = content.hash(name)?;
            if expected != &format!("sha256:{actual}") {
                return Err(E::LayersMismatch);
            }
        }
        Ok((
            "classic-docker",
            docker.config_hash,
            None,
            docker.layers.len(),
        ))
    } else {
        let root = root
            .filter(|root| root == &selection.image_id)
            .ok_or(E::DescriptorDigest)?;
        Ok((
            "containerd-oci",
            docker.config_hash,
            Some(root),
            docker.layers.len(),
        ))
    }
}

fn transfer_hash(snapshot: &mut Snapshot, check: &mut impl FnMut() -> bool) -> Result<String> {
    let mut remaining = snapshot.length();
    let mut stream = snapshot.checked_stream();
    let mut bytes = [0; 65536];
    let mut hash = Sha256::new();
    while remaining != 0 {
        checkpoint(check)?;
        let count = remaining.min(bytes.len() as u64) as usize;
        stream
            .read_exact(&mut bytes[..count])
            .map_err(|_| E::Read)?;
        checkpoint(check)?;
        hash.update(&bytes[..count]);
        remaining -= count as u64;
    }
    Ok(hash
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

/// Verify a bounded local generator export against explicit caller selections.
/// Stored layers are streamed; no compressor, engine, loader or helper executes.
pub fn verify_generator_archive(
    path: &Path,
    selection: &GeneratorSelection,
) -> Result<GeneratorIdentity> {
    verify_generator_archive_with_checkpoint(path, selection, || true)
}

/// The same verifier with synchronous admission checkpoints at hash chunks,
/// headers and graph steps. False refuses with `archive_budget_exhausted`; the
/// owner must preserve its already-known timeout/signal as the primary status.
/// Checkpoints are not a hard wall-clock bound on filesystem/reap/scheduling.
pub fn verify_generator_archive_with_checkpoint(
    path: &Path,
    selection: &GeneratorSelection,
    mut check: impl FnMut() -> bool,
) -> Result<GeneratorIdentity> {
    checkpoint(&mut check)?;
    let mut snapshot = Snapshot::open(path, super::MAX_ARCHIVE_BYTES)?;
    let result: Result<GeneratorIdentity> = (|| {
        let archive_sha256 = transfer_hash(&mut snapshot, &mut check)?;
        snapshot.verify()?;
        checkpoint(&mut check)?;
        snapshot.file_mut().rewind().map_err(|_| E::Read)?;
        let length = snapshot.length();
        let index = Index::read_with_checkpoint(
            &mut snapshot.checked_stream(),
            length,
            Limits::default(),
            &mut check,
        )?;
        index.generator_profile(&mut snapshot.checked_stream(), length, &mut check)?;
        let mut stream = snapshot.checked_stream();
        let mut content = Content {
            file: &mut stream,
            index: &index,
            check: &mut check,
            hashes: BTreeMap::new(),
        };
        let (format, config_sha256, selected_root_digest, layers) =
            validate(&mut content, selection)?;
        checkpoint(&mut check)?;
        Ok(GeneratorIdentity {
            archive_sha256,
            archive_bytes: length,
            observed_image_id: selection.image_id.clone(),
            config_sha256,
            selected_root_digest,
            format,
            layers,
            platform: "linux/amd64",
            image_tag: TAG,
            revision: selection.revision.clone(),
        })
    })();
    let closed = snapshot.finish();
    let identity = result?;
    closed?;
    checkpoint(&mut check)?;
    Ok(identity)
}

#[cfg(test)]
#[path = "generator_tests.rs"]
mod tests;
