//! A fresh private signer input graph. The exact three subjects are original
//! assembly bytes; neither this admission nor an inventory performs signing.

use super::super::namespace::{self, Budget, Chain, Held};
use super::{catalog::Catalog, context::Context, failure, provenance};
use crate::tooling::Result;
use rustix::fs::{AtFlags, FileType};
use sha2::{Digest, Sha256};
use simplestchat_ops::distribution::manifest::{
    assembly::{EvidenceSources, prepare_artifact},
    verification::RetainedMetadata,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsStr,
    io::Read,
    os::unix::fs::MetadataExt,
    path::Path,
};

fn inventory(directory: &Chain, budget: &Budget) -> Result<BTreeSet<String>> {
    let _permit = budget.permit()?;
    let cursor = rustix::fs::Dir::read_from(directory.file()).map_err(|_| failure(1))?;
    let mut result = BTreeSet::new();
    for entry in cursor {
        let entry = entry.map_err(|_| failure(1))?;
        let name = entry.file_name().to_str().map_err(|_| failure(1))?;
        if !matches!(name, "." | "..")
            && (name.len() > 128 || result.len() >= 22 || !result.insert(name.to_owned()))
        {
            return Err(failure(1));
        }
    }
    Ok(result)
}
fn same_number<T: TryInto<u64>>(value: T, expected: u64) -> bool {
    value.try_into().ok() == Some(expected)
}

fn hash_file(
    directory: &Chain,
    name: &str,
    expected: (&str, u64),
    budget: &Budget,
    checkpoint: &mut impl FnMut() -> Result<()>,
) -> Result<()> {
    checkpoint()?;
    let admitted = rustix::fs::statat(directory.file(), name, AtFlags::SYMLINK_NOFOLLOW)
        .map_err(|_| failure(1))?;
    if FileType::from_raw_mode(admitted.st_mode) != FileType::RegularFile {
        return Err(failure(1));
    }
    let file = Held::open(directory.file(), OsStr::new(name), namespace::FLAGS, budget)?;
    let result = (|| {
        let original = file.file().metadata().map_err(|_| failure(1))?;
        if !same_number(admitted.st_dev, original.dev())
            || !same_number(admitted.st_ino, original.ino())
            || !original.is_file()
            || original.len() != expected.1
            || original.nlink() != 1
            || original.uid() != rustix::process::getuid().as_raw()
            || original.mode() & 0o7022 != 0
        {
            return Err(failure(1));
        }
        let mut reader = file.file();
        let mut hash = Sha256::new();
        let mut count = 0u64;
        let mut block = [0u8; 65536];
        while count < expected.1 {
            checkpoint()?;
            let limit = (expected.1 - count).min(block.len() as u64) as usize;
            let n = reader.read(&mut block[..limit]).map_err(|_| failure(1))?;
            if n == 0 {
                return Err(failure(1));
            }
            hash.update(&block[..n]);
            count += n as u64;
        }
        if super::super::hex(&hash.finalize()) != expected.0
            || !namespace::same(&original, &file.file().metadata().map_err(|_| failure(1))?)
        {
            return Err(failure(1));
        }
        let visible = Held::open(directory.file(), OsStr::new(name), namespace::FLAGS, budget)?;
        let checked = visible
            .file()
            .metadata()
            .map_err(|_| failure(1))
            .and_then(|stamp| {
                if namespace::same(&original, &stamp) {
                    Ok(())
                } else {
                    Err(failure(1))
                }
            });
        checked.and(visible.finish())?;
        directory.verify()?;
        checkpoint()
    })();
    result.and(file.finish())
}

