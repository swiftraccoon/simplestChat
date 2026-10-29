//! Authenticate every executable and dependency before installing a backend reuse.

use super::{Context, Scope, fingerprint, inputs, io, require};
use anyhow::Result;
use regex::Regex;
use rustix::fs::{Mode, OFlags};
use serde_json::{Map, Value};
use std::collections::BTreeSet;
use std::fs::{DirBuilder, File, OpenOptions, Permissions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;

fn names() -> Vec<String> {
    inputs::BINARIES
        .iter()
        .map(|name| (*name).to_owned())
        .chain(inputs::BINARIES.iter().map(|name| format!("{name}.d")))
        .collect()
}

fn words(source: &str) -> Result<Vec<String>> {
    require(
        !source
            .chars()
            .any(|character| ['\0', '\r', '\"', '\'', '$'].contains(&character)),
        "ci_backend_dep_syntax",
    )?;
    let source = source.replace("\\\n", "");
    let lines = source.lines().collect::<Vec<_>>();
    require(
        lines.iter().filter(|line| !line.trim().is_empty()).count() == 1,
        "ci_backend_dep_lines",
    )?;
    let joined = lines.join(" ");
    let mut tokens = Vec::new();
    let mut token = String::new();
    let mut characters = joined.chars();
    while let Some(character) = characters.next() {
        if character == '\\' {
            token.push(
                characters
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("ci_backend_dep_syntax"))?,
            );
        } else if character == ' ' || character == '\t' || character == '\n' {
            if !token.is_empty() {
                tokens.push(std::mem::take(&mut token));
            }
        } else {
            token.push(character);
        }
    }
    if !token.is_empty() {
        tokens.push(token);
    }
    Ok(tokens)
}

fn dependencies(
    context: &Context,
    scope: Scope,
    directory: &Path,
    name: &str,
    built: bool,
) -> Result<()> {
    let source = io::bounded(&directory.join(format!("{name}.d")), 4 * 1024 * 1024)?;
    let words = words(std::str::from_utf8(&source)?)?;
    let expected = format!(
        "{}:",
        context.root.join("target/debug").join(name).display()
    );
    require(
        words.len() > 1 && words[0] == expected,
        "ci_backend_dep_target",
    )?;
    let inventory = context.tracked(false)?;
    let tracked = std::str::from_utf8(&inventory)?
        .split('\0')
        .filter(|name| !name.is_empty())
        .collect::<BTreeSet<_>>();
    let mut native = BTreeSet::new();
    for (variable, suffixes) in [
        (
            "OPENSSL_DIR",
            &[
                "include/openssl",
                "lib/libssl.a",
                "lib/libcrypto.a",
                "lib/pkgconfig/openssl.pc",
            ][..],
        ),
        (
            "AWS_LC_SYS_SYSTEM_DIR",
            &[
                "include",
                "lib/libcrypto-awslc.a",
                "share/rust/aws_lc_bindings.rs",
            ][..],
        ),
    ] {
        if !context.env(variable).is_empty() {
            native.extend(
                suffixes
                    .iter()
                    .map(|suffix| Path::new(context.env(variable)).join(suffix)),
            );
        }
    }
    let generated = Regex::new(concat!(
        r"\Atarget/debug/build/(?:mediasoup-sys-[a-f0-9]{16}/out/fbs\.rs|",
        r"aws-lc-sys-[a-f0-9]{16}/out/bindings\.rs)\z"
    ))
    .expect("fixed generated dependency pattern");
    let mut sources = 0;
    for word in &words[1..] {
        let path = Path::new(word);
        require(
            path.is_absolute() && path.starts_with(&context.root),
            "ci_backend_dep_external",
        )?;
        require(io::resolve(path)? == path, "ci_backend_dep_symlink")?;
        let relative = path
            .strip_prefix(&context.root)?
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("ci_backend_dep_external"))?;
        if tracked.contains(relative) {
            require(
                inputs::backend_artifact_input(scope, relative),
                "ci_backend_dep_unkeyed",
            )?;
            require(path.is_file(), "ci_backend_dep_missing")?;
            sources += 1;
        } else if path.is_dir() && (relative == "vendor" || relative.starts_with("vendor/")) {
            let prefix = format!("{relative}/");
            let children = tracked
                .iter()
                .filter(|name| name.starts_with(&prefix))
                .collect::<Vec<_>>();
            require(
                !children.is_empty()
                    && children
                        .iter()
                        .all(|name| inputs::backend_artifact_input(scope, name)),
                "ci_backend_dep_tree",
            )?;
        } else {
            require(
                generated.is_match(relative) || native.contains(path),
                "ci_backend_dep_unkeyed",
            )?;
            require(!built || path.exists(), "ci_backend_dep_missing")?;
        }
    }
    require(sources > 0, "ci_backend_dep_empty")
}

fn private_directory(path: &Path) -> Result<()> {
    DirBuilder::new().recursive(true).mode(0o700).create(path)?;
    Ok(())
}

