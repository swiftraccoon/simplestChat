//! Whole unchanged standalone restore CLI and real owned inert daemon peers.
//! Core rows include 077, retained output and final lock release. Public-entry
//! refusals use real public run in exact self-workers. No engine or host proof.
use super::super::{deploy::records, stage::process as child};
use super::*;
use crate::test_support::Fixture;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs::{self, File},
    io::{self, Read, Write},
    os::{
        fd::AsFd,
        unix::fs::{MetadataExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
    time::Duration,
};
const WHOLE: Duration = Duration::from_secs(1800);
const RESERVE: Duration = Duration::from_secs(60);
const PYTHON_BODY: Duration = Duration::from_secs(90);
const PYTHON_TAIL: Duration = Duration::from_secs(10);
const MAXIMUM: u64 = 65 * 1024 * 1024;
const JSON_BYTES: usize = 2 * 1024 * 1024;
const EXACT: &str = concat!(
    "release::attestation::restore::oracle_tests::",
    "whole_restore_matches_unchanged_python",
);
const WORKER: &str = "SIMPLESTCHAT_RESTORE_ORACLE_PUBLIC_WORKER";
const SOURCE_ATTEMPT: &str = "release.fixture000";
const CID: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Oracle {
    schema_version: u8,
    scenario: String,
    input: Value,
    observation: Value,
}
fn hash(data: &[u8]) -> String {
    super::super::files::hex(&Sha256::digest(data))
}
fn read(path: &Path) -> io::Result<Vec<u8>> {
    let original = File::open(path)?;
    let result = (|| {
        if original.metadata()?.len() > JSON_BYTES as u64 {
            return Err(io::Error::other("restore observer JSON bound"));
        }
        let mut bytes = Vec::new();
        (&original)
            .take(JSON_BYTES as u64 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > JSON_BYTES {
            return Err(io::Error::other("restore observer grew beyond JSON bound"));
        }
        Ok(bytes)
    })();
    let closed = nix::unistd::close(original).map_err(io::Error::from);
    result.and_then(|bytes| closed.map(|()| bytes))
}
fn decoded(path: &Path) -> io::Result<Value> {
    let bytes = read(path)?;
    crate::json::decode_limited(&bytes, JSON_BYTES)
        .map_err(|_| io::Error::other("invalid restore observer JSON"))?;
    serde_json::from_slice(&bytes).map_err(io::Error::other)
}
fn pin(path: &Path) -> io::Result<Value> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Ok(json!({"link":fs::read_link(path)?.to_str()
            .ok_or_else(||io::Error::other("observer link UTF8"))?,
            "mode":metadata.mode() & 0o7777}));
    }
    if !metadata.is_file() || metadata.len() > MAXIMUM {
        return Err(io::Error::other("restore observer regular file bound"));
    }
    let original = File::open(path)?;
    let result = (|| {
        let mut source = &original;
        let mut hash = Sha256::new();
        let mut buffer = [0; 65536];
        let mut count = 0_u64;
        loop {
            let read = source.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            count += read as u64;
            if count > MAXIMUM {
                return Err(io::Error::other("observer file grew"));
            }
            hash.update(&buffer[..read]);
        }
        if count != metadata.len() {
            return Err(io::Error::other("observer length changed"));
        }
        Ok(
            json!({"bytes":count,"sha256":super::super::files::hex(&hash.finalize()),
            "mode":metadata.mode() & 0o7777}),
        )
    })();
    let closed = nix::unistd::close(original).map_err(io::Error::from);
    result.and_then(|value| closed.map(|()| value))
}
fn attempt_paths(root: &Path) -> io::Result<Vec<PathBuf>> {
    let directory = root.join("state/restores");
    if !directory.exists() {
        return Ok(Vec::new());
    }
    let mut values = fs::read_dir(directory)?
        .map(|entry| entry.map(|e| e.path()))
        .collect::<io::Result<Vec<_>>>()?;
    values.sort();
    Ok(values)
}
fn normalize(value: Value, root: &Path, attempts: &[PathBuf]) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(key, value)| (key, normalize(value, root, attempts)))
                .collect(),
        ),
        Value::Array(values) => Value::Array(
            values
                .into_iter()
                .map(|value| normalize(value, root, attempts))
                .collect(),
        ),
        Value::String(mut text) => {
            for (index, path) in attempts.iter().enumerate() {
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    text = text.replace(
                        &format!("/restores/{name}"),
                        &format!("/restores/$RESTORE-{:03}", index + 1),
                    );
                }
            }
            text = text.replace(root.to_str().unwrap(), "$ROOT");
            // Fixed UUIDv4 fixture observations match across adapters. Container
            // names, owners and selectors stay exact; only physical paths normalize.
            Value::String(text)
        }
        other => other,
    }
}
#[test]
fn generated_and_foreign_container_names_remain_exact_without_admission_claims() {
    let root = Path::new("/owned/restore-fixture");
    let generation = root.join("state/restores/restore.dddddddddddddddddddddddddddddddd");
    let names = [
        "scpub-restore-dddddddddddd4ddd9ddddddddddddddd",
        "scpub-restore-dddddddddddd4ddd9dddddddddddddde",
        "scpub-restore-ffffffffffffffffffffffffffffffff",
    ];
    for name in names {
        let selector = json!(format!("name=^/{name}$"));
        assert_eq!(normalize(selector.clone(), root, &[]), selector);
        assert_eq!(
            normalize(selector.clone(), root, std::slice::from_ref(&generation)),
            selector,
        );
    }
    let leaf = json!(generation.join("restore.json").to_str().unwrap());
    assert_eq!(
        normalize(leaf, root, &[generation]),
        json!("$ROOT/state/restores/$RESTORE-001/restore.json"),
    );
}

fn inventory(path: &Path, root: &Path) -> io::Result<Value> {
    fn visit(path: &Path, root: &Path, found: &mut BTreeMap<String, Value>) -> io::Result<()> {
        if !path.exists() {
            return Ok(());
        }
        let mut entries = fs::read_dir(path)?.collect::<io::Result<Vec<_>>>()?;
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for entry in entries {
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.is_dir() && !metadata.file_type().is_symlink() {
                visit(&path, root, found)?;
            } else {
                let mut item = pin(&path)?;
                item["links"] = json!(metadata.nlink());
                if metadata.is_file()
                    && metadata.len() <= JSON_BYTES as u64
                    && (path.extension().is_some_and(|ext| ext == "json")
                        || path.file_name().is_some_and(|name| {
                            name.to_str().is_some_and(|s| s.starts_with(".release-"))
                        })
                        || path
                            .file_name()
                            .is_some_and(|name| name == "restore-verified.timestamp"))
                    && let Ok(value) = decoded(&path)
                {
                    item["json"] = value;
                }
                let name = path
                    .strip_prefix(root)
                    .map_err(io::Error::other)?
                    .to_str()
                    .ok_or_else(|| io::Error::other("observer path UTF8"))?
                    .to_owned();
                found.insert(name, item);
            }
        }
        Ok(())
    }
    let mut found = BTreeMap::new();
    visit(path, root, &mut found)?;
    let attempts = attempt_paths(root)?;
    let mut temporary = 0;
    let mut result = serde_json::Map::new();
    for (name, value) in found {
        let normalized = normalize(Value::String(name.clone()), root, &attempts);
        let mut key = normalized.as_str().unwrap().to_owned();
        if Path::new(&name)
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|name| name.starts_with(".release-"))
        {
            temporary += 1;
            key = format!(
                "{}/$ATOMIC-{temporary}",
                Path::new(&key).parent().unwrap().display()
            );
        }
        result.insert(key, normalize(value, root, &attempts));
    }
    Ok(Value::Object(result))
}
fn product_tree(root: &Path) -> io::Result<Value> {
    let mut map = serde_json::Map::new();
    for name in ["public", "state", "work"] {
        let value = inventory(&root.join(name), root)?;
        map.extend(value.as_object().unwrap().clone());
    }
    Ok(Value::Object(map))
}
fn directory_modes(root: &Path) -> io::Result<Value> {
    fn visit(path: &Path, root: &Path, map: &mut serde_json::Map<String, Value>) -> io::Result<()> {
        let meta = fs::symlink_metadata(path)?;
        if meta.is_dir() && !meta.file_type().is_symlink() {
            map.insert(
                path.strip_prefix(root).unwrap().to_str().unwrap().into(),
                json!(meta.mode() & 0o7777),
            );
            for entry in fs::read_dir(path)? {
                visit(&entry?.path(), root, map)?;
            }
        }
        Ok(())
    }
    let mut map = serde_json::Map::new();
    for name in ["public", "state", "work", "config"] {
        visit(&root.join(name), root, &mut map)?;
    }
    directory_mode_projection(map, root, &attempt_paths(root)?)
}
fn directory_mode_projection(
    modes: serde_json::Map<String, Value>,
    root: &Path,
    attempts: &[PathBuf],
) -> io::Result<Value> {
    let mut projected = serde_json::Map::new();
    for (name, mode) in modes {
        let Value::String(name) = normalize(Value::String(name), root, attempts) else {
            return Err(io::Error::other("restore directory key projection"));
        };
        if projected.insert(name, mode).is_some() {
            return Err(io::Error::other("duplicate restore directory key"));
        }
    }
    Ok(Value::Object(projected))
}