pub(super) fn prepare(
    context: &Context,
    directory: &Path,
    output: &Path,
    budget: &Budget,
    checkpoint: &mut impl FnMut() -> Result<()>,
) -> Result<Vec<u8>> {
    let chain = Chain::open(directory, budget)?;
    let result = (|| {
        checkpoint()?;
        let selection = context.selection()?;
        let manifest = namespace::read(&directory.join("ops-release.json"), 65536, budget)?;
        let index = namespace::read(&directory.join("ops-evidence.json"), 65536, budget)?;
        let predicate = namespace::read(&directory.join("ops-predicate.json"), 65536, budget)?;
        let preparation = namespace::read(&directory.join("ops-preparation.json"), 65536, budget)?;
        let retained =
            RetainedMetadata::decode(&selection, &manifest, &index, &predicate, &preparation)
                .map_err(|_| failure(1))?;
        let parent = Chain::open(output.parent().ok_or_else(|| failure(1))?, budget)?;
        let parent_result = (|| {
            if chain.witness()?.leaf_in(&parent.witness()?) {
                Err(failure(1))
            } else {
                Ok(())
            }
        })();
        parent_result.and(parent.finish())?;
        let expected = retained
            .payloads()
            .iter()
            .map(|p| p.name().to_owned())
            .collect::<BTreeSet<_>>();
        if inventory(&chain, budget)? != expected {
            return Err(failure(1));
        }
        for payload in retained.payloads() {
            hash_file(
                &chain,
                payload.name(),
                (payload.sha256(), payload.bytes()),
                budget,
                checkpoint,
            )?;
        }
        let indexed: serde_json::Value = serde_json::from_slice(&index).map_err(|_| failure(1))?;
        let mut sources = BTreeMap::new();
        let mut documents = BTreeMap::new();
        for entry in indexed["entries"].as_array().ok_or_else(|| failure(1))? {
            let logical = entry["logicalName"].as_str().ok_or_else(|| failure(1))?;
            let name = entry["payload"].as_str().ok_or_else(|| failure(1))?;
            let payload = retained
                .payloads()
                .iter()
                .find(|p| p.name() == name)
                .ok_or_else(|| failure(1))?;
            let bytes = namespace::read(&directory.join(name), 1024 * 1024, budget)?;
            if bytes.len() as u64 != payload.bytes()
                || super::super::sha(&bytes) != payload.sha256()
            {
                return Err(failure(1));
            }
            documents.insert(logical.to_owned(), bytes);
            sources.insert(logical.to_owned(), directory.join(name));
        }
        let manifest_value = serde_json::from_slice(&manifest).map_err(|_| failure(1))?;
        provenance::check(context, &manifest_value, &documents, &Catalog::load()?)?;
        checkpoint()?;
        let prepared = {
            let _helper = budget.reserve(100)?;
            let pair = simplestchat_ops::distribution::manifest::verify_pair(
                retained.manifest(),
                &directory.join("simplestchat-ops"),
                &selection,
            )
            .map_err(|_| failure(1))?;
            retained.check_pair(&pair).map_err(|_| failure(1))?;
            prepare_artifact(
                retained.manifest(),
                &directory.join("simplestchat-ops"),
                &selection,
                &EvidenceSources::new(sources).map_err(|_| failure(1))?,
                output,
            )
            .map_err(|_| failure(1))?
        };
        // Reassembly must retain exact original signer inputs. Independently
        // stream the executable; no binary-sized readback allocation is needed.
        let copied = Chain::open(output, budget)?;
        let checked = (|| {
            for payload in retained.payloads() {
                hash_file(
                    &copied,
                    payload.name(),
                    (payload.sha256(), payload.bytes()),
                    budget,
                    checkpoint,
                )?;
                hash_file(
                    &chain,
                    payload.name(),
                    (payload.sha256(), payload.bytes()),
                    budget,
                    checkpoint,
                )?;
            }
            if inventory(&copied, budget)? != expected {
                return Err(failure(1));
            }
            checkpoint()
        })();
        checked.and(copied.finish())?;
        if inventory(&chain, budget)? != expected {
            return Err(failure(1));
        }
        provenance::encoded(&serde_json::json!({
            "schemaVersion":1,"kind":"ops-signing-input-observation",
            "selection":selection.identity(),"artifact":prepared,"subjectCount":3,
            "artifactName":context.artifact_name(super::context::ArtifactKind::Signed),
            "scope":"preparedSignerInputsNotCryptographicVerificationOrRuntimeAuthority"
        }))
    })();
    let closed = chain.finish();
    result.and_then(|value| closed.map(|()| value))
}
