//! The licence that every Rust package of the build declares, read only
//! from sources that are authenticated: a crates.io archive that the lock
//! pins, a vendored tree of the record, or the checkout's own manifest.
//! The packages are those that Cargo said it compiled together with those
//! that the server's own graph names.

use super::{
    Build, SOURCE_REGISTRY,
    build::{CargoBuild, beneath, package_name, resolved},
    field, object, problem, read,
    registry::crate_files,
    table, toml,
};
use crate::{
    digest,
    document::Document,
    vendor::{
        Bounds, Problem, Verdict,
        compare::Files,
        manifest::{self, Manifest, Source},
        tree,
    },
};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

/// The most records of licences there may be.
const MAX_RECORDS: usize = 20000;
/// The most characters a licence expression may have.
const MAX_LICENSE: usize = 4096;

/// A package by its name, its version and where it comes from.
type Identity = (String, String, String);

fn text<'a>(record: &'a Document, name: &str) -> Verdict<&'a str> {
    manifest::string(field(record, name)?)
}

fn held<'a>(record: &'a Document, name: &str) -> Option<&'a str> {
    record.get(name).and_then(Document::text)
}

/// Whether the reference's interpreter takes a character for space.
pub(super) fn space(character: char) -> bool {
    character.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&character)
}

/// What a package declares of its licence: the expression as upstream
/// wrote it, and its licence file by its path and its digest. Nothing is
/// inferred.
pub fn package_license(package: &Document, files: &Files) -> Verdict<Document> {
    let expression = package.get("license").unwrap_or(&Document::Null);
    let written = |text: &str| !text.chars().all(space) && text.chars().count() <= MAX_LICENSE;
    if *expression != Document::Null && !expression.text().is_some_and(written) {
        return Err(problem("Invalid upstream license expression"));
    }
    let mut file = Document::Null;
    let reference = package.get("license-file").unwrap_or(&Document::Null);
    if *reference != Document::Null {
        let path = manifest::relative_path(manifest::string(reference)?)?;
        let body = files
            .get(path)
            .ok_or_else(|| problem("Declared license file is absent from authenticated source"))?;
        file = Document::record([("path", path.into()), ("sha256", digest(body).into())]);
    }
    Ok(Document::record([
        ("expression", expression.clone()),
        ("license_file", file),
    ]))
}

/// The package of the lock entry `locked`, which whoever asks has held
/// to come from crates.io, as its archive at `archive` declares it, with
/// the files of that archive.
fn registry_archive_license(archive: &Path, locked: &Document) -> Verdict<(Document, Files)> {
    let (name, version) = (text(locked, "name")?, text(locked, "version")?);
    let identifier = format!("{name}-{version}");
    let source = Source {
        url: format!("https://static.crates.io/crates/{name}/{identifier}.crate"),
        sha256: manifest::digest(field(locked, "checksum")?)?.to_owned(),
        format: "tar.gz".to_owned(),
        identifier,
    };
    let files = crate_files(&source, archive, "Rust license archive prefix differs")?;
    let package = files.get("Cargo.toml");
    let package = package.ok_or_else(|| problem("Missing crate file: Cargo.toml"))?;
    let package = table(package, "Cargo.toml")?;
    Ok((object(field(&package, "package")?)?.clone(), files))
}

/// The package that Cargo built from the unpacked manifest `path`, as the
/// archive that the lock pins declares it: what was unpacked is not read.
fn registry_license_source(
    build: &Build<'_>,
    path: &Path,
    locked: &Document,
) -> Verdict<(Document, Files, PathBuf)> {
    let (name, version) = (text(locked, "name")?, text(locked, "version")?);
    if held(locked, "source") != Some(SOURCE_REGISTRY) {
        return Err(problem(
            "Rust license provenance requires a pinned crates.io archive",
        ));
    }
    let registry = build.cargo_home.join("registry/src");
    let unpacked = beneath(path, &registry)?;
    let directory = unpacked.parent().unwrap_or(&unpacked);
    let index = directory.parent().unwrap_or(directory);
    let identifier = format!("{name}-{version}");
    let named = directory.file_name().and_then(|name| name.to_str()) == Some(&identifier);
    if !named || index.parent() != Some(&resolved(&registry)) {
        return Err(problem("Rust license source directory differs"));
    }
    let cache = build.cargo_home.join("registry/cache");
    let cache = index
        .file_name()
        .map_or(cache.clone(), |index| cache.join(index));
    let (package, files) =
        registry_archive_license(&cache.join(format!("{identifier}.crate")), locked)?;
    Ok((package, files, cache))
}