#[test]
fn directory_modes_project_only_observed_attempt_keys_and_keep_every_mode() {
    let root = Path::new("/owned/restore-fixture");
    let raw = "state/restores/restore.dddddddddddddddddddddddddddddddd";
    let foreign = "state/restores/restore.ffffffffffffffffffffffffffffffff";
    let attempts = [root.join(raw)];
    let modes = serde_json::Map::from_iter([
        ("state".into(), json!(0o750)),
        (raw.into(), json!(0o700)),
        (foreign.into(), json!(0o711)),
    ]);
    let projected = directory_mode_projection(modes, root, &attempts).unwrap();
    assert_eq!(projected["state"], 0o750);
    assert_eq!(projected["state/restores/$RESTORE-001"], 0o700);
    assert_eq!(projected[foreign], 0o711);
    assert!(projected.get(raw).is_none());
    let collision = serde_json::Map::from_iter([
        (raw.into(), json!(0o700)),
        ("state/restores/$RESTORE-001".into(), json!(0o711)),
    ]);
    assert!(directory_mode_projection(collision, root, &attempts).is_err());
}

fn semantic_tree(mut tree: Value) -> Value {
    for item in tree.as_object_mut().unwrap().values_mut() {
        if let Some(map) = item.as_object_mut()
            && map.contains_key("json")
        {
            // Raw byte counts and SHA remain in the original/native observations.
            // Only complete strict JSON compares independent serializer layouts.
            map.remove("bytes");
            map.remove("sha256");
        }
    }
    tree
}
fn identities(path: &Path) -> io::Result<Value> {
    fn visit(
        path: &Path,
        base: &Path,
        found: &mut serde_json::Map<String, Value>,
    ) -> io::Result<()> {
        for entry in fs::read_dir(path)? {
            let path = entry?.path();
            let meta = fs::symlink_metadata(&path)?;
            if meta.is_dir() && !meta.file_type().is_symlink() {
                visit(&path, base, found)?;
            } else {
                found.insert(
                    path.strip_prefix(base).unwrap().to_str().unwrap().into(),
                    json!([
                        meta.dev(),
                        meta.ino(),
                        meta.mode(),
                        meta.uid(),
                        meta.gid(),
                        meta.nlink(),
                        meta.len(),
                        meta.mtime(),
                        meta.mtime_nsec(),
                        meta.ctime(),
                        meta.ctime_nsec()
                    ]),
                );
            }
        }
        Ok(())
    }
    let mut map = serde_json::Map::new();
    visit(path, path, &mut map)?;
    Ok(Value::Object(map))
}
fn original_identities(root: &Path) -> io::Result<Value> {
    let mut map = identities(&root.join("public"))?
        .as_object()
        .unwrap()
        .clone();
    for name in [
        "library/restore-verify.sql",
        "bin/docker",
        "original-dump-alias",
        "original-verifier-alias",
    ] {
        let path = root.join(name);
        match fs::symlink_metadata(&path) {
            Ok(meta) => {
                map.insert(
                    name.into(),
                    json!([
                        meta.dev(),
                        meta.ino(),
                        meta.mode(),
                        meta.uid(),
                        meta.gid(),
                        meta.nlink(),
                        meta.len(),
                        meta.mtime(),
                        meta.mtime_nsec(),
                        meta.ctime(),
                        meta.ctime_nsec()
                    ]),
                );
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(Value::Object(map))
}
fn lock_available(path: &Path) -> io::Result<bool> {
    let original = File::open(path)?;
    let available = rustix::fs::flock(
        original.as_fd(),
        rustix::fs::FlockOperation::NonBlockingLockExclusive,
    )
    .is_ok();
    let closed = nix::unistd::close(original).map_err(io::Error::from);
    closed.map(|()| available)
}
fn fault(name: &str) -> Option<Fault> {
    use AtomicPoint::{Admission, DirectorySync, FileSync};
    use AtomicTarget::{CreatedRestore, FinalRestore, Freshness, InitialRestore, Outcome};
    Some(match name {
        "snapshot-read" => Fault::SnapshotRead,
        "snapshot-write" => Fault::SnapshotWrite,
        "snapshot-flush" => Fault::SnapshotFlush,
        "snapshot-fsync" => Fault::SnapshotSync,
        "snapshot-unlink" => Fault::SnapshotUnlink,
        "interrupt-before-restore" => Fault::InterruptBeforeRestore,
        "terminate-before-restore" => Fault::TerminateBeforeRestore,
        "restore-record-initial-write" => Fault::Atomic(InitialRestore, Admission),
        "restore-record-created-write" => Fault::Atomic(CreatedRestore, Admission),
        "restore-record-final-write" => Fault::Atomic(FinalRestore, Admission),
        "restore-record-final-file-sync" => Fault::Atomic(FinalRestore, FileSync),
        "restore-record-final-directory-sync" => Fault::Atomic(FinalRestore, DirectorySync),
        "outcome-write" => Fault::Atomic(Outcome, Admission),
        "outcome-file-sync" => Fault::Atomic(Outcome, FileSync),
        "outcome-directory-sync" => Fault::Atomic(Outcome, DirectorySync),
        "freshness-write" => Fault::Atomic(Freshness, Admission),
        "freshness-file-sync" => Fault::Atomic(Freshness, FileSync),
        "freshness-directory-sync" => Fault::Atomic(Freshness, DirectorySync),
        _ => return None,
    })
}
fn borrowed_bytes(file: &File) -> io::Result<Vec<u8>> {
    let length = file.metadata()?.len();
    if length > JSON_BYTES as u64 {
        return Err(io::Error::other("original evidence sink exceeds bound"));
    }
    let mut bytes = vec![0; length as usize + 1];
    let mut offset = 0;
    loop {
        let count = std::os::unix::fs::FileExt::read_at(file, &mut bytes[offset..], offset as u64)?;
        if count == 0 {
            break;
        }
        offset += count;
        if offset > length as usize {
            return Err(io::Error::other("original evidence sink grew"));
        }
    }
    if offset != length as usize {
        return Err(io::Error::other("original evidence sink length differs"));
    }
    bytes.truncate(offset);
    Ok(bytes)
}

struct Actor {
    scenario: String,
    fault_observed: bool,
    delivered: Vec<Value>,
    readonly_inputs: bool,
    locks_held: bool,
    all_settled: bool,
    snapshots: Vec<Value>,
    root: PathBuf,
    ready_started: Option<Instant>,
    ready_offsets: Vec<Duration>,
}
impl Executor for Actor {
    async fn run(&mut self, owner: &mut Owner, request: Invocation<'_>) -> CommandOutput {
        let role = request.role;
        if role == Role::Ready {
            let first = *self.ready_started.get_or_insert_with(|| owner.now());
            self.ready_offsets.push(owner.now().duration_since(first));
        }
        self.locks_held &=
            lock_available(&self.root.join("work/workload.lock")).is_ok_and(|value| !value);
        if let Some(file) = request.input {
            self.readonly_inputs &= rustix::fs::fcntl_getfl(file).is_ok_and(|flags| {
                flags.intersection(rustix::fs::OFlags::ACCMODE) == rustix::fs::OFlags::RDONLY
            });
        }
        let attempts = attempt_paths(&self.root);
        let snapshot = attempts
            .as_ref()
            .ok()
            .and_then(|paths| paths.last())
            .map(|path| decoded(&path.join("restore.json")))
            .transpose()
            .ok()
            .flatten()
            .unwrap_or(Value::Null);
        self.snapshots
            .push(json!({"role":format!("{role:?}"),"record":snapshot}));
        let original_sink = request.stdout;
        let output = process::Native.run(owner, request).await;
        // Borrow the original same sink before adding the delivery fault. No
        // duplicate descriptor or inferred serializer bytes grant this effect.
        let sink = borrowed_bytes(original_sink);
        let sink_facts = sink.as_ref().ok().map(|bytes| {
            json!({
            "bytes":bytes.len(),"sha256":hash(bytes),
            "matchesCapture":*bytes==output.stdout})
        });
        self.all_settled &= output.settled;
        self.delivered
            .push(json!({"role":format!("{role:?}"),"status":output.status,
            "settled":output.settled,"stdoutComplete":output.stdout_complete,
            "stdoutBytes":output.stdout_bytes,"stderrBytes":output.stderr_bytes,
            "stdoutSha256":hash(&output.stdout),
            "stdoutHex":super::super::files::hex(&output.stdout),
            "error":output.error.map(|cause|format!("{cause:?}")),"originalSink":sink_facts}));
        if self.scenario == "create-timeout-after-effect" && role == Role::Create {
            self.fault_observed = true;
            owner.record(Cause::Deadline);
            CommandOutput {
                error: Some(Cause::Deadline),
                ..output
            }
        } else {
            output
        }
    }
}
struct Case {
    codes: Vec<i32>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    primary: Option<Cause>,
    failures: Vec<Option<String>>,
    before: Value,
    after: Value,
    identities_before: Value,
    identities_after: Value,
    initial_files: Value,
    files: Value,
    modes: Value,
    daemon: Value,
    delivered: Vec<Value>,
    snapshots: Vec<Value>,
    shared: (usize, usize),
    records: (usize, usize),
    locked_during_output: bool,
    lock_released: bool,
    readonly_inputs: bool,
    all_settled: bool,
    fault_hits: usize,
    snapshot_unlinks: usize,
    fault_observed: bool,
    public_worker: Option<child::Completion>,
    output_closed: bool,
    ready_offsets: Vec<Duration>,
    ready_elapsed: Option<Duration>,
}
fn output_sink(path: &Path, bytes: &[u8]) -> io::Result<Vec<u8>> {
    let mut original = File::options()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    let result = (|| {
        original.write_all(bytes)?;
        original.flush()?;
        let mut observed = vec![0; bytes.len() + 1];
        let count = std::os::unix::fs::FileExt::read_at(&original, &mut observed, 0)?;
        observed.truncate(count);
        Ok(observed)
    })();
    let closed = nix::unistd::close(original).map_err(io::Error::from);
    result.and_then(|value| closed.map(|()| value))
}
const SCENARIOS: &[&str] = &[
    "success",
    "failed-release-backup",
    "subset-ledger",
    "source-hardlink",
    "repeat",
    "selector-invalid",
    "root-not-root",
    "root-mode",
    "config-mode",
    "work-mode",
    "state-mode",
    "results-mode",
    "releases-mode",
    "source-mode",
    "lock-mode",
    "lock-busy",
    "current-unfinished",
    "release-unfinished",
    "journal-schema-bool",
    "outcome-missing",
    "outcome-mode",
    "outcome-symlink",
    "outcome-syntax",
    "outcome-duplicate",
    "outcome-revision",
    "outcome-digest",
    "manifest-mode",
    "manifest-symlink",
    "manifest-syntax",
    "manifest-duplicate",
    "manifest-revision",
    "ledger-missing",
    "ledger-empty",
    "ledger-version",
    "ledger-checksum",
    "ledger-not-packaged",
    "backup-empty",
    "backup-oversized",
    "backup-mode",
    "backup-symlink",
    "backup-digest",
    "snapshot-read",
    "snapshot-write",
    "snapshot-flush",
    "snapshot-fsync",
    "image-command",
    "image-identity",
    "restore-record-initial-write",
    "prior-command",
    "prior-container",
    "create-command",
    "create-unknown",
    "create-timeout-after-effect",
    "restore-record-created-write",
    "start-command",
    "ready-retry",
    "ready-deadline",
    "ready-exact-boundary",
    "roles-command",
    "restore-command",
    "restored-ledger-command",
    "restored-ledger-diff",
    "verify-sql-missing",
    "verify-sql-private0600",
    "verify-sql-hardlink",
    "verify-command",
    "verify-invalid-json",
    "amcheck-command",
    "cleanup-select-command",
    "cleanup-ambiguous",
    "cleanup-id-diff",
    "cleanup-name-diff",
    "cleanup-image-diff",
    "cleanup-owner-diff",
    "cleanup-inspect-command",
    "cleanup-remove-command",
    "cleanup-confirm-present",
    "restore-command-cleanup-remove",
    "restore-record-final-write",
    "restore-record-final-file-sync",
    "restore-record-final-directory-sync",
    "snapshot-unlink",
    "outcome-write",
    "outcome-file-sync",
    "outcome-directory-sync",
    "freshness-write",
    "freshness-file-sync",
    "freshness-directory-sync",
    "interrupt-before-restore",
    "terminate-before-restore",
];
const BASELINE_PYTHON: &[&str] = &[
    "build/bootstrap.py",
    "build/bootstrap_access.py",
    "build/bootstrap_controller.py",
    "build/build-release.py",
    "build/caddy_materials.py",
    "build/caddy_probe_guest.py",
    "build/caddy_probe_vm.py",
    "build/capacity.py",
    "build/capacity_artifacts.py",
    "build/ci-local-evidence.py",
    "build/ci_cache_retention.py",
    "build/ci_local_act.py",
    "build/ci_release_checks.py",
    "build/ci_verified.py",
    "build/deploy.py",
    "build/fetch-release.py",
    "build/force_deploy.py",
    "build/issue_account_recovery.py",
    "build/maintain_namespace_probe.py",
    "build/maintain_namespace_probe_guest.py",
    "build/migrate.py",
    "build/migration_artifact.py",
    "build/migration_controller.py",
    "build/migration_dns.py",
    "build/native_security.py",
    "build/native_security_cache.py",
    "build/release_attestation.py",
    "build/release_build.py",
    "build/release_container_fixture.py",
    "build/release_container_harness.py",
    "build/release_deploy.py",
    "build/release_fetch_controller.py",
    "build/release_http_policy.py",
    "build/run-vps-capacity.py",
    "build/security_actions.py",
    "build/security_archive.py",
    "build/security_awslc.py",
    "build/security_check.py",
    "build/security_codeql.py",
    "build/security_codeql_cache.py",
    "build/security_codeql_cargo.py",
    "build/security_codeql_local.py",
    "build/security_codeql_resources.py",
    "build/security_codeql_rust_sources.py",
    "build/security_codeql_sources.py",
    "build/security_codeql_tools.py",
    "build/security_codeql_triage.py",
    "build/security_context.py",
    "build/security_dependency_licenses.py",
    "build/security_elf.py",
    "build/security_findings.py",
    "build/security_image.py",
    "build/security_image_advisories.py",
    "build/security_image_cache.py",
    "build/security_image_policy.py",
    "build/security_license_identity.py",
    "build/security_mutation.py",
    "build/security_native.py",
    "build/security_openssl.py",
    "build/security_policy.py",
    "build/security_rpm_notices.py",
    "build/security_rulesets.py",
    "build/security_runtime.py",
    "build/security_secret_projection.py",
    "build/security_secret_spans.py",
    "build/security_secrets.py",
    "build/security_semgrep.py",
    "build/security_semgrep_summary.py",
    "build/security_source_scope.py",
    "build/security_tools.py",
    "build/security_vendor.py",
    "build/security_vm.py",
    "build/security_vm_deploy.py",
    "build/security_vm_deploy_guest.py",
    "build/security_vm_fetch.py",
    "build/security_vm_fetch_guest.py",
    "build/security_vm_fetch_hosts.py",
    "build/security_vm_guest.py",
    "build/security_vm_ops.py",
    "build/security_vm_ops_guest.py",
    "build/security_vm_qmp.py",
    "build/security_vm_receive.py",
    "build/security_vm_receive_guest.py",
    "build/security_vm_receive_route.py",
    "build/security_vm_release.py",
    "build/security_vm_stage.py",
    "build/security_vm_stage_guest.py",
    "build/security_vm_vps_guest.py",
    "build/test-release-container.py",
    "build/verify-release.py",
    "build/vps_capacity_controller.py",
    "build/vps_capacity_remote.py",
    "build/vps_ops_bootstrap.py",
    "ops/ansible/callback_plugins/release_timing.py",
    "ops/ansible/files/backup_offsite.py",
    "ops/ansible/files/backup_public.py",
    "ops/ansible/files/bounded_process.py",
    "ops/ansible/files/browser_assets.py",
    "ops/ansible/files/database_logging.py",
    "ops/ansible/files/fetch-release.py",
    "ops/ansible/files/force_release.py",
    "ops/ansible/files/migrate_public.py",
    "ops/ansible/files/migration_inspect.py",
    "ops/ansible/files/migration_snapshot.py",
    "ops/ansible/files/migration_tls.py",
    "ops/ansible/files/monitoring_alerts.py",
    "ops/ansible/files/monitoring_collect.py",
    "ops/ansible/files/monitoring_external.py",
    "ops/ansible/files/monitoring_prepare.py",
    "ops/ansible/files/monitoring_report.py",
    "ops/ansible/files/reboot-public.py",
    "ops/ansible/files/reboot_public.py",
    "ops/ansible/files/release-public.py",
    "ops/ansible/files/release_artifact.py",
    "ops/ansible/files/release_assets.py",
    "ops/ansible/files/release_fetch_receiver.py",
    "ops/ansible/files/release_json.py",
    "ops/ansible/files/release_preflight.py",
    "ops/ansible/files/release_public.py",
    "ops/ansible/files/release_trust.py",
    "ops/ansible/files/restore_verify.py",
    "ops/ansible/files/runtime_profile.py",
    "ops/ansible/files/seed-public-room.py",
    "ops/ansible/files/seed_public_room.py",
    "ops/ansible/files/turn_public.py",
    "ops/ansible/filter_plugins/sizing.py",
    "ops/ansible/tests/release_build_oracle.py",
    "ops/ansible/tests/release_deploy_oracle.py",
    "ops/ansible/tests/release_fetch_controller_oracle.py",
    "ops/ansible/tests/release_fresh_verify_oracle.py",
    "ops/ansible/tests/release_prepare_oracle.py",
    "ops/ansible/tests/release_receive_oracle.py",
    "ops/ansible/tests/release_stage_oracle.py",
    "ops/ansible/tests/release_verify_oracle.py",
    "ops/ansible/tests/test_automation.py",
    "ops/ansible/tests/test_backup_offsite.py",
    "ops/ansible/tests/test_bootstrap_controller.py",
    "ops/ansible/tests/test_bounded_process.py",
    "ops/ansible/tests/test_browser_assets_command.py",
    "ops/ansible/tests/test_caddy_materials.py",
    "ops/ansible/tests/test_caddy_probe_guest.py",
    "ops/ansible/tests/test_caddy_probe_vm.py",
    "ops/ansible/tests/test_capacity.py",
    "ops/ansible/tests/test_capacity_artifacts.py",
    "ops/ansible/tests/test_capacity_container.py",
    "ops/ansible/tests/test_capacity_ownership.py",
    "ops/ansible/tests/test_ci_cache_retention.py",
    "ops/ansible/tests/test_ci_database_check.py",
    "ops/ansible/tests/test_ci_local_act.py",
    "ops/ansible/tests/test_ci_local_evidence.py",
    "ops/ansible/tests/test_ci_release_checks.py",
    "ops/ansible/tests/test_ci_verified.py",
    "ops/ansible/tests/test_database_logging.py",
    "ops/ansible/tests/test_deploy_controller.py",
    "ops/ansible/tests/test_dns_handoff.py",
    "ops/ansible/tests/test_force_release.py",
    "ops/ansible/tests/test_host_playbooks.py",
    "ops/ansible/tests/test_issue_account_recovery.py",
    "ops/ansible/tests/test_maintain_namespace_probe.py",
    "ops/ansible/tests/test_migrate_public.py",
    "ops/ansible/tests/test_migration_abort.py",
    "ops/ansible/tests/test_migration_artifact.py",
    "ops/ansible/tests/test_migration_controller.py",
    "ops/ansible/tests/test_migration_dns.py",
    "ops/ansible/tests/test_migration_inspect.py",
    "ops/ansible/tests/test_migration_prepare.py",
    "ops/ansible/tests/test_migration_tls.py",
    "ops/ansible/tests/test_monitoring.py",
    "ops/ansible/tests/test_monitoring_database.py",
    "ops/ansible/tests/test_monitoring_external.py",
    "ops/ansible/tests/test_native_security.py",
    "ops/ansible/tests/test_ops_artifact_workflow.py",
    "ops/ansible/tests/test_ops_runtime_workflow.py",
    "ops/ansible/tests/test_performance_compare_ci.py",
    "ops/ansible/tests/test_public_backup.py",
    "ops/ansible/tests/test_public_deployment.py",
    "ops/ansible/tests/test_public_reboot.py",
    "ops/ansible/tests/test_public_release.py",
    "ops/ansible/tests/test_public_seed.py",
    "ops/ansible/tests/test_public_templates.py",
    "ops/ansible/tests/test_python_quality.py",
    "ops/ansible/tests/test_release_assets.py",
    "ops/ansible/tests/test_release_attestation.py",
    "ops/ansible/tests/test_release_build.py",
    "ops/ansible/tests/test_release_build_oracle.py",
    "ops/ansible/tests/test_release_container_ci.py",
    "ops/ansible/tests/test_release_container_fixture.py",
    "ops/ansible/tests/test_release_container_harness.py",
    "ops/ansible/tests/test_release_deploy_oracle.py",
    "ops/ansible/tests/test_release_fetch.py",
    "ops/ansible/tests/test_release_fetch_controller.py",
    "ops/ansible/tests/test_release_fetch_controller_oracle.py",
    "ops/ansible/tests/test_release_fetch_runtime_workflow.py",
    "ops/ansible/tests/test_release_fresh_verify_oracle.py",
    "ops/ansible/tests/test_release_http_policy.py",
    "ops/ansible/tests/test_release_json.py",
    "ops/ansible/tests/test_release_playbook.py",
    "ops/ansible/tests/test_release_preflight.py",
    "ops/ansible/tests/test_release_prepare_oracle.py",
    "ops/ansible/tests/test_release_receive_oracle.py",
    "ops/ansible/tests/test_release_stage_oracle.py",
    "ops/ansible/tests/test_release_timing.py",
    "ops/ansible/tests/test_release_verify_oracle.py",
    "ops/ansible/tests/test_release_workflow.py",
    "ops/ansible/tests/test_restore_database.py",
    "ops/ansible/tests/test_restore_verify.py",
    "ops/ansible/tests/test_runtime_components.py",
    "ops/ansible/tests/test_runtime_profile.py",
    "ops/ansible/tests/test_security_actions.py",
    "ops/ansible/tests/test_security_archive.py",
    "ops/ansible/tests/test_security_base_secrets.py",
    "ops/ansible/tests/test_security_check.py",
    "ops/ansible/tests/test_security_ci.py",
    "ops/ansible/tests/test_security_codeql_cache.py",
    "ops/ansible/tests/test_security_codeql_cargo.py",
    "ops/ansible/tests/test_security_codeql_health.py",
    "ops/ansible/tests/test_security_codeql_local.py",
    "ops/ansible/tests/test_security_codeql_resources.py",
    "ops/ansible/tests/test_security_codeql_rust_sources.py",
    "ops/ansible/tests/test_security_codeql_sources.py",
    "ops/ansible/tests/test_security_codeql_triage.py",
    "ops/ansible/tests/test_security_context.py",
    "ops/ansible/tests/test_security_dependency_licenses.py",
    "ops/ansible/tests/test_security_elf.py",
    "ops/ansible/tests/test_security_fedora_license.py",
    "ops/ansible/tests/test_security_findings.py",
    "ops/ansible/tests/test_security_image_advisories.py",
    "ops/ansible/tests/test_security_image_cache.py",
    "ops/ansible/tests/test_security_image_evidence.py",
    "ops/ansible/tests/test_security_image_finalization.py",
    "ops/ansible/tests/test_security_image_parallel.py",
    "ops/ansible/tests/test_security_image_policy.py",
    "ops/ansible/tests/test_security_image_prepare.py",
    "ops/ansible/tests/test_security_image_runner.py",
    "ops/ansible/tests/test_security_image_secrets.py",
    "ops/ansible/tests/test_security_license_identity.py",
    "ops/ansible/tests/test_security_migrations.py",
    "ops/ansible/tests/test_security_mutation.py",
    "ops/ansible/tests/test_security_native.py",
    "ops/ansible/tests/test_security_openssl.py",
    "ops/ansible/tests/test_security_policy.py",
    "ops/ansible/tests/test_security_rpm_license.py",
    "ops/ansible/tests/test_security_rpm_notices.py",
    "ops/ansible/tests/test_security_rpm_secrets.py",
    "ops/ansible/tests/test_security_rulesets.py",
    "ops/ansible/tests/test_security_runtime.py",
    "ops/ansible/tests/test_security_runtime_licenses.py",
    "ops/ansible/tests/test_security_secret_projection.py",
    "ops/ansible/tests/test_security_secret_spans.py",
    "ops/ansible/tests/test_security_secrets.py",
    "ops/ansible/tests/test_security_semgrep.py",
    "ops/ansible/tests/test_security_semgrep_summary.py",
    "ops/ansible/tests/test_security_tools.py",
    "ops/ansible/tests/test_security_vendor.py",
    "ops/ansible/tests/test_security_vm.py",
    "ops/ansible/tests/test_security_vm_boot.py",
    "ops/ansible/tests/test_security_vm_deploy.py",
    "ops/ansible/tests/test_security_vm_fetch.py",
    "ops/ansible/tests/test_security_vm_fetch_guest.py",
    "ops/ansible/tests/test_security_vm_fetch_hosts.py",
    "ops/ansible/tests/test_security_vm_guest.py",
    "ops/ansible/tests/test_security_vm_ops.py",
    "ops/ansible/tests/test_security_vm_ops_guest.py",
    "ops/ansible/tests/test_security_vm_qmp.py",
    "ops/ansible/tests/test_security_vm_receive.py",
    "ops/ansible/tests/test_security_vm_release.py",
    "ops/ansible/tests/test_security_vm_stage.py",
    "ops/ansible/tests/test_security_vm_vps_guest.py",
    "ops/ansible/tests/test_session_soak_ci.py",
    "ops/ansible/tests/test_support.py",
    "ops/ansible/tests/test_turn_public.py",
    "ops/ansible/tests/test_vps_capacity_controller.py",
    "ops/ansible/tests/test_vps_capacity_oracle.py",
    "ops/ansible/tests/test_vps_capacity_remote.py",
    "ops/ansible/tests/test_vps_ops_bootstrap.py",
    "ops/ansible/tests/vps_capacity_oracle.py",
    "vendor/mediasoup-sys-0.19.0/subprojects/packagefiles/libuv/link_file_in_build_dir.py",
    "vendor/mediasoup-sys-0.19.0/tasks.py",
    "xtask/src/ci/release_http_policy/__init__.py",
    "xtask/src/ci/release_http_policy/oracle.py",
    "xtask/src/ci/release_http_policy/reference/build/release_container_fixture.py",
    "xtask/src/ci/release_http_policy/reference/build/release_container_harness.py",
    "xtask/src/ci/release_http_policy/reference/build/release_http_policy.py",
    "xtask/src/ci/release_http_policy/reference/ops/ansible/files/bounded_process.py",
    "xtask/src/ci/release_http_policy/reference/ops/ansible/files/release_artifact.py",
    "xtask/src/ci/release_http_policy/reference/ops/ansible/files/release_assets.py",
    "xtask/src/ci/release_http_policy/reference/ops/ansible/files/release_json.py",
    "xtask/src/ci/release_http_policy/reference/ops/ansible/files/release_public.py",
    "xtask/src/ci/release_http_policy/reference/ops/ansible/files/runtime_profile.py",
    concat!(
        "xtask/src/ci/release_http_policy/reference/",
        "ops/ansible/tests/test_release_container_harness.py",
    ),
    "xtask/src/ci/release_http_policy/reference/ops/ansible/tests/test_release_http_policy.py",
    "xtask/src/ci/release_http_policy/reference/ops/ansible/tests/test_support.py",
];
const REFERENCES: &[&str] = &[
    "ops/ansible/files/restore_verify.py",
    "ops/ansible/files/restore-verify.sql",
    "ops/ansible/files/release_public.py",
    "ops/ansible/files/release_artifact.py",
    "ops/ansible/files/release_json.py",
    "ops/ansible/files/bounded_process.py",
    "ops/ansible/files/runtime_profile.py",
    "ops/ansible/tests/release_stage_oracle.py",
    "ops/ansible/tests/release_fetch_controller_oracle.py",
    "ops/ansible/tests/test_support.py",
];

fn source_pins(repository: &Path) -> io::Result<Value> {
    let mut found = serde_json::Map::new();
    for name in BASELINE_PYTHON {
        found.insert((*name).into(), pin(&repository.join(name))?);
    }
    Ok(Value::Object(found))
}
fn maintained_pins(repository: &Path) -> io::Result<Value> {
    fn visit(
        path: &Path,
        repository: &Path,
        found: &mut serde_json::Map<String, Value>,
    ) -> io::Result<()> {
        if path == repository.join("ops/ansible/.venv") {
            return Ok(());
        }
        if !path.exists() {
            return Ok(());
        }
        let metadata = fs::symlink_metadata(path)?;
        if metadata.is_dir() && !metadata.file_type().is_symlink() {
            for entry in fs::read_dir(path)? {
                visit(&entry?.path(), repository, found)?;
            }
        } else if path.extension().is_some_and(|ext| ext == "py") {
            let name = path
                .strip_prefix(repository)
                .map_err(io::Error::other)?
                .to_str()
                .ok_or_else(|| io::Error::other("maintained source UTF8"))?;
            if !matches!(
                name,
                "ops/ansible/tests/release_restore_oracle.py"
                    | "ops/ansible/tests/test_release_restore_oracle.py"
            ) {
                found.insert(name.into(), pin(path)?);
            }
        }
        Ok(())
    }
    let mut map = source_pins(repository)?.as_object().unwrap().clone();
    for name in [
        "build",
        "ops",
        "typings",
        "xtask/src/ci/release_http_policy",
    ] {
        visit(&repository.join(name), repository, &mut map)?;
    }
    Ok(Value::Object(map))
}
#[test]
fn maintained_census_excludes_controller_environment_and_retains_original_sources() {
    let fixture = Fixture::new();
    let maintained = ["ops/ansible/venv.py", "build/new-maintained.py"];
    let ignored = "ops/ansible/.venv/lib/provider.py";
    for name in BASELINE_PYTHON
        .iter()
        .copied()
        .chain(maintained)
        .chain([ignored])
    {
        let path = fixture.0.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, b"original\n").unwrap();
    }
    let before = maintained_pins(&fixture.0).unwrap();
    let expected = BASELINE_PYTHON
        .iter()
        .copied()
        .chain(maintained)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        before
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<std::collections::BTreeSet<_>>(),
        expected,
    );
    fs::write(fixture.0.join(ignored), b"installed dependency changed\n").unwrap();
    assert_eq!(maintained_pins(&fixture.0).unwrap(), before);
    fs::write(
        fixture.0.join(maintained[0]),
        b"maintained source changed\n",
    )
    .unwrap();
    assert_ne!(maintained_pins(&fixture.0).unwrap(), before);
    fs::remove_file(fixture.0.join(BASELINE_PYTHON[0])).unwrap();
    assert!(maintained_pins(&fixture.0).is_err());
}
fn public_refusal(scenario: &str) -> bool {
    if scenario == "root-not-root"
        && rustix::process::geteuid().as_raw() == 0
        && rustix::system::uname().sysname() == c"Linux"
    {
        return false;
    }
    matches!(scenario, "root-not-root" | "selector-invalid")
}
fn worker_entry(scenario: &str) {
    // This branch has no fixture/root override. The real public entry must
    // refuse before fixed host filesystem access on this supported test host.
    assert!(
        public_refusal(scenario),
        "unsafe public guard worker context"
    );
    let attempt = if scenario == "selector-invalid" {
        "../not-owned"
    } else {
        SOURCE_ATTEMPT
    };
    let status = super::run(&[
        "--release-attempt".into(),
        attempt.into(),
        "--timeout-seconds".into(),
        "1800".into(),
        "--finalization-seconds".into(),
        "60".into(),
    ]);
    assert_eq!(status, std::process::ExitCode::FAILURE);
}
fn arguments_for_worker() -> Vec<OsString> {
    vec![
        EXACT.into(),
        "--ignored".into(),
        "--exact".into(),
        "--nocapture".into(),
    ]
}
async fn guard_worker(owner: &mut Owner, scenario: &str) -> io::Result<child::Completion> {
    if !public_refusal(scenario) {
        return Err(io::Error::other(
            "unsupported fixed-host public guard context",
        ));
    }
    let body_end = (owner.now() + Duration::from_secs(30)).min(owner.body_end() - PYTHON_TAIL);
    let current = std::env::current_exe()?;
    Ok(child::run(
        owner,
        child::Invocation {
            program: current,
            arguments: arguments_for_worker(),
            environment: BTreeMap::from([
                (WORKER.into(), scenario.into()),
                ("PATH".into(), "/usr/bin:/bin".into()),
                ("LC_ALL".into(), "C".into()),
            ]),
            body_end,
            settlement_end: body_end + PYTHON_TAIL,
            cleanup: false,
        },
    )
    .await)
}
fn native_case(
    reference: &Oracle,
    root: &Path,
    owner: &mut Owner,
    started: Instant,
) -> io::Result<Case> {
    let before = inventory(&root.join("public/results").join(SOURCE_ATTEMPT), root)?;
    let identities_before = original_identities(root)?;
    let initial_files = product_tree(root)?;
    let mut actor = Actor {
        scenario: reference.scenario.clone(),
        fault_observed: false,
        delivered: Vec::new(),
        readonly_inputs: true,
        locks_held: true,
        all_settled: true,
        snapshots: Vec::new(),
        root: root.into(),
        ready_started: None,
        ready_offsets: Vec::new(),
    };
    let mut public_worker = None;
    let mut codes = Vec::new();
    let mut failures = Vec::new();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut locked_during_output = true;
    let mut output_closed = true;
    let mut fault_hits = 0;
    let mut snapshot_unlinks = 0;
    let options = Options {
        attempt: reference.input["attempt"]
            .as_str()
            .ok_or_else(|| io::Error::other("oracle attempt"))?
            .into(),
        whole: WHOLE,
        reserve: RESERVE,
    };
    super::super::files::start_probe(None);
    records::start_probe();
    // Carry all actual producer/setup time once; subsequent readiness sleeps
    // advance this original clock without rewinding or replacing the owner.
    owner.elapsed = Some(started.elapsed());
    let held = if reference.scenario == "lock-busy" {
        let file = File::options()
            .read(true)
            .write(true)
            .open(root.join("work/workload.lock"))?;
        if let Err(error) = rustix::fs::flock(
            file.as_fd(),
            rustix::fs::FlockOperation::NonBlockingLockExclusive,
        ) {
            let _ = nix::unistd::close(file);
            return Err(error.into());
        }
        Some(file)
    } else {
        None
    };
    let repeat = reference.input["repeat"]
        .as_u64()
        .ok_or_else(|| io::Error::other("oracle repeat"))?;
    let result: io::Result<()> = (|| {
        for index in 0..repeat {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| {
                    owner.record(Cause::Io);
                    io::Error::other(error)
                })?;
            if matches!(
                reference.scenario.as_str(),
                "root-not-root" | "selector-invalid"
            ) {
                let completed = runtime.block_on(guard_worker(owner, &reference.scenario));
                drop(runtime);
                public_worker = Some(completed?);
                codes.push(1);
                failures.push(None);
                stderr.extend_from_slice(FAILURE);
                break;
            }
            let mut nonce = [0xdd; 16];
            nonce[15] = 0xdd + u8::try_from(index).map_err(io::Error::other)?;
            let access = Access {
                root: root.join("public"),
                config: root.join("config"),
                work: root.join("work"),
                state: root.join("state"),
                library: root.join("library"),
                docker: root.join("bin/docker"),
                socket: None,
                uid: fs::metadata(root)?.uid(),
                nonce: Some(nonce),
                timestamp: reference.input["timestamp"].as_str().map(str::to_owned),
                virtual_poll: true,
                fault: fault(&reference.scenario),
            };
            let outcome =
                runtime.block_on(execution::execute(&options, &access, owner, &mut actor));
            drop(runtime);
            let passed = outcome.passed && outcome.failure.is_none();
            fault_hits += outcome.fault_hits;
            snapshot_unlinks += outcome.snapshot_unlinks;
            failures.push(outcome.failure.clone());
            locked_during_output &=
                lock_available(&root.join("work/workload.lock")).is_ok_and(|available| !available);
            let bytes = if passed { SUCCESS } else { FAILURE };
            let captured = output_sink(&root.join(format!("observer-output-{index}")), bytes);
            if captured.is_err() {
                owner.record(Cause::Io);
                output_closed = false;
            }
            // Actual lock/original guard lives through runtime drop and output.
            outcome.finish(owner);
            let captured = captured?;
            if passed {
                stdout.extend(captured);
            } else {
                stderr.extend(captured);
            }
            codes.push(i32::from(!passed || owner.error().is_some()));
        }
        Ok(())
    })();
    let held_closed = held
        .map(|file| nix::unistd::close(file).map_err(io::Error::from))
        .transpose();
    let shared = super::super::files::take_probe();
    let closed = records::take_probe();
    result?;
    held_closed?;
    let after = inventory(&root.join("public/results").join(SOURCE_ATTEMPT), root)?;
    let identities_after = original_identities(root)?;
    let attempts = attempt_paths(root)?;
    let daemon = normalize(decoded(&root.join("daemon.json"))?, root, &attempts);
    let snapshots = actor
        .snapshots
        .into_iter()
        .map(|value| normalize(value, root, &attempts))
        .collect();
    Ok(Case {
        codes,
        stdout,
        stderr,
        primary: owner.error(),
        failures,
        before,
        after,
        identities_before,
        identities_after,
        initial_files,
        files: product_tree(root)?,
        modes: directory_modes(root)?,
        daemon,
        delivered: actor.delivered,
        snapshots,
        shared,
        records: closed,
        locked_during_output,
        lock_released: lock_available(&root.join("work/workload.lock"))?,
        readonly_inputs: actor.readonly_inputs,
        all_settled: actor.all_settled,
        fault_hits,
        snapshot_unlinks,
        fault_observed: actor.fault_observed,
        public_worker,
        output_closed,
        ready_offsets: actor.ready_offsets,
        ready_elapsed: actor
            .ready_started
            .map(|first| owner.now().duration_since(first)),
    })
}
fn settled(completed: &child::Completion, stderr: &[u8]) {
    let proof = &completed.observation;
    assert!(
        completed.result.is_ok(),
        concat!(
            "fixed producer settlement: result={:?} pid={:?} status={:?} ",
            "group={} reap={} close={:?} eof={:?} bodyEof={:?} ",
            "timedOut={} roleTimedOut={} cleanup={:?}",
        ),
        completed.result,
        proof.pid,
        proof.exit_status,
        proof.group_retired,
        proof.reaped,
        (proof.stdin_closed, proof.stdout_closed, proof.stderr_closed),
        (proof.stdout_eof, proof.stderr_eof),
        (proof.stdout_body_eof, proof.stderr_body_eof),
        proof.timed_out,
        proof.role_timed_out,
        proof.cleanup_errors,
    );
    assert_eq!(proof.exit_status, Some(0));
    assert!(proof.reaped && proof.group_retired);
    assert!(proof.stdin_closed && proof.stdout_closed && proof.stderr_closed);
    assert!(proof.stdout_eof && proof.stderr_eof);
    assert!(proof.stdout_body_eof && proof.stderr_body_eof);
    assert!(proof.cleanup_errors.is_empty());
    assert_eq!(completed.stderr, stderr);
    assert_eq!(proof.stdout_bytes, completed.stdout.len() as u64);
    assert_eq!(proof.stderr_bytes, completed.stderr.len() as u64);
    assert_eq!(proof.stdout_sha256, hash(&completed.stdout));
    assert_eq!(proof.stderr_sha256, hash(&completed.stderr));
}
fn cid_profile(name: &str, original: &Value, native: &Value) -> bool {
    name == "create-timeout-after-effect"
        && original["containerId"] == ""
        && native["containerId"] == CID
        && original["verified"] == false
        && native["verified"] == false
        && original["cleanupPassed"] == true
        && native["cleanupPassed"] == true
}
fn project_cid(name: &str, original: &Value, native: &mut Value) -> bool {
    if !cid_profile(name, original, native) {
        return false;
    }
    native["containerId"] = original["containerId"].clone();
    true
}

