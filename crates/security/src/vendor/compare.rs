//! A vendored tree against its upstream archive, byte for byte, and the
//! coverage of the whole vendored directory by the record.

use super::{
    Verdict, diff, listed,
    manifest::{self, Manifest, Tree, unread},
    problem, wrap,
};
use crate::{digest, document::Document};
use std::collections::{BTreeMap, BTreeSet};

/// The most bytes of differences that are kept.
pub(super) const MAX_DIFF: usize = 64 * 1024 * 1024;

/// Files by their paths, in the order of their paths.
pub type Files = BTreeMap<String, Vec<u8>>;

/// What the comparison of one tree gives: the digests of every file on both
/// sides, every difference whole, and the deviations that the record does
/// not account for.
#[derive(Clone, Debug, PartialEq)]
pub struct Compared {
    /// The tree, its source and every file's digests.
    pub report: Document,
    /// Every difference.
    pub diff: Vec<u8>,
    /// What the record does not account for, and what it allows that is
    /// not there.
    pub errors: Vec<String>,
}

/// Compare every file of `tree`: what `upstream` holds below the tree's
/// prefix with what is vendored in `local`. A deviation must be declared
/// with both digests, and a declared one must be there.
pub fn tree(tree: &Tree, upstream: &Files, local: &Files) -> Verdict<Compared> {
    let prefix = format!("{}/", tree.prefix);
    if upstream.keys().any(|path| !path.starts_with(&prefix)) {
        return Err(problem(format!(
            "Archive contains files outside its declared prefix: {}",
            tree.source
        )));
    }
    if upstream.is_empty() {
        return Err(problem(format!("Empty upstream tree: {}", tree.source)));
    }
    let original: BTreeMap<&str, &Vec<u8>> = upstream
        .iter()
        .map(|(path, body)| (&path[prefix.len()..], body))
        .collect();
    let paths: BTreeSet<&str> = original
        .keys()
        .copied()
        .chain(local.keys().map(String::as_str))
        .collect();
    let mut records = Vec::new();
    let mut differences = Vec::new();
    let mut actual: BTreeMap<&str, (Option<String>, Option<String>)> = BTreeMap::new();
    for path in paths {
        let (old, new) = (original.get(path).copied(), local.get(path));
        let (before, after) = (old.map(|body| digest(body)), new.map(|body| digest(body)));
        records.push(Document::record([
            ("path", path.into()),
            ("upstream_sha256", before.clone().into()),
            ("vendored_sha256", after.clone().into()),
        ]));
        if old == new {
            continue;
        }
        actual.insert(path, (before, after));
        let whole = format!("{}/{path}", tree.path);
        let rendered = diff::rendered(&whole, old.map(Vec::as_slice), new.map(Vec::as_slice));
        if differences.len() + rendered.len() > MAX_DIFF {
            return Err(problem("Vendor diff budget exceeded"));
        }
        differences.extend(rendered);
    }
    let declared: BTreeMap<&str, (Option<String>, Option<String>)> = tree
        .changes
        .iter()
        .map(|change| {
            (
                change.path.as_str(),
                (change.upstream.clone(), change.vendored.clone()),
            )
        })
        .collect();
    let named: BTreeSet<&str> = actual.keys().chain(declared.keys()).copied().collect();
    let stale = named
        .into_iter()
        .filter(|path| actual.get(path) != declared.get(path));
    let errors = stale
        .map(|path| format!("Unlisted or stale deviation: {}/{path}", tree.path))
        .collect();
    let report = Document::record([
        ("path", tree.path.as_str().into()),
        ("source", tree.source.as_str().into()),
        ("files", records.into()),
    ]);
    Ok(Compared {
        report,
        diff: differences,
        errors,
    })
}

/// Hold the vendored files to the record: the native component metadata
/// has its fields, every file is a tree's, a copied one, a maintained one
/// or the record itself, and the wrap files are the record's wraps, each
/// as the record pins it.
pub fn coverage(manifest: &Manifest, local: &Files) -> Verdict<()> {
    let components = "vendor/native-components.json";
    let missing = |path: &str| {
        problem(format!(
            "Unlisted or missing vendor files: {}",
            listed(&[path])
        ))
    };
    if manifest
        .maintained_files
        .iter()
        .any(|path| path == components)
    {
        let body = local.get(components).ok_or_else(|| missing(components))?;
        let document = Document::read(body).map_err(unread)?;
        let names = [
            "wrap_components",
            "adapted_component",
            "openssl",
            "registry_component",
            "native_links",
        ];
        let metadata = manifest::fields(&document, &names)?;
        let wrapped = metadata.get(names[0]).and_then(Document::items);
        for record in wrapped.ok_or_else(|| problem("Expected a JSON array"))? {
            manifest::fields(
                record,
                &["source", "name", "version", "linkage", "usage", "license"],
            )?;
        }
        for name in &names[1..] {
            let held = metadata.get(name).and_then(Document::fields);
            held.ok_or_else(|| problem("Expected a JSON object"))?;
        }
    }
    let in_tree = |path: &str| {
        let below = |tree: &Tree| {
            path.strip_prefix(tree.path.as_str())
                .is_some_and(|rest| rest.starts_with('/'))
        };
        manifest.trees.iter().any(below)
    };
    let mut covered: BTreeSet<&str> = manifest
        .files
        .iter()
        .map(|(path, _)| path.as_str())
        .collect();
    covered.extend(manifest.maintained_files.iter().map(String::as_str));
    covered.insert("vendor/integrity.json");
    covered.extend(
        local
            .keys()
            .map(String::as_str)
            .filter(|path| in_tree(path)),
    );
    let held: BTreeSet<&str> = local.keys().map(String::as_str).collect();
    if covered != held {
        let odd: Vec<&str> = covered.symmetric_difference(&held).copied().collect();
        return Err(problem(format!(
            "Unlisted or missing vendor files: {}",
            listed(&odd)
        )));
    }
    let declared: BTreeSet<&str> = manifest
        .wraps
        .iter()
        .map(|wrap| wrap.path.as_str())
        .collect();
    let found: BTreeSet<&str> = held
        .iter()
        .copied()
        .filter(|path| path.ends_with(".wrap"))
        .collect();
    if declared != found {
        return Err(problem("Unlisted or missing native wraps"));
    }
    for declared in &manifest.wraps {
        let text = local.get(&declared.path).map_or(&[][..], Vec::as_slice);
        wrap::validate(declared, manifest, text, local.keys().map(String::as_str))?;
    }
    Ok(())
}
