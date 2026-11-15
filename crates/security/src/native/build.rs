//! Cargo's own messages of one whole production build: only what a build
//! that finished successfully said is believed, and the server it built
//! must be the default release build of the checkout.

use super::{field, object, problem, strings};
use crate::{
    document::Document,
    vendor::{Problem, Verdict, manifest},
};
use std::{
    collections::BTreeMap,
    path::{Component, Path, PathBuf},
};

/// The most bytes the messages may have.
pub(super) const MAX_MESSAGES: usize = 32 * 1024 * 1024;
/// The most messages there may be.
const MAX_RECORDS: usize = 20000;

/// What a whole successful build said.
#[derive(Clone, Debug, PartialEq)]
pub struct CargoBuild {
    /// The server.
    pub binary: PathBuf,
    /// The last artifact of every package, by its identifier.
    pub artifacts: BTreeMap<String, Document>,
    /// What every build script that ran said, by its package and its
    /// directory.
    pub scripts: BTreeMap<(String, String), Document>,
}

fn text<'a>(record: &'a Document, name: &str) -> Verdict<&'a str> {
    manifest::string(field(record, name)?)
}

/// The name of the package that a current identifier of Cargo's names.
pub fn package_name(identifier: &str) -> Verdict<&str> {
    let named = identifier.rsplit('#').next().unwrap_or(identifier);
    let plain = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-';
    match named.split_once('@') {
        Some((name, version))
            if !version.is_empty() && !name.is_empty() && name.bytes().all(plain) =>
        {
            Ok(name)
        }
        _ => Err(problem("Unsupported Cargo package identifier")),
    }
}

/// A path as it really is as far as it exists: every link on the way is
/// followed, and a step up leaves what the way so far leads to.
pub(super) fn resolved(path: &Path) -> PathBuf {
    let mut real = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                real.pop();
            }
            Component::CurDir => {}
            other => {
                real.push(other);
                if let Ok(target) = real.canonicalize() {
                    real = target;
                }
            }
        }
    }
    real
}

/// The file at the absolute `path`, which must really lie below
/// `directory` and be there.
pub fn beneath(path: &Path, directory: &Path) -> Verdict<PathBuf> {
    if !path.is_absolute() || !resolved(path).starts_with(resolved(directory)) {
        return Err(problem("Native artifact escaped its declared directory"));
    }
    let real = path.canonicalize();
    real.map_err(|_| Problem(format!("Missing file: {}", path.display())))
}

/// Hold the artifact of the server to the default release build of the
/// checkout: no test, no debug assertions, full optimization, the default
/// features, and its executable among its files.
fn validate_production_artifact(root: &Path, artifact: &Document) -> Verdict<()> {
    let profile = object(field(artifact, "profile")?)?;
    let flag = |name: &str| profile.get(name) == Some(&Document::Bool(false));
    let produced = resolved(Path::new(text(artifact, "manifest_path")?)) == root.join("Cargo.toml")
        && strings(field(artifact, "features")?)? == ["default"]
        && flag("test")
        && flag("debug_assertions")
        && profile.get("opt_level").and_then(Document::text) == Some("3")
        && strings(field(artifact, "filenames")?)?.contains(&text(artifact, "executable")?);
    if !produced {
        return Err(problem(
            "Cargo artifact is not the default release production server",
        ));
    }
    Ok(())
}

/// The lines of the messages: after a line feed, a carriage return, or the
/// two together.
fn lines(data: &[u8]) -> Vec<&[u8]> {
    let mut lines = Vec::new();
    let mut rest = data;
    while !rest.is_empty() {
        let end = rest.iter().position(|byte| matches!(byte, b'\n' | b'\r'));
        let Some(end) = end else {
            lines.push(rest);
            break;
        };
        lines.push(&rest[..end]);
        let both = rest[end] == b'\r' && rest.get(end + 1) == Some(&b'\n');
        rest = &rest[end + 1 + usize::from(both)..];
    }
    lines
}

/// What the one whole production build of the checkout `root` said in the
/// messages `data`, build scripts whose results were kept included.
pub fn cargo_build(root: &Path, data: &[u8]) -> Verdict<CargoBuild> {
    if data.len() > MAX_MESSAGES {
        return Err(problem("Cargo message budget exceeded"));
    }
    let mut artifacts: BTreeMap<String, Document> = BTreeMap::new();
    let mut scripts = BTreeMap::new();
    let mut binaries = Vec::new();
    let mut finished = false;
    for (number, line) in lines(data).into_iter().enumerate() {
        if number >= MAX_RECORDS || finished {
            return Err(problem(
                "Cargo messages continue after completion or exceed budget",
            ));
        }
        let message = Document::read(line).map_err(manifest::unread)?;
        let message = object(&message)?;
        match text(message, "reason")? {
            "build-finished" => {
                if message.get("success") != Some(&Document::Bool(true)) {
                    return Err(problem("Native evidence requires a successful Cargo build"));
                }
                finished = true;
            }
            "build-script-executed" => {
                let package = text(message, "package_id")?.to_owned();
                let key = (package, text(message, "out_dir")?.to_owned());
                if scripts.insert(key, message.clone()).is_some() {
                    return Err(problem("Duplicate native build script event"));
                }
            }
            "compiler-artifact" => {
                let identifier = text(message, "package_id")?;
                let target = object(field(message, "target")?)?;
                let server = text(target, "name")? == "simplestChat"
                    && strings(field(target, "kind")?)? == ["bin"];
                if server {
                    validate_production_artifact(root, message)?;
                    let executable = Path::new(text(message, "executable")?);
                    binaries.push(beneath(executable, &root.join("target/release"))?);
                }
                // Cargo may say a package's build script and its library.
                let from = message.get("manifest_path");
                let prior = artifacts.get(identifier);
                if prior.is_some_and(|prior| prior.get("manifest_path") != from) {
                    return Err(problem("Ambiguous Cargo package source path"));
                }
                artifacts.insert(identifier.to_owned(), message.clone());
            }
            "compiler-message" => {}
            _ => return Err(problem("Unsupported Cargo message kind")),
        }
    }
    let [binary] = &binaries[..] else {
        return Err(problem("Missing successful production binary artifact"));
    };
    if !finished {
        return Err(problem("Missing successful production binary artifact"));
    }
    let place = root.join("target/release/simplestChat");
    let server = place.canonicalize();
    let server = server.map_err(|_| Problem(format!("Missing file: {}", place.display())))?;
    if *binary != server {
        return Err(problem("Production binary artifact path differs"));
    }
    Ok(CargoBuild {
        binary: server,
        artifacts,
        scripts,
    })
}

#[cfg(test)]
#[path = "build_tests.rs"]
mod tests;