/// A package with every field that it takes from the workspace filled in
/// from the root manifest.
pub fn inherit_workspace_package(package: &Document, root: &Document) -> Verdict<Document> {
    let empty = Document::Record(Vec::new());
    let workspace = object(root.get("workspace").unwrap_or(&empty))?;
    let shared = object(workspace.get("package").unwrap_or(&empty))?;
    let inherited = Document::record([("workspace", true.into())]);
    let mut resolved = Vec::new();
    for (name, value) in package.fields().unwrap_or_default() {
        let value = if *value == inherited {
            let declared = shared.get(name);
            declared.ok_or_else(|| {
                Problem(format!(
                    "Workspace package field is not declared by the root manifest: {name}"
                ))
            })?
        } else {
            value
        };
        resolved.push((name.clone(), value.clone()));
    }
    Ok(Document::Record(resolved))
}

/// A package of the checkout itself: the server by its own manifest, or a
/// vendored package by a tree of the record. Its files and the digest of
/// its source are answered with it.
fn local_license_source(
    build: &Build<'_>,
    integrity: &Manifest,
    path: &Path,
    locked: &Document,
) -> Verdict<(Document, Files, Document)> {
    if locked
        .get("source")
        .is_some_and(|source| *source != Document::Null)
    {
        return Err(problem("Unsupported non-registry Rust source"));
    }
    let path = beneath(path, build.root)?;
    let own = build.root.join("Cargo.toml");
    let manifest = toml(&path)?;
    let mut package = object(field(&manifest, "package")?)?.clone();
    let member = path.parent().and_then(Path::parent) == Some(build.root);
    if path == own || member {
        package = inherit_workspace_package(&package, &toml(&own)?)?;
    }
    if path == own {
        let mut files = Files::new();
        files.insert("Cargo.toml".to_owned(), read(&path)?);
        let reference = package.get("license-file").unwrap_or(&Document::Null);
        if *reference != Document::Null {
            let name = manifest::relative_path(manifest::string(reference)?)?;
            let licence = beneath(&build.root.join(name), build.root)?;
            files.insert(name.to_owned(), read(&licence)?);
        }
        return Ok((package, files, Document::Null));
    }
    let vendored = |tree: &&manifest::Tree| path == build.root.join(&tree.path).join("Cargo.toml");
    let trees: Vec<&manifest::Tree> = integrity.trees.iter().filter(vendored).collect();
    let [tree] = trees[..] else {
        return Err(problem("Unlisted local Rust package license source"));
    };
    let source = integrity.source(&tree.source);
    let source = source.ok_or_else(|| problem("Unlisted local Rust package license source"))?;
    let directory = path.parent().unwrap_or(&path);
    let files = tree::files(directory, &Bounds::REVIEWED)?;
    Ok((package, files, source.sha256.as_str().into()))
}

/// The entries of the lock for the package `name` of `version`.
fn locked<'a>(packages: &[&'a Document], name: &str, version: &str) -> Vec<&'a Document> {
    let named = |package: &&&Document| {
        held(package, "name") == Some(name) && held(package, "version") == Some(version)
    };
    packages.iter().filter(named).copied().collect()
}

/// The digest of the manifest of an authenticated package.
fn manifest_digest(files: &Files) -> Verdict<Document> {
    let manifest = files.get("Cargo.toml");
    let manifest = manifest.ok_or_else(|| problem("Missing crate file: Cargo.toml"))?;
    Ok(digest(manifest).into())
}

/// The records of the packages that only the server's graph names: each
/// from the one index that the build proved, by its entry of the lock.
fn embedded_registry_licenses(
    packages: &[&Document],
    identities: &BTreeSet<Identity>,
    indexes: &BTreeSet<PathBuf>,
) -> Verdict<Vec<Document>> {
    let mut records = Vec::new();
    for (name, version, kind) in identities {
        let [cache] = Vec::from_iter(indexes)[..] else {
            return Err(problem(
                "Embedded-only package requires one proven crates.io cache",
            ));
        };
        if kind != "crates.io" {
            return Err(problem(
                "Embedded-only package requires one proven crates.io cache",
            ));
        }
        let entries = locked(packages, name, version);
        let [entry] = entries[..] else {
            return Err(problem(
                "Embedded Rust package is missing or ambiguous in Cargo.lock",
            ));
        };
        if held(entry, "source") != Some(SOURCE_REGISTRY) {
            return Err(problem(
                "Embedded Rust package is missing or ambiguous in Cargo.lock",
            ));
        }
        let archive = cache.join(format!("{name}-{version}.crate"));
        let (package, files) = registry_archive_license(&archive, entry)?;
        if held(&package, "name") != Some(name) || held(&package, "version") != Some(version) {
            return Err(problem(
                "Embedded package identity differs from authenticated metadata",
            ));
        }
        let evidence = Document::record([
            ("compilerArtifact", false.into()),
            ("embeddedMetadata", true.into()),
        ]);
        records.push(Document::record([
            (
                "package_id",
                format!("{SOURCE_REGISTRY}#{name}@{version}").into(),
            ),
            ("name", name.as_str().into()),
            ("version", version.as_str().into()),
            ("source", kind.as_str().into()),
            ("source_sha256", field(entry, "checksum")?.clone()),
            ("manifest_sha256", manifest_digest(&files)?),
            ("first_party", false.into()),
            ("evidence", evidence),
            ("license", package_license(&package, &files)?),
        ]));
    }
    Ok(records)
}