fn assert_original(reference: &Oracle, repository: &Path, baseline: &Value) {
    let observed = &reference.observation;
    assert_eq!(reference.schema_version, 1);
    assert_eq!(observed["sourceUnchanged"], true);
    assert_eq!(observed["originalPythonUnchanged"], true);
    assert_eq!(observed["originalBaselinePythonUnchanged"], true);
    assert_eq!(observed["originalBaselinePythonPins"], *baseline);
    assert_eq!(
        observed["originalBaselinePythonCount"],
        BASELINE_PYTHON.len()
    );
    assert!(observed["originalPythonCount"].as_u64().unwrap() >= BASELINE_PYTHON.len() as u64);
    for path in REFERENCES {
        assert_eq!(
            observed["sourcePins"][*path],
            pin(&repository.join(path)).unwrap()
        );
    }
    assert_eq!(
        observed["workerSettled"],
        json!({"status":0,"reaped":true,
        "pipesClosed":true,"stderrBytes":0})
    );
    let children = observed["children"].as_array().unwrap();
    let calls = observed["daemon"]["calls"].as_array().unwrap();
    assert_eq!(children.len(), calls.len());
    for (peer, call) in children.iter().zip(calls) {
        assert_eq!(peer["status"], call["status"]);
        assert_eq!(peer["pipesClosed"], true);
        assert_eq!(peer["logsClosed"], true);
    }
    assert_eq!(observed["snapshotOriginalsClosed"], true);
    assert_eq!(observed["lockReleased"], true);
    assert_eq!(observed["inputUnchanged"], true);
    assert_eq!(observed["inputPinsBefore"], observed["inputPinsAfter"]);
    for write in observed["writes"].as_array().unwrap() {
        assert_eq!(write["lockHeld"], true);
    }
}
fn raw_create_proof(reference: &Oracle, native: &Case) {
    assert_eq!(reference.scenario, "create-timeout-after-effect");
    assert_eq!(reference.observation["faultObserved"], true);
    assert_eq!(
        reference.observation["faultEvents"],
        json!(["create-timeout-after-effect"])
    );
    assert_eq!(reference.observation["raised"][0]["type"], "TimeoutExpired");
    assert_eq!(native.primary, Some(Cause::Deadline));
    assert!(native.fault_observed);
    let original = reference.observation["daemon"]["calls"]
        .as_array()
        .unwrap()
        .iter()
        .find(|call| call["role"] == "create")
        .unwrap();
    assert_eq!(original["status"], 0);
    assert_eq!(
        original["stdoutHex"],
        super::super::files::hex(format!("{CID}\n").as_bytes())
    );
    assert_eq!(original["inputBytes"], 0);
    assert_eq!(original["inputSha256"], hash(b""));
    let create = native
        .delivered
        .iter()
        .find(|call| call["role"] == "Create")
        .unwrap();
    assert_eq!(create["status"], 0);
    assert_eq!(create["settled"], true);
    assert!(create["error"].is_null());
    assert_eq!(create["stdoutComplete"], true);
    assert_eq!(create["stdoutBytes"], 65);
    assert_eq!(
        create["originalSink"],
        json!({"bytes":65,
        "sha256":hash(format!("{CID}\n").as_bytes()),"matchesCapture":true})
    );
    assert_eq!(create["stderrBytes"], 0);
    assert_eq!(create["stdoutHex"], original["stdoutHex"]);
    assert_eq!(create["stdoutSha256"], hash(format!("{CID}\n").as_bytes()));
    assert!(native.codes.iter().all(|code| *code == 1));
}
fn same_files(reference: &Oracle, native: &Case) {
    let original = semantic_tree(reference.observation["files"].clone());
    let mut actual = semantic_tree(native.files.clone());
    if reference.scenario == "create-timeout-after-effect" {
        raw_create_proof(reference, native);
        let name = "state/restores/$RESTORE-001/restore.json";
        let original_report = &original[name]["json"];
        let projected = project_cid(
            &reference.scenario,
            original_report,
            &mut actual[name]["json"],
        );
        assert!(
            projected,
            "only exact named known-create observation can project"
        );
    }
    assert_eq!(
        actual, original,
        "{}: complete retained product tree",
        reference.scenario
    );
}
fn calls_for_profile(reference: &Oracle) -> Vec<Value> {
    let all = reference.observation["daemon"]["calls"].as_array().unwrap();
    let mut ready = 0;
    all.iter()
        .filter(|call| match call["role"].as_str().unwrap() {
            "ready" => {
                ready += 1;
                ready <= 40
            }
            "roles" | "restore" | "restored-ledger" | "verify" | "amcheck"
                if reference.scenario.starts_with("ready-") =>
            {
                false
            }
            "verify" | "amcheck" if reference.scenario == "verify-sql-private0600" => false,
            _ => true,
        })
        .cloned()
        .collect()
}
fn profile_files(reference: &Oracle, native: &Case, expected_calls: &[Value]) {
    // These two named native profiles refuse before later original-positive
    // work. Construct the exact retained prefix, dual logs and final record;
    // all original/raw native files remain intact outside comparison clones.
    let mut expected = reference.observation["files"].as_object().unwrap().clone();
    let prefix = "state/restores/$RESTORE-001/";
    expected.retain(|name, _| !name.starts_with(prefix));
    for (index, call) in expected_calls.iter().enumerate() {
        for (extension, key) in [("stdout", "stdoutHex"), ("stderr", "stderrHex")] {
            let data = unhex(call[key].as_str().unwrap());
            expected.insert(
                format!("{prefix}{:03}.{extension}", index + 1),
                json!({"bytes":data.len(),"sha256":hash(&data),"mode":0o600,"links":1}),
            );
        }
    }
    let writes = reference.observation["writes"].as_array().unwrap();
    let mut report = writes
        .iter()
        .find(|event| {
            event["path"]
                .as_str()
                .is_some_and(|path| path.ends_with("restore.json"))
        })
        .unwrap()["data"]
        .clone();
    report["containerId"] = json!(CID);
    report["cleanupPassed"] = json!(true);
    report["verified"] = json!(false);
    expected.insert(
        format!("{prefix}restore.json"),
        json!({"json":report,"mode":0o600,"links":1}),
    );
    // Original prior freshness bytes are fixed exported input, not an inferred
    // empty tree or a newly fabricated successful receipt.
    let old = b"UNCHANGED_PREVIOUS_FRESHNESS\n";
    expected.insert(
        "state/restore-verified.timestamp".into(),
        native.initial_files["state/restore-verified.timestamp"].clone(),
    );
    assert_eq!(
        native.initial_files["state/restore-verified.timestamp"],
        json!({"bytes":old.len(),"sha256":hash(old),"mode":0o600,"links":1})
    );
    assert_eq!(
        semantic_tree(native.files.clone()),
        semantic_tree(Value::Object(expected)),
        "{}: exact named refusal prefix",
        reference.scenario
    );
}
fn unhex(text: &str) -> Vec<u8> {
    assert_eq!(text.len() % 2, 0);
    text.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            let digit = |byte: u8| match byte {
                b'0'..=b'9' => byte - b'0',
                b'a'..=b'f' => byte - b'a' + 10,
                _ => panic!("invalid closed oracle hex"),
            };
            digit(pair[0]) * 16 + digit(pair[1])
        })
        .collect()
}
/// Exact three-row counter-only source-order qualification, never an operation rewrite.
#[derive(Clone, Copy)]
struct SnapshotOrder<'a> {
    scenario: &'a str,
    source_pin: &'a Value,
    original_source_pin: &'a Value,
    native_codes: &'a [i32],
    original_codes: &'a Value,
    native_calls: usize,
    original_calls: usize,
    primary: Option<Cause>,
    native_unlinks: usize,
    original_unlinks: u64,
    snapshots_absent: bool,
}
impl SnapshotOrder<'_> {
    fn source_rejected_before_snapshot(self) -> bool {
        let expected = match self.scenario {
            "backup-oversized"
                if *self.source_pin
                    == json!({
                        "bytes":64 * 1024 * 1024 + 1,
                        "sha256":"c03ed13a660c013f906110c2ce0ce6eee2aa1c164655b3a5a7f1e65f6bfbc50c",
                        "mode":0o600,"links":1,
                    }) =>
            {
                Cause::Policy
            }
            "backup-mode"
                if *self.source_pin
                    == json!({
                        "bytes":4128,
                        "sha256":"d0ab5a785d5a0df0dddd74e986fb598af77cd2d8dad11a9d1271edb7f3528027",
                        "mode":0o755,"links":1,
                    }) =>
            {
                Cause::Policy
            }
            "backup-symlink"
                if self
                    .source_pin
                    .as_object()
                    .is_some_and(|pin| pin.len() == 3)
                    && self.source_pin["link"] == "database-before.dump.original"
                    && self.source_pin["links"] == 1
                    && self.source_pin["mode"]
                        .as_u64()
                        .is_some_and(|mode| mode <= 0o7777) =>
            {
                Cause::Io
            }
            _ => return false,
        };
        // Full original metadata, including the platform's actual symlink mode,
        // stays exact. No guessed symlink permission value enters this projection.
        self.source_pin == self.original_source_pin
            && self.native_codes == [1]
            && *self.original_codes == json!([1])
            && self.native_calls == 0
            && self.original_calls == 0
            && self.primary == Some(expected)
            && self.native_unlinks == 0
            && self.original_unlinks == 1
            && self.snapshots_absent
    }
}
#[test]
fn snapshot_counter_table_requires_each_exact_named_input_and_negative_prefix() {
    let original_codes = json!([1]);
    let rows = [
        (
            "backup-oversized",
            json!({"bytes":64*1024*1024+1,
            "sha256":"c03ed13a660c013f906110c2ce0ce6eee2aa1c164655b3a5a7f1e65f6bfbc50c",
            "mode":0o600,"links":1}),
            Cause::Policy,
        ),
        (
            "backup-mode",
            json!({"bytes":4128,
            "sha256":"d0ab5a785d5a0df0dddd74e986fb598af77cd2d8dad11a9d1271edb7f3528027",
            "mode":0o755,"links":1}),
            Cause::Policy,
        ),
        (
            "backup-symlink",
            json!({"link":"database-before.dump.original",
            "mode":0o755,"links":1}),
            Cause::Io,
        ),
    ];
    for (scenario, pin, primary) in rows {
        let fixed = SnapshotOrder {
            scenario,
            source_pin: &pin,
            original_source_pin: &pin,
            native_codes: &[1],
            original_codes: &original_codes,
            native_calls: 0,
            original_calls: 0,
            primary: Some(primary),
            native_unlinks: 0,
            original_unlinks: 1,
            snapshots_absent: true,
        };
        assert!(fixed.source_rejected_before_snapshot());
        let mut metadata = pin.clone();
        metadata["mode"] = json!(0o711);
        let mut different_source = pin.clone();
        if scenario == "backup-symlink" {
            different_source["link"] = json!("foreign.dump");
        } else {
            different_source["bytes"] = json!(0);
        }
        for changed in [
            SnapshotOrder {
                scenario: "backup-empty",
                ..fixed
            },
            SnapshotOrder {
                source_pin: &different_source,
                ..fixed
            },
            SnapshotOrder {
                source_pin: &metadata,
                ..fixed
            },
            SnapshotOrder {
                native_codes: &[0],
                ..fixed
            },
            SnapshotOrder {
                original_codes: &Value::Null,
                ..fixed
            },
            SnapshotOrder {
                native_calls: 1,
                ..fixed
            },
            SnapshotOrder {
                original_calls: 1,
                ..fixed
            },
            SnapshotOrder {
                primary: Some(Cause::Changed),
                ..fixed
            },
            SnapshotOrder {
                native_unlinks: 1,
                ..fixed
            },
            SnapshotOrder {
                original_unlinks: 0,
                ..fixed
            },
            SnapshotOrder {
                snapshots_absent: false,
                ..fixed
            },
        ] {
            assert!(!changed.source_rejected_before_snapshot());
        }
    }
}

