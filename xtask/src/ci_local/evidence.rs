//! Private evidence and exact aggregate receipts for one owned local run.

use super::{Context, io_failure};
use crate::ci_evidence::receipt::{Identity, successful};
use crate::tooling::{Failure, Result};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

pub(super) fn private_directory(path: &Path, recursive: bool) -> Result<()> {
    DirBuilder::new()
        .mode(0o700)
        .recursive(recursive)
        .create(path)
        .map_err(io_failure)
}

pub(super) fn new_file(path: &Path) -> Result<File> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(io_failure)
}

pub(super) fn write(path: &Path, bytes: &[u8]) -> Result<()> {
    new_file(path)?.write_all(bytes).map_err(io_failure)
}

pub(super) fn json_file(path: &Path, value: &Value) -> Result<()> {
    let mut bytes =
        serde_json::to_vec_pretty(value).map_err(|error| Failure::new(1, error.to_string()))?;
    bytes.push(b'\n');
    write(path, &bytes)
}

pub(super) fn random_id() -> Result<String> {
    let mut bytes = [0; 16];
    File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .map_err(io_failure)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

pub(super) fn create_output(root: &Path, selected: Option<&Path>) -> Result<PathBuf> {
    if let Some(selected) = selected.filter(|path| !path.as_os_str().is_empty()) {
        let path = root.join(selected);
        if fs::symlink_metadata(&path).is_ok() {
            return Err(Failure::new(2, "--output must be a new directory."));
        }
        private_directory(&path, false)?;
        return fs::canonicalize(path).map_err(io_failure);
    }
    loop {
        let path = root
            .join("results")
            .join(format!("ci-local.{}", random_id()?));
        match DirBuilder::new().mode(0o700).create(&path) {
            Ok(()) => return fs::canonicalize(path).map_err(io_failure),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(io_failure(error)),
        }
    }
}

pub(super) fn event(context: &Context) -> Value {
    json!({
        "ref": "refs/heads/main", "before": context.base, "after": context.revision,
        "head_commit": {"id": context.revision}, "local_ci": true,
        "repository": {"default_branch": "main", "full_name": "swiftraccoon/simplestChat",
            "name": "simplestChat", "owner": {"name": "swiftraccoon", "login": "swiftraccoon"}},
    })
}

pub(super) fn summary(context: &Context, status: u8) -> Value {
    json!({
        "schema": 1, "revision": context.revision, "base": context.base,
        "selection": context.job, "runnerPlatform": context.platform,
        "runnerImage": context.image, "exitCode": status, "productionPlatform": "linux/amd64",
        "vendorScanning": "excluded", "status": if status == 0 { "passed" } else { "failed" },
        "startedAt": context.started, "finishedAt": super::timestamp(),
        "completeLocalGate": context.job == "all" && status == 0,
        "hostedOnly": ["CodeQL upload and stored-analysis ingestion",
            "GitHub OIDC release signing and publication"],
    })
}

fn read_json(path: &Path) -> Result<Value> {
    let bytes = fs::read(path).map_err(io_failure)?;
    serde_json::from_slice(&bytes).map_err(|error| Failure::new(1, error.to_string()))
}

pub(super) fn validate(context: &Context, output: &Path) -> Result<()> {
    let evidence = output.join("checks");
    let receipt = read_json(&evidence.join("required.json"))?;
    let identity = Identity {
        run_id: context.run_id.clone(),
        revision: context.revision.clone(),
        base: context.base.clone(),
    };
    let mut workflows = serde_json::Map::new();
    for name in ["ci.yml", "security.yml", "codeql.yml"] {
        let bytes =
            fs::read(context.root.join(".github/workflows").join(name)).map_err(io_failure)?;
        workflows.insert(
            name.into(),
            Value::String(
                Sha256::digest(bytes)
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect(),
            ),
        );
    }
    let valid = successful(&receipt)
        && identity.matches(&receipt)
        && receipt["workflows"] == Value::Object(workflows)
        && receipt["gates"]
            .as_array()
            .is_some_and(|array| !array.is_empty());
    let checks = receipt["checks"]
        .as_array()
        .filter(|array| !array.is_empty());
    if !valid || checks.is_none() {
        return Err(Failure::new(
            1,
            "The real aggregate gate did not validate this exact local run",
        ));
    }
    let mut identifiers = BTreeSet::new();
    for identifier in checks.into_iter().flatten() {
        let Some(identifier) = identifier.as_str().filter(|identifier| {
            !identifier.is_empty()
                && identifier.len() <= 128
                && identifier
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
        }) else {
            return Err(Failure::new(
                1,
                "A required matrix entry has an invalid receipt identifier",
            ));
        };
        if !identifiers.insert(identifier) {
            return Err(Failure::new(
                1,
                "A required matrix entry has a duplicate receipt identifier",
            ));
        }
        let actual = read_json(&evidence.join("receipts").join(format!("{identifier}.json")))?;
        if !identity.matches_matrix(&actual, identifier) {
            return Err(Failure::new(
                1,
                "A required matrix entry has no matching successful execution receipt",
            ));
        }
    }
    Ok(())
}

/// Optional cache reuse only for the runner ISA and exact authenticated bundle.
pub(super) fn codeql_cache(root: &Path, platform: &str) -> bool {
    let cache = root.join(".cache/codeql-tools");
    let validate = || -> Option<()> {
        if !cache.is_dir() {
            return None;
        }
        let manifest = read_json(&root.join("security/codeql-toolchain.json")).ok()?;
        let platform = if platform == "linux/arm64" {
            "linux-aarch64"
        } else {
            "linux-x86_64"
        };
        let pin = &manifest["bundles"][platform];
        let digest = pin["sha256"].as_str()?;
        if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return None;
        }
        let directory = cache.join(digest);
        let receipt = read_json(&directory.join("receipt.json")).ok()?;
        if receipt != json!({"archiveSha256": digest, "archiveBytes": pin["bytes"]}) {
            return None;
        }
        let executable = directory.join("codeql/codeql").metadata().ok()?;
        (executable.is_file() && executable.permissions().mode() & 0o111 != 0).then_some(())
    };
    validate().is_some()
}
