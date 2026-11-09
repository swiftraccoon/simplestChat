//! Exact receipts and private atomic bundle publication; policy is never cached.

use super::{archive, io, require};
use anyhow::Result;
use serde_json::{Value, json};
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

/// A driver keeps analyzer output private and bounds each process to 180 seconds.
/// Tests inject a writer of ordinary owned ZIPs, never a downloaded analyzer.
pub(super) trait Driver {
    fn run(&mut self, label: &str, arguments: &[String]) -> Result<()>;
}

fn digest(path: &Path, expected: u64) -> Result<String> {
    let file = io::regular(path, archive::MAX_BUNDLE, true)?;
    require(file.metadata()?.len() == expected, "codeql_archive_size")?;
    io::digest_file(file)
}

fn revision(value: &Value) -> bool {
    value.as_str().is_some_and(|text| {
        text.len() == 40
            && text
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

pub(super) fn restore(
    driver: &mut impl Driver,
    directory: &Path,
    key: &str,
    database: &Path,
) -> Result<Option<Value>> {
    require(
        directory.is_absolute() && !directory.is_symlink(),
        "codeql_cache_directory",
    )?;
    if !directory.exists() {
        return Ok(None);
    }
    let names = directory
        .read_dir()?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<std::io::Result<std::collections::BTreeSet<_>>>()?;
    require(
        names
            == ["database.zip", "receipt.json"]
                .into_iter()
                .map(Into::into)
                .collect(),
        "codeql_cache_inventory",
    )?;
    let mut receipt: Value =
        serde_json::from_slice(&io::bounded(&directory.join("receipt.json"), 4096)?)?;
    let object = receipt
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("codeql_cache_receipt"))?;
    require(
        object.keys().map(String::as_str).collect::<Vec<_>>()
            == [
                "bundleBytes",
                "bundleSha256",
                "key",
                "revision",
                "schemaVersion",
            ]
            && (receipt["schemaVersion"] == true || receipt["schemaVersion"].as_f64() == Some(1.0))
            && receipt["key"] == key
            && revision(&receipt["revision"]),
        "codeql_cache_receipt",
    )?;
    let size = receipt["bundleBytes"]
        .as_u64()
        .filter(|size| *size > 0 && *size <= archive::MAX_BUNDLE)
        .ok_or_else(|| anyhow::anyhow!("codeql_cache_bundle_size"))?;
    let bundle = directory.join("database.zip");
    require(
        receipt["bundleSha256"] == digest(&bundle, size)?,
        "codeql_cache_bundle_digest",
    )?;
    let language = database
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow::anyhow!("codeql_cache_scope"))?;
    archive::bundle_integrity(&bundle, language)?;
    driver.run(
        "codeql-cache-unbundle",
        &[
            "database".into(),
            "unbundle".into(),
            format!("--name={language}"),
            format!(
                "--target={}",
                database
                    .parent()
                    .ok_or_else(|| anyhow::anyhow!("codeql_cache_directory"))?
                    .display()
            ),
            bundle.to_string_lossy().into_owned(),
        ],
    )?;
    receipt
        .as_object_mut()
        .expect("validated object")
        .insert("reused".into(), json!(true));
    Ok(Some(receipt))
}

struct Staging(PathBuf, File);
impl Staging {
    fn current(&self) -> bool {
        match (fs::symlink_metadata(&self.0), self.1.metadata()) {
            (Ok(current), Ok(owned)) => {
                current.is_dir() && (current.dev(), current.ino()) == (owned.dev(), owned.ino())
            }
            _ => false,
        }
    }
}
impl Drop for Staging {
    fn drop(&mut self) {
        if self.current() {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
}

fn staging(parent: &Path) -> Result<Staging> {
    for _ in 0..100 {
        let mut entropy = [0_u8; 16];
        File::open("/dev/urandom")?.read_exact(&mut entropy)?;
        let path = parent.join(format!(".codeql-analyzed-{}", io::hex(&entropy)));
        match DirBuilder::new().mode(0o700).create(&path) {
            Ok(()) => {
                let descriptor = rustix::fs::open(
                    &path,
                    rustix::fs::OFlags::RDONLY
                        | rustix::fs::OFlags::DIRECTORY
                        | rustix::fs::OFlags::NOFOLLOW
                        | rustix::fs::OFlags::CLOEXEC,
                    rustix::fs::Mode::empty(),
                )?;
                return Ok(Staging(path, File::from(descriptor)));
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
    }
    anyhow::bail!("codeql_cache_staging")
}

pub(super) fn save(
    driver: &mut impl Driver,
    output: &Path,
    directory: &Path,
    key: &str,
    revision: &str,
    language: &str,
) -> Result<Value> {
    require(
        !directory.exists() && !directory.is_symlink(),
        "codeql_cache_exists",
    )?;
    let parent = directory
        .parent()
        .ok_or_else(|| anyhow::anyhow!("codeql_cache_directory"))?;
    fs::create_dir_all(parent)?;
    let staging = staging(parent)?;
    let bundle = staging.0.join("database.zip");
    driver.run(
        "codeql-cache-bundle",
        &[
            "database".into(),
            "bundle".into(),
            "--include-diagnostics".into(),
            "--include-results".into(),
            "--no-include-logs".into(),
            "--cache-cleanup=clear".into(),
            format!("--output={}", bundle.display()),
            output
                .join("databases")
                .join(language)
                .to_string_lossy()
                .into_owned(),
        ],
    )?;
    require(staging.current(), "codeql_cache_staging_changed")?;
    let size = bundle.metadata()?.len();
    require(
        size > 0 && size <= archive::MAX_BUNDLE,
        "codeql_cache_bundle_size",
    )?;
    archive::bundle_integrity(&bundle, language)?;
    let mut receipt = json!({"schemaVersion":1,"key":key,"revision":revision,
        "bundleSha256":digest(&bundle,size)?,"bundleBytes":size});
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(staging.0.join("receipt.json"))?;
    writeln!(file, "{receipt}")?;
    require(staging.current(), "codeql_cache_staging_changed")?;
    fs::rename(&staging.0, directory)?;
    receipt
        .as_object_mut()
        .expect("constructed object")
        .insert("reused".into(), json!(false));
    Ok(receipt)
}