fn compare(reference: &Oracle, native: &Case, finished: bool) {
    let name = &reference.scenario;
    let observed = &reference.observation;
    assert_eq!(
        native.before, native.after,
        "{name}: source bytes/modes/links unchanged"
    );
    assert_eq!(
        native.identities_before, native.identities_after,
        "{name}: original source inode and full metadata unchanged"
    );
    assert_eq!(native.before, observed["inputPinsBefore"]);
    assert!(native.lock_released && native.output_closed);
    assert!(native.readonly_inputs && native.all_settled);
    assert_eq!(
        native.shared.1, 0,
        "{name}: no fallback directory/original close"
    );
    assert_eq!(native.records.1, 0, "{name}: no fallback record close");
    if !native.delivered.is_empty() {
        assert!(native.locked_during_output);
    }
    if let Some(worker) = &native.public_worker {
        settled(worker, FAILURE);
        let trailer = String::from_utf8_lossy(&worker.stdout);
        assert!(trailer.contains("1 passed; 0 failed"));
        assert!(native.delivered.is_empty());
        assert!(finished);
    }
    let profile = matches!(
        name.as_str(),
        "ready-deadline" | "ready-exact-boundary" | "verify-sql-private0600"
    );
    if profile {
        assert_eq!(native.codes, vec![1]);
        assert!(native.stdout.is_empty());
        assert_eq!(native.stderr, FAILURE);
        assert!(!finished);
        if name.starts_with("ready-") {
            assert_eq!(
                native.failures,
                vec![Some("native_deadline_refused".into())]
            );
            assert_eq!(native.primary, Some(Cause::Deadline));
            assert_eq!(observed["daemon"]["ready"], 61);
            assert_eq!(
                observed["daemon"]["calls"].as_array().unwrap().len(),
                if name == "ready-deadline" { 69 } else { 74 }
            );
            assert_eq!(native.daemon["calls"].as_array().unwrap().len(), 48);
            assert_eq!(native.daemon["ready"], 40);
            assert_eq!(native.ready_elapsed, Some(Duration::from_secs(20)));
            assert_eq!(
                native.ready_offsets,
                (0..40)
                    .map(|index| Duration::from_millis(index * 500))
                    .collect::<Vec<_>>()
            );
        } else {
            assert_eq!(reference.input["nativeProfile"], "fixed-verifier-sql644");
            assert_eq!(observed["exitCodes"], json!([0]));
            assert_eq!(native.primary, Some(Cause::Policy));
            assert_eq!(native.failures, vec![Some("native_restore_refused".into())]);
        }
        let expected = calls_for_profile(reference);
        assert_eq!(native.daemon["calls"], json!(expected));
        profile_files(reference, native, &expected);
    } else {
        assert_eq!(
            native.codes,
            serde_json::from_value::<Vec<i32>>(observed["exitCodes"].clone()).unwrap(),
            "{name}: actual operation statuses"
        );
        assert_eq!(
            native.stdout,
            observed["stdout"].as_str().unwrap().as_bytes(),
            "{name}"
        );
        assert_eq!(
            native.stderr,
            observed["stderr"].as_str().unwrap().as_bytes(),
            "{name}"
        );
        assert_eq!(
            native.daemon, observed["daemon"],
            "{name}: all actual lower effects"
        );
        same_files(reference, native);
    }
    assert_eq!(
        native.modes, observed["directoryModes"],
        "{name}: all directory modes"
    );
    let calls = native.daemon["calls"].as_array().unwrap();
    assert_eq!(native.delivered.len(), calls.len());
    for (delivered, call) in native.delivered.iter().zip(calls) {
        assert_eq!(delivered["status"], call["status"]);
        assert_eq!(delivered["stdoutComplete"], true);
        assert_eq!(delivered["settled"], true);
        assert_eq!(delivered["originalSink"]["matchesCapture"], true);
        assert_eq!(delivered["originalSink"]["bytes"], delivered["stdoutBytes"]);
        assert_eq!(
            delivered["originalSink"]["sha256"],
            delivered["stdoutSha256"]
        );
        assert_eq!(delivered["stdoutHex"], call["stdoutHex"]);
        assert_eq!(
            delivered["stderrBytes"],
            unhex(call["stderrHex"].as_str().unwrap()).len()
        );
    }
    let original_unlinks = observed["snapshotUnlinkAttempts"].as_u64().unwrap();
    let source = format!("public/results/{SOURCE_ATTEMPT}/database-before.dump");
    let snapshots_absent = [&native.files, &observed["files"]].iter().all(|files| {
        files
            .as_object()
            .unwrap()
            .keys()
            .all(|path| !path.ends_with("/snapshot.dump"))
    });
    let ordering = SnapshotOrder {
        scenario: name,
        source_pin: &native.before[source.as_str()],
        original_source_pin: &observed["inputPinsBefore"][source.as_str()],
        native_codes: &native.codes,
        original_codes: &observed["exitCodes"],
        native_calls: calls.len(),
        original_calls: observed["daemon"]["calls"].as_array().unwrap().len(),
        primary: native.primary,
        native_unlinks: native.snapshot_unlinks,
        original_unlinks,
        snapshots_absent,
    };
    if ordering.source_rejected_before_snapshot() {
        // Native refuses only these exact source admissions before snapshot creation.
        // Python finally attempts unlink even if its target was never opened.
        // All raw counters and the already-exact full trees/effects remain intact.
        assert_eq!(native.snapshot_unlinks, 0);
        assert_eq!(original_unlinks, 1);
    } else {
        assert_eq!(
            native.snapshot_unlinks as u64, original_unlinks,
            "{name}: each snapshot unlink attempted exactly once"
        );
    }
    assert_eq!(
        native.fault_hits,
        usize::from(fault(name).is_some()),
        "{name}: each lower fault fires once, with no finalizer retry"
    );
    assert_eq!(native.fault_observed, name == "create-timeout-after-effect");
    assert_eq!(
        observed["faultObserved"],
        native.fault_hits > 0 || native.fault_observed
    );
    if name == "restore-command-cleanup-remove" {
        assert_eq!(native.primary, Some(Cause::Command));
        assert_eq!(
            native.failures,
            vec![Some("Command 007 failed; inspect private output".into())]
        );
        assert_eq!(observed["raised"][0]["type"], "ReleaseError");
        assert_eq!(
            observed["raised"][0]["message"],
            "Command 010 failed; inspect private output"
        );
    }
    if native.codes.iter().all(|code| *code == 0) {
        assert!(finished && native.primary.is_none());
        assert!(native.failures.iter().all(Option::is_none));
    }
    // Complete initial/created records must already be visible to the actual
    // daemon role, not materialized later by the fixture or final observer.
    for snapshot in &native.snapshots {
        if snapshot["role"] == "Inspect" {
            assert!(snapshot["record"].is_null());
        } else {
            assert!(snapshot["record"].is_object());
        }
    }
}

