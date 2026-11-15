//! The receipt of the vendor verification: before any native identity of
//! the vendored sources is used, the receipt must be the one of the record
//! as it is now, and every file it speaks of must be the file that is
//! there now.

use super::{field, object, part, problem, read, text};
use crate::{
    digest,
    document::Document,
    vendor::{
        Bounds, Problem, Verdict,
        compare::{self, Files},
        manifest::{self, Manifest, Tree},
        tree,
    },
};
use std::{collections::BTreeSet, path::Path};

/// The most bytes a receipt may have.
const MAX_REPORT: u64 = 8 * 1024 * 1024;

/// The digest of a file that is vendored now.
fn current(local: &Files, path: &str) -> Verdict<String> {
    let body = local.get(path);
    let body = body.ok_or_else(|| Problem(format!("Missing vendor file: {path}")))?;
    Ok(digest(body))
}

/// The records of a list, each held to be one.
fn records(value: &Document) -> Verdict<Vec<&Document>> {
    manifest::array(value)?.iter().map(object).collect()
}

/// The paths that `records` name, none of them twice.
fn paths<'a>(records: &[&'a Document], name: &str, label: &str) -> Verdict<Vec<&'a str>> {
    let mut named = Vec::new();
    for record in records {
        named.push(manifest::string(field(record, name)?)?);
    }
    manifest::unique(named.iter().copied(), label)?;
    Ok(named)
}

/// Hold what the receipt says of one tree to the tree of the record and
/// to the files that are vendored now: every file that it says is there
/// has the bytes that are there, and what it says differs from upstream
/// is exactly what the record declares.
fn validate_receipt_tree(tree: &Tree, report: &Document, local: &Files) -> Verdict<()> {
    if report.get("source").and_then(Document::text) != Some(&tree.source) {
        return Err(problem("Vendor receipt tree source differs"));
    }
    let names = ["path", "upstream_sha256", "vendored_sha256"];
    let mut members = Vec::new();
    for member in manifest::array(field(report, "files")?)? {
        members.push(manifest::fields(member, &names)?);
    }
    paths(&members, "path", "receipt tree member")?;
    let below = format!("{}/", tree.path);
    let expected: BTreeSet<&str> = local
        .keys()
        .filter_map(|path| path.strip_prefix(&below))
        .collect();
    let declared: BTreeSet<&str> = tree
        .changes
        .iter()
        .map(|change| change.path.as_str())
        .collect();
    let (mut present, mut differences) = (BTreeSet::new(), BTreeSet::new());
    for member in members {
        let path = manifest::relative_path(text(member, "path")?)?;
        let (old, new) = (
            part(member, "upstream_sha256"),
            part(member, "vendored_sha256"),
        );
        if *old != Document::Null {
            manifest::digest(old)?;
        }
        if *new != Document::Null {
            present.insert(path);
            if new.text() != Some(&current(local, &format!("{below}{path}"))?) {
                return Err(problem("Vendor receipt tree bytes differ"));
            }
        }
        if old != new {
            differences.insert(path);
            let change = tree.changes.iter().find(|change| change.path == path);
            let reviewed = change.is_some_and(|change| {
                old.text() == change.upstream.as_deref() && new.text() == change.vendored.as_deref()
            });
            if !reviewed {
                return Err(problem("Vendor receipt deviation differs"));
            }
        }
    }
    if present != expected || differences != declared {
        return Err(problem("Vendor receipt member coverage differs"));
    }
    Ok(())
}

/// Hold the copied and the maintained files of the receipt to the record
/// and to the files that are vendored now.
fn validate_receipt_files(integrity: &Manifest, report: &Document, local: &Files) -> Verdict<()> {
    let copied: BTreeSet<&str> = integrity
        .files
        .iter()
        .map(|(path, _)| path.as_str())
        .collect();
    let maintained: BTreeSet<&str> = integrity
        .maintained_files
        .iter()
        .map(String::as_str)
        .collect();
    for (name, expected) in [("files", copied), ("maintained_files", maintained)] {
        let listed = records(part(report, name))?;
        let named = paths(&listed, "path", "receipt file")?;
        if named.iter().copied().collect::<BTreeSet<&str>>() != expected {
            return Err(problem("Vendor receipt file coverage differs"));
        }
        for (record, path) in listed.iter().zip(named) {
            if record.get("sha256").and_then(Document::text) != Some(&current(local, path)?) {
                return Err(problem("Vendor receipt current file differs"));
            }
        }
    }
    Ok(())
}

/// Hold the receipt at `receipt` to the checkout `root` and to the record
/// `integrity` of its vendored sources, and answer the receipt's digest.
pub fn validate_vendor_receipt(
    root: &Path,
    receipt: &Path,
    integrity: &Manifest,
) -> Verdict<String> {
    let bounds = Bounds::REVIEWED;
    let data = tree::read_regular(receipt, MAX_REPORT)?;
    let report = Document::read(&data).map_err(manifest::unread)?;
    let names = [
        "manifest_sha256",
        "diff_sha256",
        "sources",
        "trees",
        "files",
        "maintained_files",
    ];
    let report = manifest::fields(&report, &names)?;
    let said = |name: &str| part(report, name).text().map(str::to_owned);
    if said("manifest_sha256") != Some(digest(&read(&root.join("vendor/integrity.json"))?)) {
        return Err(problem("Vendor receipt manifest differs"));
    }
    let differences = receipt.parent().unwrap_or(receipt).join("vendor.diff");
    let differences = tree::read_regular(&differences, bounds.differences as u64)?;
    if said("diff_sha256") != Some(digest(&differences)) {
        return Err(problem("Vendor receipt diff differs"));
    }
    let sources = records(part(report, "sources"))?;
    let named = paths(&sources, "id", "receipt source")?;
    let pinned: BTreeSet<&str> = integrity
        .sources
        .iter()
        .map(|source| source.identifier.as_str())
        .collect();
    if named.iter().copied().collect::<BTreeSet<&str>>() != pinned {
        return Err(problem("Vendor receipt source coverage differs"));
    }
    for (record, name) in sources.iter().zip(named) {
        let held = |field: &str| record.get(field).and_then(Document::text);
        let same = integrity.source(name).is_some_and(|source| {
            held("url") == Some(&source.url)
                && held("sha256") == Some(&source.sha256)
                && held("format") == Some(&source.format)
        });
        if !same {
            return Err(problem("Vendor receipt upstream source differs"));
        }
    }
    let local: Files = tree::files(&root.join("vendor"), &bounds)?
        .into_iter()
        .map(|(path, body)| (format!("vendor/{path}"), body))
        .collect();
    compare::coverage(integrity, &local)?;
    let reports = records(part(report, "trees"))?;
    let mut reported = Vec::new();
    for report in &reports {
        reported.push(manifest::string(field(report, "path")?)?);
    }
    if reported
        != integrity
            .trees
            .iter()
            .map(|tree| tree.path.as_str())
            .collect::<Vec<&str>>()
    {
        return Err(problem("Vendor receipt tree coverage differs"));
    }
    for (tree, report) in integrity.trees.iter().zip(reports) {
        validate_receipt_tree(tree, report, &local)?;
    }
    validate_receipt_files(integrity, report, &local)?;
    Ok(digest(&data))
}

#[cfg(test)]
#[path = "receipt_tests.rs"]
mod tests;
