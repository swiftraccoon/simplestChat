//! The unchanged wrapper crate of the external library: what Cargo
//! unpacked and built is the archive of the reviewed record, byte for
//! byte, and the C source it bundles is named though it is not what is
//! linked.

use super::{
    Build, SOURCE_REGISTRY,
    build::{CargoBuild, beneath, resolved},
    field, object, problem, reviewed_license, revision, table,
};
use crate::{
    document::Document,
    vendor::{Bounds, Problem, Verdict, archive, compare::Files, manifest, tree},
};
use std::path::Path;

fn text<'a>(record: &'a Document, name: &str) -> Verdict<&'a str> {
    manifest::string(field(record, name)?)
}

/// The file `name` of an authenticated crate.
fn member<'a>(files: &'a Files, name: &str) -> Verdict<&'a [u8]> {
    let held = files.get(name).map(Vec::as_slice);
    held.ok_or_else(|| Problem(format!("Missing crate file: {name}")))
}

/// The files of the crate archive at `path` that `source` pins, by their
/// paths below the directory of its name, or `differs` is what is wrong.
pub(super) fn crate_files(source: &manifest::Source, path: &Path, differs: &str) -> Verdict<Files> {
    let bounds = Bounds::REVIEWED;
    let data = tree::read_regular(path, bounds.source as u64)?;
    let files = archive::files(source, &data, &bounds)?;
    let prefix = format!("{}/", source.identifier);
    let whole = files.len();
    let below =
        |(name, body): (String, Vec<u8>)| Some((name.strip_prefix(&prefix)?.to_owned(), body));
    let files: Files = files.into_iter().filter_map(below).collect();
    if files.len() != whole {
        return Err(problem(differs));
    }
    Ok(files)
}

/// The version that the bundled header says: the first line that is
/// exactly the definition of the version, of digits and dots.
fn bundled_version(header: &str) -> Option<&str> {
    fn version(line: &str) -> Option<&str> {
        let said = line.strip_prefix("#define AWSLC_VERSION_NUMBER_STRING \"")?;
        let said = said.strip_suffix('"')?;
        let plain = |byte: u8| byte.is_ascii_digit() || byte == b'.';
        (!said.is_empty() && said.bytes().all(plain)).then_some(said)
    }
    header.split('\n').find_map(version)
}

/// The evidence of the wrapper crate that the reviewed record `value`
/// names: the build `graph` built it and ran its build script, and what
/// was unpacked for the build is its authenticated archive.
pub fn registry_component(
    build: &Build<'_>,
    graph: &CargoBuild,
    value: &Document,
) -> Verdict<Document> {
    let component = object(value)?;
    let (name, version) = (text(component, "crate")?, text(component, "crate_version")?);
    let identifier = format!("{SOURCE_REGISTRY}#{name}@{version}");
    let built = graph.artifacts.get(&identifier);
    let built = built.filter(|_| {
        graph
            .scripts
            .keys()
            .any(|(package, _)| *package == identifier)
    });
    let built = built.ok_or_else(|| {
        problem("Declared native wrapper crate is absent from the production build")
    })?;
    let source = manifest::source(field(component, "source")?)?;
    let registry = build.cargo_home.join("registry/src");
    let unpacked = beneath(Path::new(text(built, "manifest_path")?), &registry)?;
    let directory = unpacked.parent().unwrap_or(&unpacked);
    let index = directory.parent().unwrap_or(directory);
    let named = directory.file_name().and_then(|name| name.to_str()) == Some(&source.identifier);
    if !named || index.parent() != Some(&resolved(&registry)) {
        return Err(problem("Native registry source directory differs"));
    }
    let cache = build.cargo_home.join("registry/cache");
    let packed = index.file_name().map(|index| cache.join(index));
    let packed = packed
        .unwrap_or(cache)
        .join(format!("{}.crate", source.identifier));
    let original = crate_files(&source, &packed, "Native crate archive prefix differs")?;
    let mut local = tree::files(directory, &Bounds::REVIEWED)?;
    let marked = local.remove(".cargo-ok").as_deref() == Some(br#"{"v":1}"#);
    if !marked || original != local {
        return Err(problem(
            "Bundled native source differs from authenticated crate bytes",
        ));
    }
    let metadata = table(member(&original, "Cargo.toml.orig")?, "Cargo.toml.orig")?;
    let package = object(field(&metadata, "package")?)?;
    let declared = object(field(object(field(package, "metadata")?)?, "aws-lc-sys")?)?;
    let header = std::str::from_utf8(member(&original, "aws-lc/include/openssl/base.h")?);
    let header = header.map_err(|_| problem("No text: aws-lc/include/openssl/base.h"))?;
    let unnamed = || problem("Authenticated AWS-LC wrapper lacks its bundled source identity");
    let bundled = bundled_version(header).ok_or_else(unnamed)?;
    if !revision(text(declared, "commit-hash")?) {
        return Err(unnamed());
    }
    let packaged = table(member(&original, "Cargo.toml")?, "Cargo.toml")?;
    let packaged = object(field(&packaged, "package")?)?;
    if packaged.get("license").and_then(Document::text) != Some(reviewed_license(component)?) {
        return Err(problem(
            "Bundled AWS-LC license differs from authenticated source",
        ));
    }
    let unused = Document::record([
        ("version", bundled.into()),
        ("revision", field(declared, "commit-hash")?.clone()),
    ]);
    Ok(Document::record([
        ("name", field(component, "name")?.clone()),
        ("version", field(component, "version")?.clone()),
        ("revision", field(component, "revision")?.clone()),
        ("crate", name.into()),
        ("crate_version", version.into()),
        ("source_sha256", source.sha256.as_str().into()),
        ("verified_files", original.len().into()),
        ("license", field(component, "license")?.clone()),
        ("unused_bundled_source", unused),
    ]))
}

#[cfg(test)]
#[path = "registry_tests.rs"]
mod tests;