fn copy(source: &Path, destination: &Path, new: bool) -> Result<()> {
    let mut source = io::regular(source, 1024 * 1024 * 1024, false)?;
    let flags = OFlags::WRONLY
        | OFlags::CREATE
        | OFlags::NOFOLLOW
        | OFlags::NONBLOCK
        | OFlags::CLOEXEC
        | if new { OFlags::EXCL } else { OFlags::TRUNC };
    let descriptor = rustix::fs::open(destination, flags, Mode::from_raw_mode(0o700))?;
    let mut destination = File::from(descriptor);
    require(
        destination.metadata()?.is_file(),
        "ci_cache_binary_destination",
    )?;
    std::io::copy(&mut source, &mut destination)?;
    destination.set_permissions(Permissions::from_mode(0o700))?;
    Ok(())
}

fn revision(value: &Value) -> Result<&str> {
    let value = value
        .as_str()
        .filter(|value| !value.is_empty() && value.chars().count() <= 2048)
        .ok_or_else(|| anyhow::anyhow!("invalid_tool_string"))?;
    require(
        value.len() == 40
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "ci_cache_revision",
    )?;
    Ok(value)
}

fn record<'a>(value: &'a Value, names: &[&str]) -> Result<&'a Map<String, Value>> {
    let object = value
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("invalid_tool_record"))?;
    require(
        object.len() == names.len() && names.iter().all(|name| object.contains_key(*name)),
        "invalid_tool_record_fields",
    )?;
    Ok(object)
}

pub(super) fn save(context: &Context, scope: Scope, key: &str, directory: &Path) -> Result<()> {
    require(!scope.codeql(), "ci_cache_codeql_requires_database")?;
    require(
        fingerprint::key(context, scope)? == key,
        "ci_cache_inputs_changed",
    )?;
    let revision = context.env("GITHUB_SHA");
    require(
        revision.len() == 40
            && revision
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "ci_cache_revision",
    )?;
    let output = context.root.join("target/debug");
    if scope.binary_reuse() {
        for name in inputs::BINARIES {
            dependencies(context, scope, &output, name, true)?;
        }
    }
    private_directory(directory)?;
    let mut artifacts = Vec::new();
    if scope.binary_reuse() {
        for name in names() {
            let source = output.join(&name);
            artifacts.push(format!(
                "{}: {}",
                io::json_string(&name),
                io::json_string(&io::digest(&source)?)
            ));
            require(
                name.ends_with(".d")
                    || rustix::fs::access(&source, rustix::fs::Access::EXEC_OK).is_ok(),
                "ci_cache_binary_not_executable",
            )?;
            let destination = directory.join(&name);
            require(
                !destination.exists() && !destination.is_symlink(),
                "ci_cache_existing_binary",
            )?;
            copy(&source, &destination, true)?;
        }
    }
    let receipt = format!(
        "{{\"schema\": 1, \"key\": {}, \"revision\": {}, \"artifacts\": {{{}}}}}\n",
        io::json_string(key),
        io::json_string(revision),
        artifacts.join(", ")
    );
    let path = directory.join("receipt.json");
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(receipt.as_bytes())?;
    file.set_permissions(Permissions::from_mode(0o600))?;
    Ok(())
}

pub(super) fn verify(
    context: &Context,
    scope: Scope,
    key: &str,
    directory: &Path,
) -> Result<String> {
    require(!scope.codeql(), "ci_cache_codeql_requires_database")?;
    require(
        fingerprint::key(context, scope)? == key,
        "ci_cache_inputs_changed",
    )?;
    let source = io::bounded(&directory.join("receipt.json"), 65536)?;
    let receipt: Value = serde_json::from_slice(&source)?;
    let receipt = record(&receipt, &["schema", "key", "revision", "artifacts"])?;
    // Python equality accepts JSON 1.0 and true as equal to schema 1. Preserve
    // that existing receipt contract; this migration does not revise its schema.
    require(
        (receipt["schema"].as_f64() == Some(1.0) || receipt["schema"] == Value::Bool(true))
            && receipt["key"].as_str() == Some(key),
        "ci_cache_receipt_identity",
    )?;
    let revision = revision(&receipt["revision"])?;
    let names = if scope.binary_reuse() {
        names()
    } else {
        Vec::new()
    };
    let fields = names.iter().map(String::as_str).collect::<Vec<_>>();
    let artifacts = record(&receipt["artifacts"], &fields)?;
    for name in &names {
        require(
            Some(io::digest(&directory.join(name))?.as_str()) == artifacts[name].as_str(),
            "ci_cache_binary_changed",
        )?;
    }
    if scope.binary_reuse() {
        for name in inputs::BINARIES {
            dependencies(context, scope, directory, name, false)?;
        }
        for name in inputs::BINARIES {
            let destination = context.root.join("target/debug").join(name);
            if let Some(parent) = destination.parent() {
                std::fs::create_dir_all(parent)?;
            }
            require(!destination.is_symlink(), "ci_cache_binary_destination")?;
            copy(&directory.join(name), &destination, false)?;
        }
    }
    Ok(revision.to_owned())
}
