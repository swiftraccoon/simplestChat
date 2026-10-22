//! Reuse CI's source closure and Python JSON framing for analyzed-database keys.

use super::{io, process, require};
use crate::ci::{Context, Scope, fingerprint};
use anyhow::Result;
use regex::Regex;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn python_json(value: &Value) -> String {
    match value {
        Value::String(text) => io::json_string(text),
        Value::Array(values) => format!(
            "[{}]",
            values
                .iter()
                .map(python_json)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Value::Object(values) => format!(
            "{{{}}}",
            values
                .iter()
                .map(|(key, value)| format!("{}: {}", io::json_string(key), python_json(value)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        _ => value.to_string(),
    }
}

fn pin(root: &Path) -> Result<Value> {
    let pin: Value = serde_json::from_slice(&io::bounded(
        &root.join("security/codeql-toolchain.json"),
        16384,
    )?)?;
    require(
        pin["schemaVersion"] == true || pin["schemaVersion"].as_f64() == Some(1.0),
        "codeql_toolchain_schema",
    )?;
    let mut languages = pin["languages"]
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("codeql_toolchain_languages"))?
        .keys()
        .map(String::as_str)
        .collect::<Vec<_>>();
    languages.sort_unstable();
    require(
        languages
            == [
                "actions",
                "c-cpp",
                "javascript-typescript",
                "python",
                "rust",
            ],
        "codeql_toolchain_languages",
    )?;
    let workflow = io::bounded(&root.join(".github/workflows/codeql.yml"), 1024 * 1024)?;
    let pattern = Regex::new(r"github/codeql-action/[a-z-]+@([a-f0-9]{40})")?;
    let revisions = pattern
        .captures_iter(std::str::from_utf8(&workflow)?)
        .map(|capture| capture[1].to_owned())
        .collect::<std::collections::BTreeSet<_>>();
    require(
        revisions.len() == 1
            && revisions.first().map(String::as_str) == pin["actionRevision"].as_str(),
        "codeql_action_pin_mismatch",
    )?;
    Ok(pin)
}

pub(super) fn openssl_identity(prefix: &Path) -> Result<String> {
    require(
        prefix.is_absolute() && prefix.canonicalize()? == prefix,
        "codeql_cache_openssl_path",
    )?;
    let mut hash = Sha256::new();
    for directory in ["include", "lib", "share/simplestchat"] {
        let root = prefix.join(directory);
        require(
            root.is_dir() && !root.is_symlink(),
            "codeql_cache_openssl_missing",
        )?;
        let mut files = Vec::new();
        descendants(&root, &mut files)?;
        files.sort();
        for path in files {
            if path.is_dir() {
                continue;
            }
            hash.update(
                path.strip_prefix(prefix)?
                    .to_str()
                    .ok_or_else(|| anyhow::anyhow!("codeql_cache_openssl_path"))?
                    .as_bytes(),
            );
            hash.update(b"\0");
            hash.update(io::digest(&path)?.as_bytes());
            hash.update(b"\0");
        }
    }
    Ok(io::hex(&hash.finalize()))
}

fn descendants(root: &Path, paths: &mut Vec<PathBuf>) -> Result<()> {
    for entry in root.read_dir()? {
        let path = entry?.path();
        require(!path.is_symlink(), "codeql_cache_openssl_symlink")?;
        if path.is_dir() {
            descendants(&path, paths)?;
        }
        paths.push(path);
    }
    Ok(())
}

pub(super) fn identity_key(identity: &Value, namespace: &str, language: &str) -> String {
    format!(
        "codeql-analyzed-v2-{language}-{namespace}-{}",
        io::sha256(python_json(identity).as_bytes())
    )
}

pub(super) fn key(
    context: &Context,
    source: &Path,
    openssl: Option<&Path>,
    language: &str,
    suite: &str,
    include_vendor: bool,
) -> Result<String> {
    require(source.is_absolute(), "codeql_cache_source_path")?;
    require(
        matches!(language, "rust" | "c-cpp") && matches!(suite, "security" | "all"),
        "codeql_cache_scope",
    )?;
    let scope = if language == "c-cpp" {
        Scope::CodeqlNative
    } else {
        Scope::CodeqlRust
    };
    let source_text = source
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("codeql_cache_source_path"))?;
    let mut identity = json!({
        "inputs":fingerprint::key(context,scope)?,"language":language,
        "suite":suite,"includeVendor":include_vendor,"sourceRoot":source_text,
        "toolchain":pin(&context.root)?
    });
    if language == "c-cpp" {
        let prefix = openssl.ok_or_else(|| anyhow::anyhow!("codeql_cache_openssl_missing"))?;
        let packages = process::capture(
            &context.root,
            &context.environment,
            "dpkg-query",
            &["-W".into(), "-f=${Package}\t${Version}\n".into()],
            30,
            4 * 1024 * 1024,
            65536,
        )?;
        let compilers = ["gcc", "g++"]
            .into_iter()
            .map(|name| {
                Ok((
                    name,
                    io::digest(&Path::new("/usr/bin").join(name).canonicalize()?)?,
                ))
            })
            .collect::<Result<BTreeMap<_, _>>>()?;
        let object = identity.as_object_mut().expect("constructed object");
        object.insert(
            "opensslRoot".into(),
            json!(
                prefix
                    .to_str()
                    .ok_or_else(|| anyhow::anyhow!("codeql_cache_openssl_path"))?
            ),
        );
        object.insert("opensslSha256".into(), json!(openssl_identity(prefix)?));
        object.insert("packagesSha256".into(), json!(io::sha256(&packages)));
        object.insert("compilerSha256".into(), json!(compilers));
    }
    let (system, architecture) = (
        context.system.to_lowercase(),
        context.architecture.to_lowercase(),
    );
    let pattern = Regex::new(r"\A[a-z0-9_]{1,32}\z")?;
    require(
        pattern.is_match(&system) && pattern.is_match(&architecture),
        "ci_cache_namespace",
    )?;
    let trust = if context.trusted() {
        "main"
    } else {
        "untrusted"
    };
    Ok(identity_key(
        &identity,
        &format!("{trust}-{system}-{architecture}"),
        language,
    ))
}