fn exclusive_oracle(arguments: &[OsString]) -> bool {
    let mut exact = false;
    let mut ignored = false;
    let mut filter = None;
    let mut at = 0;
    while at < arguments.len() {
        let Some(word) = arguments[at].to_str() else {
            return false;
        };
        match word {
            "--exact" => exact = true,
            "--ignored" => ignored = true,
            "--test-threads" | "--color" | "--format" | "--logfile" | "--shuffle-seed" => {
                at += 1;
                if arguments.get(at).is_none() {
                    return false;
                }
            }
            "--nocapture"
            | "--show-output"
            | "--quiet"
            | "-q"
            | "--report-time"
            | "--ensure-time"
            | "--exclude-should-panic"
            | "--force-run-in-process"
            | "--shuffle" => {}
            value
                if [
                    "--test-threads=",
                    "--color=",
                    "--format=",
                    "--logfile=",
                    "--shuffle-seed=",
                ]
                .iter()
                .any(|prefix| value.starts_with(prefix)) => {}
            value if value.starts_with('-') => return false,
            value => {
                if filter.replace(value).is_some() {
                    return false;
                }
            }
        }
        at += 1;
    }
    exact && ignored && filter == Some(EXACT)
}
struct OracleMask {
    previous: nix::sys::stat::Mode,
    active: bool,
}
impl OracleMask {
    fn new() -> Self {
        Self {
            previous: nix::sys::stat::umask(nix::sys::stat::Mode::from_bits_truncate(0o077)),
            active: true,
        }
    }
    fn restore(mut self) -> (nix::sys::stat::Mode, nix::sys::stat::Mode) {
        let active = nix::sys::stat::umask(self.previous);
        let restored = nix::sys::stat::umask(self.previous);
        self.active = false;
        (active, restored)
    }
}
impl Drop for OracleMask {
    fn drop(&mut self) {
        if self.active {
            // Panic fallback restores global state but is never positive proof.
            nix::sys::stat::umask(self.previous);
        }
    }
}