/// The licence evidence of every Rust package of the build: those that
/// Cargo said it compiled, in the order of their identifiers, and then
/// those that only the graph `embedded` of the server names.
pub fn rust_licenses(
    build: &Build<'_>,
    graph: &CargoBuild,
    integrity: &Manifest,
    embedded: &Document,
) -> Verdict<Vec<Document>> {
    let lock = toml(&build.root.join("Cargo.lock"))?;
    let mut packages = Vec::new();
    for package in manifest::array(field(&lock, "package")?)? {
        packages.push(object(package)?);
    }
    let named = manifest::array(field(embedded, "packages")?)?;
    let mut identities: BTreeSet<Identity> = BTreeSet::new();
    for package in named {
        let package = object(package)?;
        let part = |name: &str| text(package, name).map(str::to_owned);
        identities.insert((part("name")?, part("version")?, part("source")?));
    }
    if identities.len() != named.len() {
        return Err(problem("Ambiguous embedded Rust package identity"));
    }
    let own = build.root.join("Cargo.toml");
    let (mut records, mut recorded, mut indexes) = (Vec::new(), BTreeSet::new(), BTreeSet::new());
    for (identifier, artifact) in &graph.artifacts {
        let name = package_name(identifier)?;
        let version = identifier.rsplit('@').next().unwrap_or_default();
        let entries = locked(&packages, name, version);
        let [entry] = entries[..] else {
            return Err(problem(
                "Actual Cargo artifact is missing or ambiguous in Cargo.lock",
            ));
        };
        let path = Path::new(text(artifact, "manifest_path")?);
        let first_party = resolved(path) == own;
        let registry = identifier.starts_with(&format!("{SOURCE_REGISTRY}#"));
        let (package, files, source, kind) = if registry {
            let (package, files, cache) = registry_license_source(build, path, entry)?;
            indexes.insert(cache);
            (
                package,
                files,
                field(entry, "checksum")?.clone(),
                "crates.io",
            )
        } else {
            let (package, files, source) = local_license_source(build, integrity, path, entry)?;
            (package, files, source, "local")
        };
        if held(&package, "name") != Some(name) || held(&package, "version") != Some(version) {
            return Err(problem(
                "Cargo artifact package identity differs from authenticated metadata",
            ));
        }
        if first_party && package.get("publish") != Some(&Document::Bool(false)) {
            return Err(problem(
                "First-party Cargo publication policy must be false",
            ));
        }
        let identity = (name.to_owned(), version.to_owned(), kind.to_owned());
        let evidence = Document::record([
            ("compilerArtifact", true.into()),
            ("embeddedMetadata", identities.contains(&identity).into()),
        ]);
        let mut record = vec![
            ("package_id".to_owned(), identifier.as_str().into()),
            ("name".to_owned(), name.into()),
            ("version".to_owned(), version.into()),
            ("source".to_owned(), kind.into()),
            ("source_sha256".to_owned(), source),
            ("manifest_sha256".to_owned(), manifest_digest(&files)?),
            ("first_party".to_owned(), first_party.into()),
            ("evidence".to_owned(), evidence),
        ];
        if first_party {
            record.push(("cargo_publish".to_owned(), false.into()));
        }
        record.push(("license".to_owned(), package_license(&package, &files)?));
        records.push(Document::Record(record));
        recorded.insert(identity);
    }
    let only: BTreeSet<Identity> = identities.difference(&recorded).cloned().collect();
    records.extend(embedded_registry_licenses(&packages, &only, &indexes)?);
    if records.len() > MAX_RECORDS {
        return Err(problem("Rust license evidence exceeds the record budget"));
    }
    Ok(records)
}

#[cfg(test)]
#[path = "licenses_tests.rs"]
mod tests;