#[test]
fn exact_sole_process_admission_precedes_any_mask_or_owner() {
    let args = |values: &[&str]| values.iter().map(OsString::from).collect::<Vec<_>>();
    assert!(exclusive_oracle(&args(&[EXACT, "--ignored", "--exact"])));
    assert!(exclusive_oracle(&args(&[
        "--test-threads",
        "1",
        EXACT,
        "--ignored",
        "--exact",
        "--format=pretty",
        "--nocapture"
    ])));
    for values in [
        vec![EXACT, "--ignored"],
        vec!["--ignored", "--exact"],
        vec![EXACT, "--ignored", "--exact", "other_test"],
        vec!["short_filter", "--ignored", "--exact"],
    ] {
        assert!(!exclusive_oracle(&args(&values)));
    }
}
#[test]
fn create_effect_projection_is_closed_to_one_failed_joined_report_pair() {
    let original = json!({"containerId":"","verified":false,"cleanupPassed":true});
    let native = json!({"containerId":CID,"verified":false,"cleanupPassed":true});
    let mut projected = native.clone();
    assert!(project_cid(
        "create-timeout-after-effect",
        &original,
        &mut projected
    ));
    assert_eq!(projected, original);
    for (name, left, right) in [
        ("create-command", original.clone(), native.clone()),
        (
            "create-timeout-after-effect",
            json!({"containerId":"foreign","verified":false,
            "cleanupPassed":true}),
            native.clone(),
        ),
        (
            "create-timeout-after-effect",
            original.clone(),
            json!({"containerId":"unknown",
            "verified":false,"cleanupPassed":true}),
        ),
        (
            "create-timeout-after-effect",
            original,
            json!({"containerId":CID,"verified":true,
            "cleanupPassed":true}),
        ),
    ] {
        let mut value = right.clone();
        assert!(!project_cid(name, &left, &mut value));
        assert_eq!(value, right);
    }
}
#[test]
#[ignore = "requires pinned controller Python; operations automation owns exact isolated execution"]
fn whole_restore_matches_unchanged_python() {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    assert!(
        exclusive_oracle(&args),
        "requires sole exact ignored oracle process before admission"
    );
    if let Some(role) = std::env::var_os(WORKER) {
        worker_entry(role.to_str().expect("fixed public worker role"));
        return;
    }
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let prefix = std::env::var_os("PYTHON_CHECK_ENV")
        .map(PathBuf::from)
        .unwrap_or_else(|| repository.join("ops/ansible/.venv"));
    let sources_before = source_pins(repository);
    // Admission failure has no owned runtime/operation state at this boundary.
    assert!(
        sources_before.is_ok(),
        "complete explicit original Python baseline exists"
    );
    let baseline = sources_before.unwrap();
    let maintained_before = maintained_pins(repository).unwrap();
    for scenario in SCENARIOS {
        eprintln!("whole standalone restore case: {scenario}");
        let started = Instant::now();
        let mut owner = Owner::new(started, WHOLE, RESERVE).unwrap();
        let mask = OracleMask::new();
        let previous = mask.previous;
        let fixture = Fixture::new();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build();
        let completed = match runtime {
            Ok(runtime) => {
                let python = runtime.block_on(child::run(
                    &mut owner,
                    child::Invocation {
                        program: prefix.join("bin/python3"),
                        arguments: vec![
                            "-B".into(),
                            repository
                                .join("ops/ansible/tests/release_restore_oracle.py")
                                .into_os_string(),
                            "--scenario".into(),
                            (*scenario).into(),
                            "--fixture-directory".into(),
                            fixture.0.as_os_str().into(),
                        ],
                        environment: BTreeMap::from([
                            ("PATH".into(), prefix.join("bin").into_os_string()),
                            ("LC_ALL".into(), "C".into()),
                            ("PYTHONDONTWRITEBYTECODE".into(), "1".into()),
                        ]),
                        body_end: started + PYTHON_BODY,
                        settlement_end: started + PYTHON_BODY + PYTHON_TAIL,
                        cleanup: false,
                    },
                ));
                drop(runtime);
                let reference = serde_json::from_slice::<Oracle>(&python.stdout);
                let native = if python.result.is_ok() {
                    match &reference {
                        Ok(reference) => {
                            native_case(reference, &fixture.0.join("native"), &mut owner, started)
                        }
                        Err(_) => Err(io::Error::other(
                            "malformed whole original restore observation",
                        )),
                    }
                } else {
                    Err(io::Error::other("whole original restore worker refused"))
                };
                (python, reference, native)
            }
            Err(_) => {
                owner.record(Cause::Io);
                let _ = mask.restore();
                let _ = owner.finish();
                drop(fixture);
                panic!("owned original restore runtime admission refused");
            }
        };
        // All native runtimes, output originals and Outcome guards have already
        // completed. Restore the real mask before the sole original end fence.
        let (active, restored) = mask.restore();
        // The real end remains authoritative even when virtual readiness has
        // advanced farther than physical time. This only moves forward.
        let actual = Instant::now();
        if actual > owner.now() {
            owner.observe_at(actual);
        }
        let finished = owner.finish();
        let (python, reference, native) = completed;
        assert_eq!(active, nix::sys::stat::Mode::from_bits_truncate(0o077));
        assert_eq!(restored, previous);
        settled(&python, b"");
        let reference = reference.unwrap();
        let native = native.unwrap();
        assert_eq!(reference.scenario, *scenario);
        assert_original(&reference, repository, &baseline);
        assert_eq!(
            reference.observation["originalPythonCount"],
            maintained_before.as_object().unwrap().len()
        );
        compare(&reference, &native, finished.is_ok());
    }
    assert_eq!(
        source_pins(repository).unwrap(),
        baseline,
        "all named original Python unchanged"
    );
    assert_eq!(
        maintained_pins(repository).unwrap(),
        maintained_before,
        "complete dynamic maintained Python census unchanged"
    );
}
